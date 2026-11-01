use bytemuck::{Pod, Zeroable};
use glam::{Vec3, Vec4};
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
    Buffer, Device, FragmentState, PipelineCompilationOptions, PipelineLayout, RenderPipeline, RenderPipelineDescriptor,
    Queue, ShaderModule, TextureFormat, TextureView, VertexState, util::DeviceExt,
};

// The two tones the dither chooses between, in linear RGB. Edited from the
// side menu in gui.rs
#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: [f32; 3],
    pub light: [f32; 3],
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            dark: [0.002, 0.0, 0.01],
            light: [1.0, 0.15, 0.6],
        }
    }
}

// CPU-side mirror of the shader's palette uniform. The field order and layout
// must match the WGSL struct
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PaletteUniform {
    // Using Vec4 so bytes are aligned in WGSL, w is unused
    dark: Vec4,
    light: Vec4,
}

impl From<&Palette> for PaletteUniform {
    fn from(palette: &Palette) -> Self {
        Self {
            dark: Vec3::from(palette.dark).extend(0.0),
            light: Vec3::from(palette.light).extend(0.0),
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
}

impl PostProcess {
    pub fn new(
        device: &Device,
        shader_module: &ShaderModule,
        surface_format: TextureFormat,
        scene_view: &TextureView,
    ) -> Self {
        let palette_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Palette buffer"),
            contents: bytemuck::bytes_of(&PaletteUniform::from(&Palette::default())),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_group_layout = create_bind_group_layout(device);
        let bind_group = create_bind_group(device, &bind_group_layout, scene_view, &palette_buffer);
        let pipeline_layout = create_pipeline_layout(device, &bind_group_layout);
        let pipeline = create_pipeline(device, shader_module, &pipeline_layout, surface_format);

        Self {
            pipeline,
            bind_group_layout,
            bind_group,
            palette_buffer,
        }
    }

    // The bind group points at one specific texture view, so it has to be
    // rebuilt whenever the scene texture is recreated (window resize)
    pub fn set_scene_view(&mut self, device: &Device, scene_view: &TextureView) {
        self.bind_group = create_bind_group(device, &self.bind_group_layout, scene_view, &self.palette_buffer);
    }

    // Upload the palette, which takes effect at the next queue submit
    pub fn write_palette(&self, queue: &Queue, palette: &Palette) {
        queue.write_buffer(&self.palette_buffer, 0, bytemuck::bytes_of(&PaletteUniform::from(palette)));
    }
}

// Describes the shape of the post-process bindings, both visible to the
// fragment stage: a 2D float texture, read with textureLoad so it needs no
// sampler and no filtering, and the palette uniform buffer
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

// Binds the scene texture view to binding 0 and the palette to binding 1
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
