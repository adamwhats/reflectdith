use std::collections::VecDeque;
use std::f32::consts::TAU;

use super::{Scene, SceneCtx, SceneView, WallpaperMode};
use crate::engine::{camera::CameraUniform, camera::Orbit, gui, vertices::Vertex};
use glam::{Quat, Vec2, Vec3};
use rand::RngExt;
use wgpu::{BufferDescriptor, Device};

// Starting view distance, neighbour count and weights for the three rules,
// which are then set from the menu
const VIEW_DISTANCE: f32 = 0.5;
const NEIGHBOURS: usize = 7;
const SEPARATION: f32 = 0.2;
const ALIGNMENT: f32 = 2.0;
const COHESION: f32 = 2.0;
// Cosine of the half-angle each boid can see, so -0.5 leaves a 120° blind
// spot behind it
const FIELD_OF_VIEW: f32 = -0.5;
const MIN_SPEED: f32 = 1.0;
const MAX_SPEED: f32 = 5.0;
// Half-size of the viewer flight volume. Wallpaper bounds fit the fixed camera.
const BOUNDARY_SIZE: f32 = 3.0;
// Predict a turning arc from flight speed instead of braking into a wall.
const WALL_TURN_RATE: f32 = 5.0;
const WALL_CLEARANCE: f32 = 0.2;
// Starting trail length in seconds of flight, set from the menu. 0 turns
// trails off
const TRAIL_LENGTH: f32 = 0.0;
// Trails are sampled at this interval, and can be up to MAX_TRAIL_LENGTH
// seconds long, which sizes the vertex buffer
const TRAIL_INTERVAL: f32 = 1.0 / 20.0;
const MAX_TRAIL_LENGTH: f32 = 3.0;
const MAX_TRAIL_POINTS: usize = (MAX_TRAIL_LENGTH / TRAIL_INTERVAL) as usize;
// Sides around a boid's cone, radius of its base, which sits at its position,
// and length from there to its nose. Trails are tubes with the same sides,
// starting at the base's radius so the two meet edge to edge
const BOID_FACES: u8 = 8;
const BOID_RADIUS: f32 = 0.1;
const BOID_LENGTH: f32 = 0.3;

#[derive(Clone)]
struct Boid {
    position: Vec3,
    velocity: Vec3,
    rotation: Quat,
    // Past positions, newest first, one every TRAIL_INTERVAL
    trail: VecDeque<Vec3>,
}

impl Boid {
    pub fn new(position: Vec3, velocity: Vec3) -> Boid {
        let mut boid = Boid {
            position,
            velocity,
            rotation: Quat::IDENTITY,
            trail: VecDeque::new(),
        };
        boid.update_rotation();
        boid
    }

    // Remap positions and history together when the flight volume changes.
    // Keep speed in world units while aligning the heading with the new trail.
    fn rescale_space(&mut self, ratio: Vec3) {
        self.position *= ratio;
        for point in &mut self.trail {
            *point *= ratio;
        }
        let speed = self.velocity.length();
        self.velocity = (self.velocity * ratio).normalize_or_zero() * speed;
        self.update_rotation();
    }

    fn steer_from_walls(&mut self, half_extent: Vec3, dt: f32) {
        let speed = self.velocity.length();
        if speed < f32::EPSILON {
            return;
        }
        let heading = self.velocity / speed;
        let radius = speed / WALL_TURN_RATE;
        let margin = radius * 1.5 + WALL_CLEARANCE;
        let mut target = Vec3::ZERO;
        let mut urgency = 0_f32;
        for axis in 0..3 {
            let mut normal = Vec3::ZERO;
            normal[axis] = self.position[axis].signum();
            if heading.dot(normal) <= -0.25 {
                continue;
            }
            let gap = half_extent[axis] - self.position[axis].abs();
            let t = ((margin - gap) / (radius * 0.5)).clamp(0., 1.);
            let weight = t * t * (3. - 2. * t);
            if weight == 0. {
                continue;
            }
            let mut tangent = heading - normal * heading.dot(normal);
            if tangent.length_squared() < 0.01 {
                tangent = -self.position + normal * self.position.dot(normal);
                if tangent.length_squared() < 0.01 {
                    tangent = normal.cross(if axis == 1 { Vec3::Z } else { Vec3::Y });
                }
            }
            target += (tangent.normalize() - normal * 2.0).normalize() * weight;
            urgency = urgency.max(weight);
        }
        if let Some(target) = target.try_normalize() {
            let angle = heading.angle_between(target);
            if angle > 1e-5 {
                let fraction = (WALL_TURN_RATE * urgency * dt / angle).min(1.);
                let turn = Quat::IDENTITY.slerp(Quat::from_rotation_arc(heading, target), fraction);
                self.velocity = turn * self.velocity;
            }
        }
    }

    // Predictive steering normally turns before the wall; keep a hard limit so
    // strong flocking forces cannot carry a body outside the fitted volume.
    fn contain(&mut self, half_extent: Vec3) {
        let clamped = self.position.clamp(-half_extent, half_extent);
        for axis in 0..3 {
            if self.position[axis] != clamped[axis] && self.position[axis] * self.velocity[axis] > 0. {
                self.velocity[axis] = -self.velocity[axis];
            }
        }
        self.position = clamped;
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
fn spawn_boids(num_boids: u32, half_extent: Vec3) -> Vec<Boid> {
    let mut boids = vec![];
    let mut rng = rand::rng();
    for _ in 0..num_boids {
        boids.push(Boid::new(
            Vec3::new(
                rng.random_range(-1.0..1.0),
                rng.random_range(-1.0..1.0),
                rng.random_range(-1.0..1.0),
            ) * (half_extent / BOUNDARY_SIZE),
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
    neighbours: usize,
    separation: f32,
    alignment: f32,
    cohesion: f32,
    // Trail length in seconds from the menu, and the time since the last
    // trail point was recorded
    trail_length: f32,
    trail_timer: f32,
    time_scale: f32,
    half_extent: Vec3,
}

impl Boids {
    pub fn new(device: &Device, num_boids: u32) -> Self {
        // Initialise geometry once, this is shared by each Boid
        let geom = create_boid_vertices(BOID_FACES, BOID_RADIUS, BOID_LENGTH);

        // Room for every boid's body and longest trail, at 6 vertices (two
        // triangles) per side per trail segment. Fewer are drawn when trails
        // are short
        let max_vertices = num_boids as usize * (geom.len() + MAX_TRAIL_POINTS * BOID_FACES as usize * 6);

        // Build Boids struct
        Self {
            boids: spawn_boids(num_boids, Vec3::splat(BOUNDARY_SIZE)),
            geom,
            num_vertices: 0,
            vertex_buffer: device.create_buffer(&BufferDescriptor {
                label: Some("Boid vertices"),
                size: (max_vertices * size_of::<Vertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            drawn_vertices: Vec::with_capacity(max_vertices),
            shadow_revision: 0,
            view_distance: VIEW_DISTANCE,
            neighbours: NEIGHBOURS,
            separation: SEPARATION,
            alignment: ALIGNMENT,
            cohesion: COHESION,
            trail_length: TRAIL_LENGTH,
            trail_timer: 0.0,
            time_scale: 1.,
            half_extent: Vec3::splat(BOUNDARY_SIZE),
        }
    }

    fn step(&mut self, dt: f32) {
        self.shadow_revision += 1;

        // Record a trail point every TRAIL_INTERVAL, keeping as many as fit
        // in the trail length
        self.trail_timer += dt;
        let record_trail = self.trail_timer >= TRAIL_INTERVAL;
        if record_trail {
            self.trail_timer %= TRAIL_INTERVAL;
        }
        let trail_points = ((self.trail_length / TRAIL_INTERVAL).round() as usize).min(MAX_TRAIL_POINTS);

        // Calculate accelerations
        let mut accelerations = vec![];
        let mut visible = vec![];
        for this in self.boids.iter() {
            // Find the boids in range and in front, as (distance, index)
            let dist_range = f32::EPSILON..=self.view_distance;
            let heading = this.velocity.normalize_or_zero();
            visible.clear();
            for (index, other) in self.boids.iter().enumerate() {
                let to_other = other.position - this.position;
                let dist = to_other.length();
                if dist_range.contains(&dist) && heading.dot(to_other / dist) > FIELD_OF_VIEW {
                    visible.push((dist, index));
                }
            }

            // Only follow the nearest few, so a group can split when one side
            // turns away rather than every boid following the whole flock
            visible.sort_by(|a, b| a.0.total_cmp(&b.0));
            visible.truncate(self.neighbours);

            let mut separation = Vec3::ZERO;
            let mut neighbour_positions = Vec3::ZERO;
            let mut neighbour_velocity = Vec3::ZERO;
            let mut neighbour_count: f32 = 0.0;
            for &(dist, index) in visible.iter() {
                let other = &self.boids[index];
                separation += (this.position - other.position) / (dist * dist);
                neighbour_positions += other.position;
                neighbour_velocity += other.velocity;
                neighbour_count += 1.0;
            }
            // Add acceleration terms
            let mut acceleration = Vec3::ZERO;
            if neighbour_count > 0.0 {
                acceleration += self.separation * separation;
                acceleration += self.cohesion * ((neighbour_positions / neighbour_count) - this.position);
                acceleration += self.alignment * ((neighbour_velocity / neighbour_count) - this.velocity);
            }

            accelerations.push(acceleration);
        }

        // Update boids
        for (boid, acceleration) in self.boids.iter_mut().zip(accelerations) {
            boid.velocity += acceleration * dt;
            boid.velocity = boid.velocity.clamp_length(MIN_SPEED, MAX_SPEED);
            boid.steer_from_walls(self.half_extent, dt);
            boid.position += boid.velocity * dt;
            boid.contain(self.half_extent);
            boid.update_rotation();
            if record_trail {
                boid.trail.push_front(boid.position);
            }
            boid.trail.truncate(trail_points);
        }
    }

    pub fn upload(&mut self, queue: &wgpu::Queue) {
        // Clear old verticies
        self.drawn_vertices.clear();

        let sides = BOID_FACES as usize;
        let mut points = vec![];
        let mut rings = vec![];
        for boid in self.boids.iter() {
            // Transform geom to the new positions
            for v in self.geom.iter() {
                self.drawn_vertices.push(Vertex {
                    position: (boid.rotation * Vec3::from(v.position) + boid.position).into(),
                    normal: (boid.rotation * Vec3::from(v.normal)).into(),
                });
            }

            // The trail is a tube from the boid back through its past
            // positions, tapering from the cone's base to a point
            points.clear();
            points.push(boid.position);
            points.extend(boid.trail.iter().copied());
            let segments = points.len() - 1;

            // A ring of corners around each point. The first lines up with
            // the cone's base, and each after it is turned as little as
            // possible from the one before to sit across the trail, so the
            // tube doesn't twist
            rings.clear();
            let (mut u, mut v) = (boid.rotation * Vec3::X, boid.rotation * Vec3::Y);
            for k in 0..=segments {
                if k > 0 {
                    // Direction of travel through point k
                    if let Some(forward) = (points[k - 1] - points[(k + 1).min(segments)]).try_normalize() {
                        u = (u - forward * u.dot(forward)).try_normalize().unwrap_or(u);
                        v = forward.cross(u);
                    }
                }
                let radius = BOID_RADIUS * (1.0 - k as f32 / segments as f32);
                for j in 0..sides {
                    let (s, c) = (TAU * j as f32 / sides as f32).sin_cos();
                    rings.push(points[k] + (u * c + v * s) * radius);
                }
            }

            // Join each pair of rings with a quad per side
            for k in 0..segments {
                let centre = (points[k] + points[k + 1]) * 0.5;
                // Dims towards the tail. The scene shader reads the normal's
                // length as brightness
                let brightness = 1.0 - (k as f32 + 0.5) / segments as f32;
                for j in 0..sides {
                    let next = (j + 1) % sides;
                    let (a0, a1) = (rings[k * sides + j], rings[k * sides + next]);
                    let (b0, b1) = (rings[(k + 1) * sides + j], rings[(k + 1) * sides + next]);

                    // One normal for the quad, from its wider end, which never
                    // closes to a point, turned to face out of the tube
                    let mut normal = (a1 - a0).cross(b0 - a0).normalize_or_zero();
                    if normal.dot((a0 + a1 + b0 + b1) * 0.25 - centre) < 0.0 {
                        normal = -normal;
                    }
                    let normal = normal * brightness;

                    for [p0, mut p1, mut p2] in [[a0, a1, b0], [b0, a1, b1]] {
                        // Wind anticlockwise as seen from outside, so the
                        // outside isn't culled as a back face
                        if (p1 - p0).cross(p2 - p0).dot(normal) < 0.0 {
                            std::mem::swap(&mut p1, &mut p2);
                        }
                        for p in [p0, p1, p2] {
                            self.drawn_vertices.push(Vertex {
                                position: p.into(),
                                normal: normal.into(),
                            });
                        }
                    }
                }
            }
        }

        // Write transformed vertices to queue
        self.num_vertices = self.drawn_vertices.len() as u32;
        queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&self.drawn_vertices));
    }

    pub fn gpu_bytes(&self) -> u64 {
        return self.vertex_buffer.size();
    }
}

impl Scene for Boids {
    fn wallpaper_mode() -> WallpaperMode {
        WallpaperMode::Span
    }

    fn reset(&mut self) {
        self.boids = spawn_boids(self.boids.len() as u32, self.half_extent);
        self.trail_timer = 0.;
        self.shadow_revision += 1;
    }

    // The boids fly in open space
    fn has_floor(&self) -> bool {
        false
    }

    // Keep the original camera distance; wallpaper bounds fit this view.
    fn default_orbit(&self) -> Orbit {
        let mut orbit = Orbit::default();
        orbit.fit_sphere(BOUNDARY_SIZE);
        orbit
    }

    fn fit_wallpaper(&mut self, orbit: Orbit, aspect: f32) -> Orbit {
        let orbit = orbit.orthographic();
        let extent = orbit.visible_box(aspect, BOID_LENGTH.max(BOID_RADIUS));
        if !extent.is_finite() || extent.min_element() <= 0. || extent.abs_diff_eq(self.half_extent, 1e-5) {
            return orbit;
        }
        let ratio = extent / self.half_extent;
        for boid in &mut self.boids {
            boid.rescale_space(ratio);
        }
        self.half_extent = extent;
        self.shadow_revision += 1;
        eprintln!(
            "Boid flight volume half-extents: {:.2} × {:.2} × {:.2}",
            extent.x, extent.y, extent.z
        );
        orbit
    }

    fn update(&mut self, _ctx: &SceneCtx, dt: f32) {
        // Scale simulation time, then substep to keep flock steering stable
        // when the frame rate is low or playback is faster than real time.
        let elapsed = dt.clamp(0., 0.1) * self.time_scale;
        let steps = (elapsed / (1. / 120.)).ceil() as u32;
        if steps > 0 {
            let step = elapsed / steps as f32;
            for _ in 0..steps {
                self.step(step);
            }
        }
    }

    // The vertices are already in world space. With no floor drawn, the
    // floor origin only centres the light's shadow view, a unit above it
    fn view(&self) -> SceneView {
        SceneView {
            floor_origin: Vec3::new(0., -1., 0.),
            // Clear in front of the centre, fading out across the back half
            // of the box
            fog: Vec2::new(-self.half_extent.y / BOUNDARY_SIZE, self.half_extent.max_element()),
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

    fn apply_settings(&mut self, settings: &reflecdith_control::settings::SceneSettings) {
        let settings = &settings.boids;
        self.view_distance = settings.view_distance;
        self.neighbours = settings.neighbours;
        self.separation = settings.separation;
        self.alignment = settings.alignment;
        self.cohesion = settings.cohesion;
        self.trail_length = settings.trail_length;
        self.time_scale = settings.time_scale;
    }

    fn ui(&mut self, ui: &mut egui::Ui, settings: &mut reflecdith_control::settings::SceneSettings) {
        let settings = &mut settings.boids;
        gui::value_slider(ui, "Simulation speed", &mut settings.time_scale, 0.0..=2.0, 2, "×");
        gui::value_slider(ui, "View distance", &mut settings.view_distance, 0.0..=2.0, 2, "");
        gui::value_slider(ui, "Neighbours", &mut settings.neighbours, 1..=20, 0, "");
        gui::value_slider(ui, "Separation", &mut settings.separation, 0.0..=1.0, 2, "");
        gui::value_slider(ui, "Alignment", &mut settings.alignment, 0.0..=10.0, 1, "");
        gui::value_slider(ui, "Cohesion", &mut settings.cohesion, 0.0..=10.0, 1, "");
        gui::value_slider(
            ui,
            "Trails",
            &mut settings.trail_length,
            0.0..=MAX_TRAIL_LENGTH,
            2,
            " s",
        );

        // Respawn the same number of boids, keeping the settings above
        let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
        if ui.add_sized(size, egui::Button::new("Restart")).clicked() {
            self.reset();
        }
    }
}

#[cfg(test)]
mod playback_tests {
    use super::*;
    use crate::engine::{camera, render_tests::gpu};

    #[test]
    #[ignore = "requires a working GPU adapter"]
    fn slow_motion_scales_elapsed_time_and_zero_speed_freezes_the_flock() {
        let (device, _) = gpu();
        let layout = camera::create_camera_transform_bind_group_layout(&device);
        let ctx = SceneCtx {
            device: &device,
            camera_layout: &layout,
        };
        let mut slow = Boids::new(&device, 8);
        let mut normal = Boids::new(&device, 8);
        normal.boids = slow.boids.clone();
        slow.time_scale = 0.25;
        slow.update(&ctx, 0.04);
        normal.update(&ctx, 0.01);
        for (a, b) in slow.boids.iter().zip(&normal.boids) {
            assert!(a.position.abs_diff_eq(b.position, 1e-6));
            assert!(a.velocity.abs_diff_eq(b.velocity, 1e-6));
        }
        slow.time_scale = 0.;
        let before = slow.boids.clone();
        let revision = slow.shadow_revision;
        slow.update(&ctx, 0.1);
        for (before, after) in before.iter().zip(&slow.boids) {
            assert_eq!(before.position, after.position);
            assert_eq!(before.velocity, after.velocity);
        }
        assert_eq!(slow.shadow_revision, revision);
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn wall_turns_are_gradual_reach_the_edges_and_do_not_need_a_bounce() {
        let extent = Vec3::new(10.9, 2.05, 2.05);
        for speed in [1., 3., 5.] {
            for sign in [-1., 1.] {
                let mut boid = Boid::new(Vec3::ZERO, Vec3::X * speed * sign);
                let mut furthest = 0_f32;
                for _ in 0..3000 {
                    let before = boid.velocity;
                    boid.steer_from_walls(extent, 1. / 120.);
                    assert!(before.angle_between(boid.velocity) <= WALL_TURN_RATE / 120. + 1e-4);
                    assert!((boid.velocity.length() - speed).abs() < 1e-3);
                    boid.position += boid.velocity / 120.;
                    assert!(
                        boid.position.abs().cmple(extent).all(),
                        "would need a hard bounce: speed={speed}, position={:?}, velocity={:?}",
                        boid.position,
                        boid.velocity
                    );
                    furthest = furthest.max(boid.position.x.abs());
                    if boid.velocity.x * sign < 0. {
                        break;
                    }
                }
                assert!(boid.velocity.x * sign < 0.);
                assert!(furthest > extent.x * 0.9, "turned too early: {furthest}");
            }
        }
    }

    #[test]
    fn containment_reflects_outward_motion_but_preserves_inward_motion() {
        let extent = Vec3::new(6., 3., 3.);
        let mut boid = Boid::new(Vec3::new(6.2, -3.2, 3.2), Vec3::new(1., -2., 3.));
        boid.contain(extent);
        assert_eq!(boid.position, Vec3::new(6., -3., 3.));
        assert_eq!(boid.velocity, Vec3::new(-1., 2., -3.));
        boid.position = Vec3::new(7., 0., 0.);
        boid.velocity = Vec3::new(-1., 0., 0.);
        boid.contain(extent);
        assert_eq!(boid.position.x, 6.);
        assert_eq!(boid.velocity.x, -1.);
    }

    #[test]
    fn remapping_space_keeps_history_speed_and_heading_consistent() {
        let mut boid = Boid::new(Vec3::new(1., -0.5, 0.2), Vec3::new(0.2, 0.4, 0.8));
        boid.trail = [Vec3::new(0.5, -0.25, 0.1), Vec3::ZERO].into();
        let before = boid.clone();
        let ratio = Vec3::new(2., 3., 1.);
        boid.rescale_space(ratio);
        assert_eq!(boid.position, before.position * ratio);
        assert_eq!(boid.trail.len(), before.trail.len());
        for (after, before) in boid.trail.iter().zip(&before.trail) {
            assert_eq!(*after, *before * ratio);
        }
        assert!((boid.velocity.length() - before.velocity.length()).abs() < 1e-6);
        assert!((boid.rotation * Vec3::Z).abs_diff_eq(boid.velocity.normalize(), 1e-6));
        boid.rescale_space(ratio.recip());
        assert!(boid.position.abs_diff_eq(before.position, 1e-6));
        assert!(boid.velocity.abs_diff_eq(before.velocity, 1e-6));
    }

    #[test]
    #[ignore = "requires a working GPU adapter"]
    fn resizing_live_flock_updates_walls_without_respawning_or_accumulating_scale() {
        let (device, _) = crate::engine::render_tests::gpu();
        let mut flock = Boids::new(&device, 8);
        flock.boids[0].trail = [flock.boids[0].position * 0.5].into();
        let before = flock.boids.clone();
        let orbit = flock.default_orbit().orthographic();
        let wide = orbit.visible_box(32. / 9., BOID_LENGTH);
        let ratio = wide / flock.half_extent;
        flock.fit_wallpaper(orbit, 32. / 9.);
        assert_eq!(flock.half_extent, wide);
        assert_eq!(flock.boids.len(), before.len());
        for (after, before) in flock.boids.iter().zip(&before) {
            assert_eq!(after.position, before.position * ratio);
        }
        let revision = flock.shadow_revision;
        flock.fit_wallpaper(orbit, 32. / 9.);
        assert_eq!(flock.shadow_revision, revision);
        flock.fit_wallpaper(orbit, 16. / 9.);
        assert!(flock.half_extent.x < wide.x / 1.9);
        flock.fit_wallpaper(orbit, 32. / 9.);
        for (after, before) in flock.boids.iter().zip(&before) {
            assert!(after.position.abs_diff_eq(before.position * ratio, 1e-6));
        }
        assert!(flock.boids[0].trail[0].abs_diff_eq(before[0].trail[0] * ratio, 1e-6));
        flock.fit_wallpaper(orbit, 0.5);
        assert!(flock.half_extent.y > flock.half_extent.x);
        flock.fit_wallpaper(orbit, 32. / 9.);
        // The fitted right wall turns the bird without crossing the bound.
        flock.boids[0].position = Vec3::new(wide.x - 0.1, 0., 0.);
        flock.boids[0].velocity = Vec3::new(5., 0., 0.);
        for _ in 0..100 {
            flock.step(1. / 120.);
        }
        assert!(flock.boids[0].position.x <= wide.x);
        assert!(flock.boids[0].velocity.x < 0.);
        assert!(
            flock
                .boids
                .iter()
                .all(|b| b.position.is_finite() && b.velocity.is_finite())
        );
        let count = flock.boids.len();
        flock.trail_timer = 0.03;
        flock.reset();
        assert_eq!(flock.boids.len(), count);
        assert_eq!(flock.half_extent, wide);
        assert_eq!(flock.trail_timer, 0.);
        assert!(
            flock
                .boids
                .iter()
                .all(|b| b.trail.is_empty() && b.position.abs().cmple(wide).all())
        );
    }
}
