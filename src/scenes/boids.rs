use std::f32::consts::TAU;

use super::{Scene, SceneCtx, SceneView};
use crate::engine::{camera::CameraUniform, camera::Orbit, gui, vertices::Vertex};
use glam::{Quat, Vec3};
use rand::RngExt;
use wgpu::{BufferDescriptor, Device};

// Starting view distance and weights for the three rules, which are then set
// from the menu
const VIEW_DISTANCE: f32 = 0.5;
const SEPARATION: f32 = 0.2;
const ALIGNMENT: f32 = 5.0;
const COHESION: f32 = 5.0;
const MIN_SPEED: f32 = 1.0;
const MAX_SPEED: f32 = 5.0;
const BOUNDARY_RADIUS: f32 = 1.5;
const BOUNDARY_STRENGTH: f32 = 2.0;
const BOUNDARY_FALLOFF: f32 = 0.25;

struct Boid {
    position: Vec3,
    velocity: Vec3,
    rotation: Quat,
}

impl Boid {
    pub fn new(position: Vec3, velocity: Vec3) -> Boid {
        let mut boid = Boid {
            position,
            velocity,
            rotation: Quat::IDENTITY,
        };
        boid.update_rotation();
        boid
    }

    // Point the boid along its velocity, keeping the old rotation if the
    // velocity is zero
    fn update_rotation(&mut self) {
        if let Some(direction) = self.velocity.try_normalize() {
            self.rotation = Quat::from_rotation_arc(Vec3::Z, direction);
        }
    }
}

// Create vertices for a a boid
fn create_boid_vertices(num_faces: u8, r: f32, l: f32) -> Vec<Vertex> {
    let mut vertices = vec![];

    // Helper for points on cone base
    let ring = |n: u8| {
        let (s, c) = (TAU * n as f32 / num_faces as f32).sin_cos();
        Vec3::new(c, s, 0.0) * r
    };

    // Helper to push 3 points and calculate the correct normal
    let mut tri = |p0: Vec3, p1: Vec3, p2: Vec3| {
        let normal = (p1 - p0).cross(p2 - p0).normalize();
        vertices.push(Vertex {
            position: p0.into(),
            normal: normal.into(),
        });
        vertices.push(Vertex {
            position: p1.into(),
            normal: normal.into(),
        });
        vertices.push(Vertex {
            position: p2.into(),
            normal: normal.into(),
        });
    };

    // Tips are shared
    let tip_up = Vec3::new(0.0, 0.0, l);
    let tip_down = Vec3::new(0.0, 0.0, -l * 0.25);

    // Define all faces
    for n in 0..num_faces {
        // Calculate corner points
        let (a, b) = (ring(n), ring(n + 1));

        // Push both triangles, ensuring CCW winding order
        tri(a, b, tip_up);
        tri(b, a, tip_down);
    }
    return vertices;
}

// Scatter boids through a box around the origin, drifting slowly in random
// directions
fn spawn_boids(num_boids: u32) -> Vec<Boid> {
    let mut boids = vec![];
    let mut rng = rand::rng();
    for _ in 0..num_boids {
        boids.push(Boid::new(
            Vec3::new(
                rng.random_range(-1.0..1.0),
                rng.random_range(-1.0..1.0),
                rng.random_range(-1.0..1.0),
            ),
            Vec3::new(
                rng.random_range(-0.1..0.1),
                rng.random_range(-0.1..0.1),
                rng.random_range(-0.1..0.1),
            ),
        ));
    }
    boids
}

pub struct Boids {
    geom: Vec<Vertex>,
    boids: Vec<Boid>,
    num_vertices: u32,
    vertex_buffer: wgpu::Buffer,
    drawn_vertices: Vec<Vertex>,
    shadow_revision: u64,
    view_distance: f32,
    separation: f32,
    alignment: f32,
    cohesion: f32,
}

impl Boids {
    pub fn new(device: &Device, num_boids: u32) -> Self {
        // Initialise geometry once, this is shared by each Boid
        let geom = create_boid_vertices(8, 0.1, 0.3);
        let num_vertices = num_boids * geom.len() as u32;

        // Build Boids struct
        Self {
            boids: spawn_boids(num_boids),
            geom,
            num_vertices,
            vertex_buffer: device.create_buffer(&BufferDescriptor {
                label: Some("Boid vertices"),
                size: num_vertices as u64 * size_of::<Vertex>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            drawn_vertices: Vec::with_capacity(num_vertices as usize),
            shadow_revision: 0,
            view_distance: VIEW_DISTANCE,
            separation: SEPARATION,
            alignment: ALIGNMENT,
            cohesion: COHESION,
        }
    }

    pub fn upload(&mut self, queue: &wgpu::Queue) {
        // Clear old verticies
        self.drawn_vertices.clear();

        // Transform geom to the new positions
        for boid in self.boids.iter_mut() {
            for v in self.geom.iter_mut() {
                self.drawn_vertices.push(Vertex {
                    position: (boid.rotation * Vec3::from(v.position) + boid.position).into(),
                    normal: (boid.rotation * Vec3::from(v.normal)).into(),
                });
            }
        }

        // Write transformed vertices to queue
        queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&self.drawn_vertices));
    }

    pub fn gpu_bytes(&self) -> u64 {
        return self.vertex_buffer.size();
    }
}

impl Scene for Boids {
    // The boids fly in open space
    fn has_floor(&self) -> bool {
        false
    }

    // Back far enough to see the whole box they start in
    fn default_orbit(&self) -> Orbit {
        let mut orbit = Orbit::default();
        orbit.fit_robot();
        orbit
    }

    fn update(&mut self, _ctx: &SceneCtx, dt: f32) {
        self.shadow_revision += 1;

        // Calculate accelerations
        let mut accelerations = vec![];
        for this in self.boids.iter() {
            // Check neighbours
            let dist_range = f32::EPSILON..=self.view_distance;
            let mut separation = Vec3::ZERO;
            let mut neighbour_positions = Vec3::ZERO;
            let mut neighbour_velocity = Vec3::ZERO;
            let mut neighbour_count: f32 = 0.0;
            for other in self.boids.iter() {
                let offset = this.position - other.position;
                let dist = offset.length();
                if dist_range.contains(&dist) {
                    separation += offset / (dist * dist);
                    neighbour_positions += other.position;
                    neighbour_velocity += other.velocity;
                    neighbour_count += 1.0;
                }
            }
            // Add acceleration terms
            let mut acceleration = Vec3::ZERO;
            if neighbour_count > 0.0 {
                acceleration += self.separation * separation;
                acceleration += self.cohesion * ((neighbour_positions / neighbour_count) - this.position);
                acceleration += self.alignment * ((neighbour_velocity / neighbour_count) - this.velocity);
            }

            // Pull back towards the centre once outside the boundary, growing
            // e-fold every BOUNDARY_FALLOFF past it. The -1 makes it start
            // from zero at the edge rather than jumping in
            let overshoot = this.position.length() - BOUNDARY_RADIUS;
            if overshoot > 0.0 {
                let strength = BOUNDARY_STRENGTH * ((overshoot / BOUNDARY_FALLOFF).exp() - 1.0);
                acceleration -= strength * this.position.normalize();
            }
            accelerations.push(acceleration);
        }

        // Update boids
        for (boid, acceleration) in self.boids.iter_mut().zip(accelerations) {
            boid.velocity += acceleration * dt;
            boid.velocity = boid.velocity.clamp_length(MIN_SPEED, MAX_SPEED);
            boid.position += boid.velocity * dt;
            boid.update_rotation();
        }
    }

    // The vertices are already in world space. With no floor drawn, the
    // floor origin only centres the light's shadow view, a unit above it
    fn view(&self) -> SceneView {
        SceneView {
            floor_origin: Vec3::new(0., -1., 0.),
            ..Default::default()
        }
    }

    fn prepare(&mut self, queue: &wgpu::Queue, _camera: &CameraUniform) {
        self.upload(queue);
    }

    fn shadow_revision(&self) -> u64 {
        self.shadow_revision
    }

    // The shadow pass draws the same vertices, so it uses the default
    // draw_shadow
    fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.draw(0..self.num_vertices, 0..1)
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        gui::value_slider(ui, "View distance", &mut self.view_distance, 0.0..=2.0, 2, "");
        gui::value_slider(ui, "Separation", &mut self.separation, 0.0..=1.0, 2, "");
        gui::value_slider(ui, "Alignment", &mut self.alignment, 0.0..=10.0, 1, "");
        gui::value_slider(ui, "Cohesion", &mut self.cohesion, 0.0..=10.0, 1, "");

        // Respawn the same number of boids, keeping the settings above
        let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
        if ui.add_sized(size, egui::Button::new("Restart")).clicked() {
            self.boids = spawn_boids(self.boids.len() as u32);
        }
    }
}
