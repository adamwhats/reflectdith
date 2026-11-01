use std::f32::consts::TAU;

use super::{Scene, SceneCtx, SceneView};
use crate::engine::{camera::CameraUniform, camera::Orbit, vertices::Vertex};
use glam::{Quat, Vec3};
use rand::RngExt;
use wgpu::{BufferDescriptor, Device};

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

pub struct Boids {
    geom: Vec<Vertex>,
    boids: Vec<Boid>,
    num_vertices: u32,
    vertex_buffer: wgpu::Buffer,
    drawn_vertices: Vec<Vertex>,
    // Changed every update, as the boids move every frame, for the shadow
    // cache
    revision: u64,
}

impl Boids {
    pub fn new(device: &Device, num_boids: u32) -> Self {
        // Initialise geometry once, this is shared by each Boid
        let geom = create_boid_vertices(8, 0.03, 0.1);
        let num_vertices = num_boids * geom.len() as u32;

        // Create each Boid
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

        // Build Boids struct
        Self {
            boids,
            geom,
            num_vertices,
            vertex_buffer: device.create_buffer(&BufferDescriptor {
                label: Some("Boid vertices"),
                size: num_vertices as u64 * size_of::<Vertex>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            drawn_vertices: Vec::with_capacity(num_vertices as usize),
            revision: 0,
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

    fn update(&mut self, _ctx: &SceneCtx, _dt: f32) {
        // TODO: the simulation step goes here
        self.revision += 1;
        for boid in self.boids.iter_mut() {
            boid.position += boid.velocity * 0.02;
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
        self.revision
    }

    // The shadow pass draws the same vertices, so it uses the default
    // draw_shadow
    fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.draw(0..self.num_vertices, 0..1)
    }
}
