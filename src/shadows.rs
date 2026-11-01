//! Directional-light depth map, sampled by both robot and floor materials.
use bytemuck::{Pod, Zeroable};
use glam::{
    Mat4, Vec3, Vec4,
    camera::rh::{proj::directx, view::look_at_mat4},
};
use std::cell::Cell;
use wgpu::util::DeviceExt;

const MAP_SIZE: u32 = 2048;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Pod, Zeroable)]
struct ShadowUniform {
    light_view_proj: Mat4,
    settings: Vec4,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Casters {
    Robot { geometry: u64, pose: u64 },
    Mesh { model: usize, angle: u32 },
    Empty,
}

#[derive(Default)]
struct ShadowCache(Cell<Option<(u64, Casters)>>);
impl ShadowCache {
    fn needs_render(&self, light: u64, casters: Casters) -> bool {
        let stamp = (light, casters);
        self.0.replace(Some(stamp)) != Some(stamp)
    }
}

pub struct Shadows {
    pub pipeline: wgpu::RenderPipeline,
    pub depth_view: wgpu::TextureView,
    pub scene_layout: wgpu::BindGroupLayout,
    pub scene_group: wgpu::BindGroup,
    pub depth_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    last_uniform: Cell<Option<ShadowUniform>>,
    revision: Cell<u64>,
    cache: ShadowCache,
    pub rendered_maps: Cell<u64>,
}
impl Shadows {
    pub fn new(device: &wgpu::Device, camera_layout: &wgpu::BindGroupLayout) -> Self {
        let depth_view = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("Scene shadow map"),
                size: wgpu::Extent3d {
                    width: MAP_SIZE,
                    height: MAP_SIZE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Shadow light transform"),
            contents: bytemuck::bytes_of(&ShadowUniform {
                light_view_proj: Mat4::IDENTITY,
                settings: Vec4::ZERO,
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let uniform_entry = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let depth_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Shadow depth layout"),
            entries: &[uniform_entry.clone()],
        });
        let depth_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Shadow depth uniforms"),
            layout: &depth_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Scene shadow sampling layout"),
            entries: &[
                uniform_entry,
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Scene shadow comparison sampler"),
            compare: Some(wgpu::CompareFunction::LessEqual),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let scene_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Scene shadows"),
            layout: &scene_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&depth_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        // The depth pass has no binding to its own texture attachment.
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Shadow pipeline layout"),
            bind_group_layouts: &[Some(camera_layout), Some(&depth_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Shadow shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shadow.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Scene shadow depth pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex_shadow"),
                compilation_options: Default::default(),
                buffers: &[Some(crate::vertices::buffer_layout())],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.,
                    clamp: 0.,
                },
            }),
            multisample: Default::default(),
            fragment: None,
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            depth_view,
            scene_layout,
            scene_group,
            depth_group,
            buffer,
            last_uniform: Cell::new(None),
            revision: Cell::new(0),
            cache: ShadowCache::default(),
            rendered_maps: Cell::new(0),
        }
    }

    pub fn update(&self, queue: &wgpu::Queue, camera: &crate::camera::CameraUniform, floor: Vec3, strength: f32) {
        let centre = floor + Vec3::Y;
        let direction = camera.to_light.truncate().normalize();
        let view = look_at_mat4(centre + direction * 12., centre, Vec3::Y);
        let projection = directx::orthographic(-4., 4., -4., 4., 0.1, 30.);
        let uniform = ShadowUniform {
            light_view_proj: projection * view,
            settings: Vec4::new(strength, 0., 0., 0.),
        };
        let last = self.last_uniform.replace(Some(uniform));
        if last != Some(uniform) {
            queue.write_buffer(&self.buffer, 0, bytemuck::bytes_of(&uniform));
        }
        // Only the light's transform changes the depth map; strength is
        // applied when sampling it
        if last.map(|last| last.light_view_proj) != Some(uniform.light_view_proj) {
            self.revision.set(self.revision.get().wrapping_add(1));
        }
    }
    pub fn needs_render(&self, casters: Casters) -> bool {
        let render = self.cache.needs_render(self.revision.get(), casters);
        if render {
            self.rendered_maps.set(self.rendered_maps.get() + 1);
        }
        render
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_cache_tracks_light_geometry_pose_and_view_changes() {
        let cache = ShadowCache::default();
        let robot = Casters::Robot { geometry: 1, pose: 2 };
        assert!(cache.needs_render(1, robot));
        assert!(!cache.needs_render(1, robot));
        assert!(cache.needs_render(2, robot));
        assert!(cache.needs_render(2, Casters::Robot { geometry: 1, pose: 3 }));
        assert!(cache.needs_render(2, Casters::Robot { geometry: 2, pose: 3 }));
        let mesh = Casters::Mesh { model: 0, angle: 0 };
        assert!(cache.needs_render(2, mesh));
        assert!(!cache.needs_render(2, mesh));
        assert!(cache.needs_render(2, Casters::Mesh { model: 0, angle: 1 }));
        assert!(cache.needs_render(2, Casters::Mesh { model: 1, angle: 1 }));
        assert!(cache.needs_render(2, Casters::Empty));
        assert!(cache.needs_render(2, robot));
    }
}
