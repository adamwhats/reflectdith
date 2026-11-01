use std::time::Duration;

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
const ROT_VEL: f32 = 0.5;
// Light orbits the opposite way to the model
const LIGHT_ROT_VEL: f32 = -0.4;
// Meshes are loaded centred on the origin and scaled to fit a unit sphere
const EYE: Vec3 = Vec3::new(0.0, 1.0, 2.0);
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
    // Model-to-world transform (rotation)
    pub model: Mat4,
    // World-to-clip transform (view followed by projection)
    pub view_proj: Mat4,
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

// Return the CameraUniform at the current time
pub fn camera_uniform_at_time(run_time: Duration, aspect_ratio: f32) -> CameraUniform {
    // Static camera for now
    let camera = Vec4::new(EYE[0], EYE[1], EYE[2], 0.0);

    // Turntable spin around the world Y axis. The mesh is already centred and
    // oriented when loaded, so this is the only model transform
    let model = Mat4::from_rotation_y(run_time.as_secs_f32() * ROT_VEL);

    // Light orbits around the world Y axis
    let to_light = Quat::from_rotation_y(run_time.as_secs_f32() * LIGHT_ROT_VEL) * Vec3::ONE.normalize();

    // View - camera at EYE looking at the origin, where the mesh is centred
    let view = look_at_mat4(EYE, Vec3::ZERO, Vec3::Y);

    // Camera intrinsics
    let projection = directx::perspective(FOV_Y, aspect_ratio, NEAR, FAR);

    // Return CameraUniform
    CameraUniform {
        camera,
        to_light: to_light.extend(0.0),
        model,
        view_proj: projection * view,
    }
}
