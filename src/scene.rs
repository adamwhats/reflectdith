use crate::vertices;

use wgpu::{
    BindGroupLayout, DepthStencilState, Device, FragmentState, PipelineCompilationOptions, PipelineLayout,
    RenderPipeline, RenderPipelineDescriptor, ShaderModule, VertexState,
};

// Describes what bind groups the scene pipeline expects: just the camera
pub fn create_pipeline_layout(device: &Device, camera_transform_layout: &BindGroupLayout) -> PipelineLayout {
    device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("Scene pipeline layout"),
        bind_group_layouts: &[Some(camera_transform_layout)],
        immediate_size: 0,
    })
}

// Creates a pipeline which draws into the offscreen scene texture, using the
// given fragment shader entry point, so the mesh and the floor can share a
// vertex shader and differ in how they are shaded. The post-process pipeline
// lives in post.rs
pub fn create_pipeline(
    device: &Device,
    shader_module: &ShaderModule,
    pipeline_layout: &PipelineLayout,
    scene_format: wgpu::TextureFormat,
    depth_format: wgpu::TextureFormat,
    fragment_entry: &str,
    cull_mode: Option<wgpu::Face>,
) -> RenderPipeline {
    let render_pipeline_descriptor = RenderPipelineDescriptor {
        label: Some("Scene Pipeline"),
        layout: Some(pipeline_layout),

        // Add the vertex state
        vertex: VertexState {
            module: shader_module,
            entry_point: Some("vertex_main"),
            compilation_options: PipelineCompilationOptions::default(),
            buffers: &[Some(vertices::buffer_layout())],
        },

        // Add the primitive state
        primitive: wgpu::PrimitiveState {
            front_face: wgpu::FrontFace::Ccw,
            cull_mode,
            ..Default::default()
        },

        // Add the fragment state
        fragment: Some(FragmentState {
            module: shader_module,
            entry_point: Some(fragment_entry),
            compilation_options: PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: scene_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),

        // Add the depth state
        depth_stencil: Some(DepthStencilState {
            format: depth_format,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),

        // Other fields
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    };

    device.create_render_pipeline(&render_pipeline_descriptor)
}
