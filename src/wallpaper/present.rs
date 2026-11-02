use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Mapping {
    pub crop: [f32; 4],
    pub destination: [f32; 4],
    pub target: [f32; 4],
    pub dark: [f32; 4],
}

pub struct Canvas {
    pub _texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub size: (u32, u32),
    pub generation: u64,
}
impl Canvas {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, size: (u32, u32), generation: u64) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Shared wallpaper canvas"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Self {
            _texture: texture,
            view,
            size,
            generation,
        }
    }
}

pub struct Presenter {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
    binding: wgpu::BindGroup,
    generation: u64,
}
impl Presenter {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, canvas: &Canvas) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Monitor crop layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Monitor crop"),
            contents: &[0; std::mem::size_of::<Mapping>()],
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let binding = Self::binding(device, &layout, &uniform, &canvas.view);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Monitor presentation"),
            source: wgpu::ShaderSource::Wgsl(include_str!("present.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Monitor presentation"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Monitor presentation"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            layout,
            uniform,
            binding,
            generation: canvas.generation,
        }
    }
    fn binding(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        uniform: &wgpu::Buffer,
        view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Monitor canvas"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform.as_entire_binding(),
                },
            ],
        })
    }
    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        canvas: &Canvas,
        mapping: Mapping,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
    ) {
        if self.generation != canvas.generation {
            self.binding = Self::binding(device, &self.layout, &self.uniform, &canvas.view);
            self.generation = canvas.generation;
        }
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&mapping));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Present monitor crop"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.binding, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::render_tests::{gpu, read_pixels};
    use wgpu::util::DeviceExt;

    #[test]
    #[ignore = "requires a working GPU adapter"]
    fn monitor_crops_preserve_pixels_and_duplicates_fit_without_stretching() {
        let (device, queue) = gpu();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let pixels: Vec<u8> = (0..64)
            .flat_map(|y| (0..128).flat_map(move |x| [x as u8, y as u8, 150, 255]))
            .collect();
        let texture = device.create_texture_with_data(
            &queue,
            &wgpu::TextureDescriptor {
                label: Some("Known desktop pixels"),
                size: wgpu::Extent3d {
                    width: 128,
                    height: 64,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &pixels,
        );
        let view = texture.create_view(&Default::default());
        let canvas = Canvas {
            _texture: texture,
            view,
            size: (128, 64),
            generation: 1,
        };
        for (crop, fit, duplicate) in [
            ([0., 0., 0.5, 1.], [0., 0., 1., 1.], false),
            ([0.5, 0., 0.5, 1.], [0., 0., 1., 1.], false),
            ([0., 0., 1., 1.], super::super::layout::fit(canvas.size, (64, 64)), true),
        ] {
            let target = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("Monitor regression target"),
                size: wgpu::Extent3d {
                    width: 64,
                    height: 64,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let mut presenter = Presenter::new(&device, format, &canvas);
            let mut encoder = device.create_command_encoder(&Default::default());
            presenter.draw(
                &device,
                &queue,
                &canvas,
                Mapping {
                    crop,
                    destination: fit,
                    target: [64., 64., 0., 0.],
                    dark: [0., 0., 0., 1.],
                },
                &mut encoder,
                &target.create_view(&Default::default()),
            );
            queue.submit(Some(encoder.finish()));
            let output = read_pixels(&device, &queue, &target, 4);
            for y in 0..64 {
                for x in 0..64 {
                    let actual = &output[(y * 64 + x) * 4..][..4];
                    let expected = if duplicate {
                        if (16..48).contains(&y) {
                            [2 * x as u8 + 1, 2 * (y as u8 - 16) + 1, 150, 255]
                        } else {
                            [0, 0, 0, 255]
                        }
                    } else {
                        [x as u8 + (crop[0] * 128.) as u8, y as u8, 150, 255]
                    };
                    assert_eq!(actual, expected, "at ({x}, {y}), crop={crop:?}");
                }
            }
        }
    }
}
