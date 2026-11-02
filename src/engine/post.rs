use super::blue_noise;
use std::cell::Cell;

use bytemuck::{Pod, Zeroable};
use glam::{Vec3, Vec4};
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
    Buffer, Device, FragmentState, PipelineCompilationOptions, PipelineLayout, Queue, RenderPipeline,
    RenderPipelineDescriptor, ShaderModule, TextureFormat, TextureView, VertexState, util::DeviceExt,
};

pub use reflecdith_control::settings::{DEFAULT_BAYER_DENSITY, DEFAULT_LEVELS, MAX_LEVELS, Palette, bayer_cell_size};

// CPU-side mirror of the shader's post uniform. The field order and layout
// must match the WGSL struct
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PostUniform {
    // Using Vec4 so bytes are aligned in WGSL, w is unused
    tones: [Vec4; MAX_LEVELS as usize],
    // x is capture scale, y the number of tones, z Bayer cell width in pixels, w shared grid.
    block: Vec4,
}

impl PostUniform {
    fn new(palette: &Palette, block_size: u32, levels: u32, bayer_density: f32, shared_grid: bool) -> Self {
        let levels = levels.clamp(2, MAX_LEVELS);
        let dark = Vec3::from(palette.dark).powf(1. / 2.2);
        let light = Vec3::from(palette.light).powf(1. / 2.2);
        let mut tones = [Vec4::ZERO; MAX_LEVELS as usize];
        for (index, colour) in tones.iter_mut().take(levels as usize).enumerate() {
            let tone = index as f32 / (levels - 1) as f32;
            *colour = (dark * (1. - tone) + light * tone).powf(2.2).extend(0.);
        }
        Self {
            tones,
            block: Vec4::new(
                block_size as f32,
                levels as f32,
                bayer_cell_size(bayer_density) as f32,
                u32::from(shared_grid) as f32,
            ),
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
    last_palette: Cell<(Palette, u32, u32, f32, bool)>,
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
            contents: bytemuck::bytes_of(&PostUniform::new(
                &Palette::default(),
                1,
                DEFAULT_LEVELS,
                DEFAULT_BAYER_DENSITY,
                true,
            )),
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
            last_palette: Cell::new((Palette::default(), 1, DEFAULT_LEVELS, DEFAULT_BAYER_DENSITY, true)),
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
    pub fn write_uniform(
        &self,
        queue: &Queue,
        palette: &Palette,
        block_size: u32,
        levels: u32,
        bayer_density: f32,
        shared_grid: bool,
    ) {
        let settings = (*palette, block_size, levels, bayer_density, shared_grid);
        if self.last_palette.replace(settings) == settings {
            return;
        }
        queue.write_buffer(
            &self.palette_buffer,
            0,
            bytemuck::bytes_of(&PostUniform::new(
                palette,
                block_size,
                levels,
                bayer_density,
                shared_grid,
            )),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_palettes_stay_muted_with_readable_dark_light_separation() {
        for seed in 0..1024 {
            let palette = Palette::muted_from_seed(seed);
            for colour in [palette.dark, palette.light] {
                assert!(colour.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c)));
                let gamma = colour.map(|c| {
                    if c <= 0.0031308 {
                        12.92 * c
                    } else {
                        1.055 * c.powf(1. / 2.4) - 0.055
                    }
                });
                let min = gamma.into_iter().fold(f32::INFINITY, f32::min);
                let max = gamma.into_iter().fold(f32::NEG_INFINITY, f32::max);
                assert!(max - min <= 0.381, "saturated palette at seed {seed}");
            }
            let luminance = |c: [f32; 3]| Vec3::from(c).dot(Vec3::new(0.2126, 0.7152, 0.0722));
            assert!(luminance(palette.light) - luminance(palette.dark) > 0.06);
        }
    }

    #[test]
    fn precomputed_palette_keeps_endpoints_and_gamma_spaced_midtones() {
        let palette = Palette::default();
        for levels in 2..=MAX_LEVELS {
            let uniform = PostUniform::new(&palette, 2, levels, 1., true);
            assert_eq!(uniform.block, Vec4::new(2., levels as f32, 1., 1.));
            assert!(uniform.tones[0].truncate().abs_diff_eq(Vec3::from(palette.dark), 1e-6));
            assert!(
                uniform.tones[levels as usize - 1]
                    .truncate()
                    .abs_diff_eq(Vec3::from(palette.light), 1e-6)
            );
        }
        let uniform = PostUniform::new(
            &Palette {
                dark: [0.; 3],
                light: [1.; 3],
            },
            1,
            3,
            1.,
            true,
        );
        assert!(
            uniform.tones[1]
                .truncate()
                .abs_diff_eq(Vec3::splat(0.5_f32.powf(2.2)), 1e-6)
        );
    }
}
