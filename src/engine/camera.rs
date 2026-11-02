use bytemuck::{Pod, Zeroable};
use glam::{
    Mat4, Quat, Vec3, Vec4,
    camera::rh::{proj::directx, view::look_at_mat4},
};
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
    Buffer, Device, util::DeviceExt,
};

// Constants
// Default rotation speeds in radians per second
pub const ROT_VEL: f32 = 0.5;
// Light orbits the opposite way to the model
pub const LIGHT_ROT_VEL: f32 = -0.4;
// Meshes are loaded centred on the origin and scaled to fit a unit sphere
const EYE: Vec3 = Vec3::new(0.0, 1.0, 2.0);
// Radians of orbit per pixel dragged
const ORBIT_SENSITIVITY: f32 = 0.005;
// Zoom per scroll step, as a fraction of the distance
const ZOOM_SENSITIVITY: f32 = 0.1;
// Closest and furthest the camera can be from the origin. The closest is
// outside the unit sphere the mesh fits in
const MIN_DISTANCE: f32 = 1.2;
const MAX_DISTANCE: f32 = 20.0;
const FOV_Y: f32 = std::f32::consts::FRAC_PI_4;
const NEAR: f32 = 0.1;
const FAR: f32 = 100.0;

// Describes the shape of the camera transform binding: a single uniform
// buffer, visible to the vertex stage
pub fn create_camera_transform_bind_group_layout(device: &Device) -> BindGroupLayout {
    device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: Some("Camera transform bind group layout"),
        entries: &[BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

// CPU-side mirror of the shader's camera uniform. The field order and layout
// must match the WGSL struct
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct CameraUniform {
    // Camera look direction, using Vec4 so bytes are aligned in WGSL
    pub camera: Vec4,
    // Direction from the surface towards the light in world space, Vec4 for alignment
    pub to_light: Vec4,
    // x is the specular shininess and y the specular strength, Vec4 for alignment
    pub specular: Vec4,
    // x is the floor brightness, y and z the distances from the centre where
    // it starts and finishes fading to black; w is noise cells per world unit
    pub floor: Vec4,
    // Model-to-world transform (rotation)
    pub model: Mat4,
    // World-to-clip transform (view followed by projection)
    pub view_proj: Mat4,
    // x and y are where fog starts and finishes, as distances past the orbit
    // centre, so they hold as the camera zooms. Off unless y is above x. Last,
    // so shaders which don't fog can leave it out of their struct
    pub fog: Vec4,
}

// How the surfaces are shaded, edited from the side menu
#[derive(Clone, Copy)]
pub struct Shading {
    // How tight the specular highlight is, higher is smaller and sharper
    pub shininess: f32,
    // How bright the specular highlight is
    pub specular_strength: f32,
    // The grey level the floor reflects
    pub floor_brightness: f32,
    // Distances from the centre in world units where the floor starts and
    // finishes fading to black
    pub floor_fade_start: f32,
    pub floor_fade_end: f32,
    pub floor_noise_density: f32,
    // How dark shadows are, from 0 (off) to 1 (fully shadowed)
    pub shadow_strength: f32,
}

impl Default for Shading {
    fn default() -> Self {
        Self {
            shininess: 32.0,
            specular_strength: 1.5,
            floor_brightness: 0.5,
            floor_fade_start: 0.6,
            floor_fade_end: 2.5,
            floor_noise_density: 128.0,
            shadow_strength: 1.0,
        }
    }
}

impl CameraUniform {
    // Follow the footprint with the observer. Lighting stays in the fixed
    // world frame, so turning the robot changes its lighting and shadow direction.
    pub fn follow(&self, motion: Mat4) -> Self {
        Self {
            camera: motion.transform_point3(self.camera.truncate()).extend(0.),
            view_proj: self.view_proj * motion.inverse(),
            ..*self
        }
    }

    // The floor shares the camera and world light, with its own model origin.
    pub fn for_floor_at(&self, origin: Vec3) -> Self {
        Self {
            model: Mat4::from_translation(origin),
            ..*self
        }
    }
}

// Groups the GPU-side resources for the camera
pub struct CameraTransform {
    pub buffer: Buffer,
    pub bind_group: BindGroup,
}

// Camera transform helper type
impl CameraTransform {
    pub fn new(device: &Device, layout: &BindGroupLayout, camera_transform_data: &[u8]) -> Self {
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Camera transform buffer"),
            contents: camera_transform_data,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("Camera transform bind group"),
            layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });

        Self { buffer, bind_group }
    }
}

// A camera orbiting the origin, where the mesh is centred, at a distance
#[derive(Clone, Copy)]
pub struct Orbit {
    // Angle around the world Y axis in radians, 0 looks along -Z
    yaw: f32,
    // Angle above the XZ plane in radians
    pitch: f32,
    distance: f32,
}

// Starts at the default eye position
impl Default for Orbit {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            pitch: (EYE.y / EYE.length()).asin(),
            distance: EYE.length(),
        }
    }
}

impl Orbit {
    pub fn fit_robot(&mut self) {
        self.distance = 3.5;
    }

    // Back far enough for a sphere of this radius around the origin to fill
    // the view's height
    pub fn fit_sphere(&mut self, radius: f32) {
        self.distance = (radius / (FOV_Y / 2.0).sin()).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    // Where the camera is in world space
    fn eye(&self) -> Vec3 {
        self.distance
            * Vec3::new(
                self.pitch.cos() * self.yaw.sin(),
                self.pitch.sin(),
                self.pitch.cos() * self.yaw.cos(),
            )
    }

    // Orbit by a mouse drag in pixels. The scene follows the cursor, so
    // dragging right moves the camera left. Pitch stops short of straight up
    // or down; the actual floor height constrains the lower angle
    pub fn drag(&mut self, dx: f32, dy: f32) {
        let limit = std::f32::consts::FRAC_PI_2 - 0.01;
        self.yaw -= dx * ORBIT_SENSITIVITY;
        self.pitch = (self.pitch + dy * ORBIT_SENSITIVITY).clamp(-limit, limit);
    }

    // Apply the actual floor height in the orbit's frame, including after zooming.
    pub fn above_floor(&self, floor_height: f32) -> Self {
        let limit = std::f32::consts::FRAC_PI_2 - 0.01;
        let height = floor_height + 0.02;
        let distance = self.distance.max(height / limit.sin());
        let minimum_pitch = (height / distance).clamp(-1., 1.).asin().clamp(-limit, limit);
        Self {
            distance,
            pitch: self.pitch.max(minimum_pitch),
            ..*self
        }
    }

    // Zoom by scroll steps, positive moves closer. It scales the distance so
    // each step feels the same near or far
    pub fn zoom(&mut self, steps: f32) {
        self.distance = (self.distance * (-steps * ZOOM_SENSITIVITY).exp()).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }
}

// Return the CameraUniform for the orbit camera, the scene's model transform,
// the light angle in degrees, and the shading. The image is shifted right by
// ndc_offset_x, in normalised device coordinates where the whole width is 2,
// to centre the model in the part of the window which isn't covered by the
// menu
pub fn camera_uniform(
    orbit: &Orbit,
    model: Mat4,
    light_angle_degrees: f32,
    shading: &Shading,
    aspect_ratio: f32,
    ndc_offset_x: f32,
) -> CameraUniform {
    let eye = orbit.eye();
    let camera = eye.extend(0.0);

    // Add the light control's rotation around world Y, independently of the
    // robot heading and camera orbit.
    let to_light = Quat::from_rotation_y(light_angle_degrees.to_radians()) * Vec3::ONE.normalize();

    // View - camera at the eye looking at the origin, where the mesh is centred
    let view = look_at_mat4(eye, Vec3::ZERO, Vec3::Y);

    // Camera intrinsics
    let projection = directx::perspective(FOV_Y, aspect_ratio, NEAR, FAR);

    // Shift in clip space, after the projection. A translation there adds to
    // x in proportion to w, which is a constant shift once divided by w
    let shift = Mat4::from_translation(Vec3::new(ndc_offset_x, 0.0, 0.0));

    // Return CameraUniform
    CameraUniform {
        camera,
        to_light: to_light.extend(0.0),
        specular: Vec4::new(shading.shininess, shading.specular_strength, 0.0, 0.0),
        // The fade end is kept past its start, as smoothstep needs the start
        // to be below the end
        floor: Vec4::new(
            shading.floor_brightness,
            shading.floor_fade_start,
            shading.floor_fade_end.max(shading.floor_fade_start + 0.01),
            shading.floor_noise_density,
        ),
        model,
        view_proj: shift * projection * view,
        fog: Vec4::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orbit_stays_above_floor_after_drag_zoom_and_robot_follow() {
        let mut orbit = Orbit::default();
        orbit.drag(100., -1e6);
        let ground_level = orbit.above_floor(-0.5);
        assert!((ground_level.eye().y + 0.48).abs() < 1e-5);
        assert!(ground_level.pitch < 0., "allow orbiting below the model centre");
        let motion = Mat4::from_rotation_translation(Quat::from_rotation_y(1.2), Vec3::new(3., 2., -4.));
        for zoom in [100., -100., 100.] {
            orbit.zoom(zoom);
            for floor_height in [-1., 0., 0.8, 5.] {
                let safe = orbit.above_floor(floor_height);
                let camera = camera_uniform(&safe, Mat4::IDENTITY, 0., &Shading::default(), 1., 0.).follow(motion);
                assert!(camera.camera.y >= floor_height + 2.02 - 1e-5);
                assert!(safe.eye().is_finite());
            }
        }
    }

    #[test]
    fn world_light_and_added_rotation_stay_independent_of_footprint_heading() {
        let orbit = Orbit::default();
        let shading = Shading::default();
        let initial = camera_uniform(&orbit, Mat4::IDENTITY, 0., &shading, 16. / 9., 0.);
        let turn = Mat4::from_rotation_translation(
            Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            Vec3::new(2., 0., 3.),
        );
        let followed = initial.follow(turn);
        assert_eq!(followed.to_light, initial.to_light);
        assert_ne!(followed.camera, initial.camera);
        assert_ne!(followed.view_proj, initial.view_proj);
        let light = followed.to_light.truncate();
        let turned_surface_normal = turn.transform_vector3(Vec3::X);
        assert!((turned_surface_normal.dot(light) - Vec3::X.dot(light)).abs() > 0.5);
        let rotated = camera_uniform(&orbit, Mat4::IDENTITY, 35., &shading, 16. / 9., 0.).follow(turn);
        let expected = Quat::from_rotation_y(35_f32.to_radians()) * initial.to_light.truncate();
        assert!(rotated.to_light.truncate().abs_diff_eq(expected, 1e-6));
        assert_eq!(rotated.for_floor_at(Vec3::X).to_light, rotated.to_light);
    }
}
