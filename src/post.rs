use crate::blue_noise;

use bytemuck::{Pod, Zeroable};
use glam::{Vec3, Vec4};
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
    Buffer, Device, FragmentState, PipelineCompilationOptions, PipelineLayout, Queue, RenderPipeline,
    RenderPipelineDescriptor, ShaderModule, TextureFormat, TextureView, VertexState, util::DeviceExt,
};

// The tones the dither chooses between, in linear RGB. Edited from the
// side menu in gui.rs
#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: [f32; 3],
    pub light: [f32; 3],
}

// How many tones the dither quantises to, evenly spaced from the dark colour
// to the light colour. 2 is just the two colours
pub const DEFAULT_LEVELS: u32 = 3;

impl Default for Palette {
    fn default() -> Self {
        Self {
            // sRGB #170505 and #685841, stored as linear RGB.
            dark: [0.008568126, 0.001517635, 0.001517635],
            light: [0.13843162, 0.09758735, 0.052860647],
        }
    }
}

// CPU-side mirror of the shader's post uniform. The field order and layout
// must match the WGSL struct
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PostUniform {
    // Using Vec4 so bytes are aligned in WGSL, w is unused
    dark: Vec4,
    light: Vec4,
    // x is the size in pixels of each dither cell, which is 1 on screen and the
    // scale factor when capturing at a higher resolution so the dither keeps
    // the same look. y is the number of tones. The rest is unused
    block: Vec4,
}

impl PostUniform {
    fn new(palette: &Palette, block_size: u32, levels: u32) -> Self {
        Self {
            dark: Vec3::from(palette.dark).extend(0.0),
            light: Vec3::from(palette.light).extend(0.0),
            block: Vec4::new(block_size as f32, levels as f32, 0.0, 0.0),
        }
    }
}

// Groups the GPU-side resources for the post-process pass, which draws one
// fullscreen triangle into the surface and reads the offscreen scene texture
pub struct PostProcess {
    pub pipeline: RenderPipeline,
    // Kept so the bind group can be recreated when the scene texture is
    bind_group_layout: BindGroupLayout,
    pub bind_group: BindGroup,
    // Uniform buffer holding the dither palette
    palette_buffer: Buffer,
    // The blue noise texture used as the dither threshold for the floor
    noise_view: TextureView,
}

impl PostProcess {
    pub fn new(
        device: &Device,
        queue: &Queue,
        shader_module: &ShaderModule,
        surface_format: TextureFormat,
        scene_view: &TextureView,
    ) -> Self {
        let palette_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Palette buffer"),
            contents: bytemuck::bytes_of(&PostUniform::new(&Palette::default(), 1, DEFAULT_LEVELS)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let noise_view = create_noise_view(device, queue);
        let bind_group_layout = create_bind_group_layout(device);
        let bind_group = create_bind_group(device, &bind_group_layout, scene_view, &palette_buffer);
        let pipeline_layout = create_pipeline_layout(device, &bind_group_layout);
        let pipeline = create_pipeline(device, shader_module, &pipeline_layout, surface_format);

        Self {
            pipeline,
            bind_group_layout,
            bind_group,
            palette_buffer,
            noise_view,
        }
    }

    // The scene pass samples blue noise using world coordinates, then carries
    // its threshold in alpha for the palette pass. One binding is shared by all draws.
    pub fn world_noise_layout(device: &Device) -> BindGroupLayout {
        device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("World noise layout"),
            entries: &[BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        })
    }

    pub fn world_noise_bind_group(&self, device: &Device, layout: &BindGroupLayout) -> BindGroup {
        device.create_bind_group(&BindGroupDescriptor {
            label: Some("World noise"),
            layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&self.noise_view),
            }],
        })
    }

    // The bind group points at one specific texture view, so it has to be
    // rebuilt whenever the scene texture is recreated (window resize)
    pub fn set_scene_view(&mut self, device: &Device, scene_view: &TextureView) {
        self.bind_group = self.bind_group_for(device, scene_view);
    }

    // A bind group reading a different scene texture but the same palette,
    // used to dither a screenshot rendered at another resolution
    pub fn bind_group_for(&self, device: &Device, scene_view: &TextureView) -> BindGroup {
        create_bind_group(device, &self.bind_group_layout, scene_view, &self.palette_buffer)
    }

    // Upload the palette, dither cell size and number of tones, which take
    // effect at the next queue submit
    pub fn write_uniform(&self, queue: &Queue, palette: &Palette, block_size: u32, levels: u32) {
        queue.write_buffer(
            &self.palette_buffer,
            0,
            bytemuck::bytes_of(&PostUniform::new(palette, block_size, levels)),
        );
    }
}

// Make the blue noise texture and upload it, as a single 8-bit channel
fn create_noise_view(device: &Device, queue: &Queue) -> TextureView {
    let size = blue_noise::SIZE as u32;
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("Blue noise texture"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &blue_noise::generate(),
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

// Describes the shape of the post-process bindings, all visible to the
// fragment stage: the scene texture (read with textureLoad) and palette uniform.
fn create_bind_group_layout(device: &Device) -> BindGroupLayout {
    device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: Some("Post bind group layout"),
        entries: &[
            BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            BindGroupLayoutEntry {
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
    })
}

// Binds the scene texture (including floor thresholds in alpha) and palette.
fn create_bind_group(
    device: &Device,
    layout: &BindGroupLayout,
    scene_view: &TextureView,
    palette_buffer: &Buffer,
) -> BindGroup {
    device.create_bind_group(&BindGroupDescriptor {
        label: Some("Post bind group"),
        layout,
        entries: &[
            BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(scene_view),
            },
            BindGroupEntry {
                binding: 1,
                resource: palette_buffer.as_entire_binding(),
            },
        ],
    })
}

// Describes what bind groups the post pipeline expects: the scene texture and
// palette
fn create_pipeline_layout(device: &Device, bind_group_layout: &BindGroupLayout) -> PipelineLayout {
    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Post pipeline layout"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    })
}

// Creates the pipeline which draws the scene texture to the surface
fn create_pipeline(
    device: &Device,
    shader_module: &ShaderModule,
    pipeline_layout: &PipelineLayout,
    surface_format: TextureFormat,
) -> RenderPipeline {
    let render_pipeline_descriptor = RenderPipelineDescriptor {
        label: Some("Post Pipeline"),
        layout: Some(pipeline_layout),

        // No vertex buffers, the vertex shader builds the triangle from the
        // vertex index
        vertex: VertexState {
            module: shader_module,
            entry_point: Some("vertex_main"),
            compilation_options: PipelineCompilationOptions::default(),
            buffers: &[],
        },

        // Default primitive state, the fullscreen triangle is never culled
        primitive: wgpu::PrimitiveState::default(),

        // Writes straight to the surface
        fragment: Some(FragmentState {
            module: shader_module,
            entry_point: Some("fragment_main"),
            compilation_options: PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),

        // No depth in this pass
        depth_stencil: None,

        // Other fields
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    };

    device.create_render_pipeline(&render_pipeline_descriptor)
}
