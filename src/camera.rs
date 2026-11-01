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
    pub shadows: bool,
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
            shadows: true,
        }
    }
}

impl CameraUniform {
    // The same camera and light, but for the floor: it doesn't spin with the
    // model, and sits at the given height
    pub fn follow(&self, motion: Mat4) -> Self {
        Self {
            camera: motion.transform_point3(self.camera.truncate()).extend(0.),
            view_proj: self.view_proj * motion.inverse(),
            ..*self
        }
    }

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
    // or down, where the up direction would flip
    pub fn drag(&mut self, dx: f32, dy: f32) {
        let limit = std::f32::consts::FRAC_PI_2 - 0.01;
        self.yaw -= dx * ORBIT_SENSITIVITY;
        self.pitch = (self.pitch + dy * ORBIT_SENSITIVITY).clamp(-limit, limit);
    }

    // Zoom by scroll steps, positive moves closer. It scales the distance so
    // each step feels the same near or far
    pub fn zoom(&mut self, steps: f32) {
        self.distance = (self.distance * (-steps * ZOOM_SENSITIVITY).exp()).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }
}

// Return the CameraUniform for the orbit camera, the model and light angles in
// degrees, and the shading. The image is shifted right by ndc_offset_x, in normalised device
// coordinates where the whole width is 2, to centre the model in the part of
// the window which isn't covered by the menu
pub fn camera_uniform(
    orbit: &Orbit,
    model_angle_degrees: f32,
    light_angle_degrees: f32,
    shading: &Shading,
    aspect_ratio: f32,
    ndc_offset_x: f32,
) -> CameraUniform {
    let eye = orbit.eye();
    let camera = eye.extend(0.0);

    // Spin around the world Y axis. The mesh is already centred and oriented
    // when loaded, so this is the only model transform
    let model = Mat4::from_rotation_y(model_angle_degrees.to_radians());

    // Light orbits around the world Y axis
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
    }
}
