//! GPU regressions for the relationship between dither cells and scene sampling.
use super::{camera, pipeline, post, shadows, textures, vertices};
use glam::Vec3;
use std::collections::{HashMap, HashSet};
use wgpu::util::DeviceExt;

const SIZE: u32 = 64;

fn gpu() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(wgpu::util::initialize_adapter_from_env_or_default(&instance, None)).unwrap();
    pollster::block_on(adapter.request_device(&Default::default())).unwrap()
}

fn target(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Pixel-grid regression target"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn read_pixels(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture, bytes_per_pixel: u32) -> Vec<u8> {
    let row = SIZE * bytes_per_pixel;
    assert_eq!(row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Pixel-grid regression readback"),
        size: (row * SIZE) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(SIZE),
            },
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.map_async(wgpu::MapMode::Read, .., move |result| tx.send(result).unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = buffer.get_mapped_range(..).unwrap().to_vec();
    buffer.unmap();
    bytes
}

#[test]
#[ignore = "requires a working GPU adapter"]
fn geometry_shadow_and_material_edges_fill_complete_output_cells() {
    let (device, queue) = gpu();
    // Deliberately put a diagonal silhouette, changing light and a floor material
    // boundary through cells. Sampling only the threshold cannot pass this test.
    let input: Vec<[f32; 4]> = (0..SIZE)
        .flat_map(|y| {
            (0..SIZE).map(move |x| {
                if x < 17 + y / 3 {
                    let light = if (x + y) % 5 < 2 { 0.01 } else { 0.9 };
                    [light, light, light, 1.]
                } else {
                    let light = if x + y < 70 { 0.03 } else { 0.5 };
                    [light, light, light, -((x % 8) as f32 + 0.5) / 8.]
                }
            })
        })
        .collect();
    let input = device.create_texture_with_data(
        &queue,
        &wgpu::TextureDescriptor {
            label: Some("Sub-cell geometry and shadow edges"),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        bytemuck::cast_slice(&input),
    );
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(include_str!("post.wgsl").into()),
    });
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let post = post::PostProcess::new(
        &device,
        &queue,
        &shader,
        format,
        &input.create_view(&Default::default()),
    );
    let output = target(&device, format);
    let view = output.create_view(&Default::default());
    for (capture_scale, density, shared_grid) in [
        (1, 0.125, true),
        (2, 0.125, true),
        (1, 0.5, true),
        (1, 0.3, true),
        (2, 0.3, true),
        (1, 1., true),
        (2, 1., true),
        (1, 0.125, false),
        (1, 0.125, true),
    ] {
        post.write_uniform(
            &queue,
            &post::Palette {
                dark: [0.; 3],
                light: [1.; 3],
            },
            capture_scale,
            3,
            density,
            shared_grid,
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&post.pipeline);
            pass.set_bind_group(0, &post.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
        let pixels = read_pixels(&device, &queue, &output, 4);
        let pixel = |x: u32, y: u32| &pixels[((y * SIZE + x) * 4) as usize..((y * SIZE + x) * 4 + 4) as usize];
        let mut colours = HashSet::new();
        let mut cells = HashMap::new();
        let mut split_cells = false;
        for y in 0..SIZE {
            for x in 0..SIZE {
                let key = (
                    ((x as f32 + 0.5) * density / capture_scale as f32).floor() as i32,
                    ((y as f32 + 0.5) * density / capture_scale as f32).floor() as i32,
                );
                let expected = cells.entry(key).or_insert_with(|| pixel(x, y).to_vec());
                split_cells |= pixel(x, y) != expected.as_slice();
                if shared_grid {
                    assert_eq!(
                        pixel(x, y),
                        expected.as_slice(),
                        "edge cut output cell at ({x}, {y}), density={density}, capture={capture_scale}"
                    );
                }
                colours.insert(pixel(x, y).to_vec());
            }
        }
        assert!(colours.len() >= 2, "test scene must contain visible edges");
        if !shared_grid {
            assert!(
                split_cells,
                "disabling the shared grid must restore edges within Bayer cells"
            );
        }
    }
}

#[test]
#[ignore = "requires a working GPU adapter"]
fn floor_shadow_and_fade_are_constant_inside_each_world_noise_cell() {
    let (device, queue) = gpu();
    let camera_layout = camera::create_camera_transform_bind_group_layout(&device);
    let noise_layout = post::PostProcess::world_noise_layout(&device);
    let shadows = shadows::Shadows::new(&device, &camera_layout);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(include_str!("scene.wgsl").into()),
    });
    let layout = pipeline::create_pipeline_layout(&device, &camera_layout, &noise_layout, &shadows.scene_layout);
    let pipeline = pipeline::create_pipeline(
        &device,
        &shader,
        &layout,
        textures::SCENE_FORMAT,
        textures::DEPTH_FORMAT,
        "fragment_floor",
        None,
    );
    let shading = camera::Shading {
        floor_noise_density: 4.,
        floor_fade_start: 0.,
        floor_fade_end: 6.,
        ..Default::default()
    };
    let camera = camera::camera_uniform(&camera::Orbit::default(), glam::Mat4::IDENTITY, 0., &shading, 1., 0.);
    let floor_origin = Vec3::new(0., -0.6, 0.);
    let floor_camera = camera.for_floor_at(floor_origin);
    let camera_group = camera::CameraTransform::new(&device, &camera_layout, bytemuck::bytes_of(&camera));
    let floor_group = camera::CameraTransform::new(&device, &camera_layout, bytemuck::bytes_of(&floor_camera));
    let noise = device.create_texture_with_data(
        &queue,
        &wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &[128],
    );
    let noise_view = noise.create_view(&Default::default());
    let noise_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &noise_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&noise_view),
        }],
    });
    let point = |x, z| vertices::Vertex {
        position: [x, 0., z],
        normal: [0., 1., 0.],
    };
    let plate = [
        point(-0.5, -0.5),
        point(-0.5, 0.5),
        point(0.5, 0.5),
        point(-0.5, -0.5),
        point(0.5, 0.5),
        point(0.5, -0.5),
    ];
    let plate = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&plate),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let floor = vertices::create_floor_buffer(&device);
    let output = target(&device, textures::SCENE_FORMAT);
    let view = output.create_view(&Default::default());
    let depth = textures::create_depth_view(&device, SIZE, SIZE);
    let mut images = Vec::new();
    for enabled in [true, false] {
        shadows.update(&queue, &camera, floor_origin, if enabled { 1. } else { 0. });
        let mut encoder = device.create_command_encoder(&Default::default());
        if enabled {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &shadows.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            pass.set_pipeline(&shadows.pipeline);
            pass.set_bind_group(0, &camera_group.bind_group, &[]);
            pass.set_bind_group(1, &shadows.depth_group, &[]);
            pass.set_vertex_buffer(0, plate.slice(..));
            pass.draw(0..6, 0..1);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &floor_group.bind_group, &[]);
            pass.set_bind_group(1, &noise_group, &[]);
            pass.set_bind_group(2, &shadows.scene_group, &[]);
            pass.set_vertex_buffer(0, floor.slice(..));
            pass.draw(0..vertices::FLOOR_VERTEX_COUNT, 0..1);
        }
        queue.submit(Some(encoder.finish()));
        images.push(read_pixels(&device, &queue, &output, 8));
    }
    assert_ne!(images[0], images[1], "the plate must actually cast a visible shadow");
    let inverse = camera.view_proj.inverse();
    for pixels in images {
        let mut cells: HashMap<(i32, i32), ([u8; 6], usize)> = HashMap::new();
        for y in 0..SIZE {
            for x in 0..SIZE {
                let offset = ((y * SIZE + x) * 8) as usize;
                if pixels[offset + 7] & 0x80 == 0 {
                    continue;
                }
                let ndc = Vec3::new(
                    (x as f32 + 0.5) / SIZE as f32 * 2. - 1.,
                    1. - (y as f32 + 0.5) / SIZE as f32 * 2.,
                    0.,
                );
                let near = inverse.project_point3(ndc);
                let far = inverse.project_point3(Vec3 { z: 1., ..ndc });
                let world = near + (far - near) * ((floor_origin.y - near.y) / (far.y - near.y));
                let grid = world * shading.floor_noise_density;
                // Avoid CPU/GPU roundoff at cell boundaries.
                if [grid.x.fract().abs(), grid.z.fract().abs()]
                    .iter()
                    .any(|f| *f < 0.002 || *f > 0.998)
                {
                    continue;
                }
                let key = (grid.x.floor() as i32, grid.z.floor() as i32);
                let rgb: [u8; 6] = pixels[offset..offset + 6].try_into().unwrap();
                let entry = cells.entry(key).or_insert((rgb, 0));
                assert_eq!(entry.0, rgb, "shadow or fade cut world floor cell {key:?}");
                entry.1 += 1;
            }
        }
        assert!(cells.values().filter(|(_, count)| *count > 1).count() > 20);
    }
}

#[test]
#[ignore = "requires a working GPU adapter"]
fn first_vertex_indexing_renders_mesh_identically() {
    let (device, queue) = gpu();
    let camera_layout = camera::create_camera_transform_bind_group_layout(&device);
    let noise_layout = post::PostProcess::world_noise_layout(&device);
    // New shadows have zero strength, so lighting does not read the map
    let shadows = shadows::Shadows::new(&device, &camera_layout);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(include_str!("scene.wgsl").into()),
    });
    let layout = pipeline::create_pipeline_layout(&device, &camera_layout, &noise_layout, &shadows.scene_layout);
    let pipeline = pipeline::create_pipeline(
        &device,
        &shader,
        &layout,
        textures::SCENE_FORMAT,
        textures::DEPTH_FORMAT,
        "fragment_main",
        Some(wgpu::Face::Back),
    );
    let camera = camera::camera_uniform(
        &camera::Orbit::default(),
        glam::Mat4::from_rotation_y(37_f32.to_radians()),
        52.,
        &camera::Shading::default(),
        1.,
        0.,
    );
    let camera_group = camera::CameraTransform::new(&device, &camera_layout, bytemuck::bytes_of(&camera));
    let noise = device.create_texture_with_data(
        &queue,
        &wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &[128],
    );
    let noise_view = noise.create_view(&Default::default());
    let noise_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &noise_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&noise_view),
        }],
    });

    let source = vertices::from_stl(std::path::Path::new("models/utah_teapot_low.stl"));
    let unindexed = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&source),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let mesh = vertices::upload(&device, &source);
    assert!(
        mesh.gpu_bytes() < std::mem::size_of_val(source.as_slice()) as u64,
        "the teapot must take the indexed path for this test to mean anything"
    );

    let output = target(&device, textures::SCENE_FORMAT);
    let view = output.create_view(&Default::default());
    let depth = textures::create_depth_view(&device, SIZE, SIZE);
    let mut images = Vec::new();
    for indexed in [false, true] {
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &camera_group.bind_group, &[]);
            pass.set_bind_group(1, &noise_group, &[]);
            pass.set_bind_group(2, &shadows.scene_group, &[]);
            if indexed {
                mesh.draw(&mut pass);
            } else {
                pass.set_vertex_buffer(0, unindexed.slice(..));
                pass.draw(0..source.len() as u32, 0..1);
            }
        }
        queue.submit(Some(encoder.finish()));
        images.push(read_pixels(&device, &queue, &output, 8));
    }
    assert!(images[0].iter().any(|byte| *byte != 0), "the teapot must be in view");
    assert_eq!(images[0], images[1]);
}
