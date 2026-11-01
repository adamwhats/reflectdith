use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
    Device, FragmentState, PipelineCompilationOptions, PipelineLayout, RenderPipeline, RenderPipelineDescriptor,
    ShaderModule, TextureFormat, TextureView, VertexState,
};

// Groups the GPU-side resources for the post-process pass, which draws one
// fullscreen triangle into the surface and reads the offscreen scene texture
pub struct PostProcess {
    pub pipeline: RenderPipeline,
    // Kept so the bind group can be recreated when the scene texture is
    bind_group_layout: BindGroupLayout,
    pub bind_group: BindGroup,
}

impl PostProcess {
    pub fn new(
        device: &Device,
        shader_module: &ShaderModule,
        surface_format: TextureFormat,
        scene_view: &TextureView,
    ) -> Self {
        let bind_group_layout = create_bind_group_layout(device);
        let bind_group = create_bind_group(device, &bind_group_layout, scene_view);
        let pipeline_layout = create_pipeline_layout(device, &bind_group_layout);
        let pipeline = create_pipeline(device, shader_module, &pipeline_layout, surface_format);

        Self {
            pipeline,
            bind_group_layout,
            bind_group,
        }
    }

    // The bind group points at one specific texture view, so it has to be
    // rebuilt whenever the scene texture is recreated (window resize)
    pub fn set_scene_view(&mut self, device: &Device, scene_view: &TextureView) {
        self.bind_group = create_bind_group(device, &self.bind_group_layout, scene_view);
    }
}

// Describes the shape of the post-process binding: a single 2D float texture,
// visible to the fragment stage. It is read with textureLoad so it needs no
// sampler and no filtering
fn create_bind_group_layout(device: &Device) -> BindGroupLayout {
    device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: Some("Post bind group layout"),
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

// Binds the scene texture view to binding 0
fn create_bind_group(device: &Device, layout: &BindGroupLayout, scene_view: &TextureView) -> BindGroup {
    device.create_bind_group(&BindGroupDescriptor {
        label: Some("Post bind group"),
        layout,
        entries: &[BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(scene_view),
        }],
    })
}

// Describes what bind groups the post pipeline expects: just the scene texture
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
