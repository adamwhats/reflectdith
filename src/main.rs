mod camera;
mod post;
mod scene;
mod textures;
mod vertices;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::Window,
};

use wgpu::{
    CurrentSurfaceTexture, Device, Instance, Queue, RenderPipeline, RequestAdapterOptions, ShaderModuleDescriptor,
    ShaderSource, StoreOp, Surface, SurfaceColorSpace, SurfaceConfiguration, TextureView, util::DeviceExt,
};

// Caps the render loop at this many frames per second, 0 means uncapped
const TARGET_FPS: f64 = 0.0;

// Top-level struct holding the state of the main application
struct App {
    window: Option<Arc<Window>>,
    wgpu_context: Option<WgpuContext>,
    start_time: Instant,
    fps_frame_count: u32,
    fps_last_update: Instant,
    fps_last_frame_start: Instant,
}

// Holds the wgpu context
struct WgpuContext {
    // Device is the handle used to interact with the physical GPU (the Adapter)
    device: Device,
    // Queue is where we submit commands to be sent to the device for execution
    queue: Queue,
    // Surface is the drawable target on screen that we render to
    surface: Surface<'static>,
    // Surface configuration defines how the surface is configured for rendering
    surface_config: SurfaceConfiguration,
    // The vertices of the mesh to be rendered
    vertex_buffer: wgpu::Buffer,
    num_vertices: usize,
    // The camera's model and view-projection matrix buffer and its bind group
    camera_transform: camera::CameraTransform,
    // View of a texture storing per-pixel depth
    depth_view: TextureView,
    // View of the offscreen texture the scene is rendered into, which the
    // post-process pass then reads
    scene_view: TextureView,
    // The baked pipeline state for drawing the mesh: shader stages, vertex
    // layout, and fixed-function GPU config, bound before every draw call
    scene_pipeline: RenderPipeline,
    // The pipeline and bind group for the pass which draws the scene texture
    // to the surface
    post: post::PostProcess,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Define window attributes
        let window_attributes = Window::default_attributes().with_title("ReflecDith");

        // Create a window with the specified attributes
        self.window = Some(Arc::new(
            event_loop
                .create_window(window_attributes)
                .expect("Failed to create window"),
        ));

        // Create a wgpu instance, entry point for the API
        let instance = Instance::default();

        // Request an adapter, which represents a physical GPU, and the device,
        // which is the handle used to interact it Polling is used here
        // to block until the adapter and device are ready, as the request_*
        // methods are asynchronous
        let adapter_options = RequestAdapterOptions::default();
        let adapter =
            pollster::block_on(instance.request_adapter(&adapter_options)).expect("Failed to request adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default())).expect("Failed to request device");

        // Create a surface for rendering
        let surface = instance
            .create_surface(Arc::clone(self.window.as_ref().unwrap()))
            .expect("Failed to create surface");
        let size = self.window.as_ref().unwrap().inner_size();
        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps.formats[0];

        // Define and apply the surface configuration
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width,
            height: size.height,
            present_mode: surface_caps.present_modes[0],
            alpha_mode: surface_caps.alpha_modes[0],
            color_space: SurfaceColorSpace::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        // Load the stl vertices into the vertex buffer
        let vertices = vertices::from_stl("models/utah_teapot_low.stl");
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Mesh vertex buffer"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        // Compile the WGSL source into a shader modules
        let scene_shader_module = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("Scene shader"),
            source: ShaderSource::Wgsl(include_str!("scene.wgsl").into()),
        });
        let post_shader_module = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("Post shader"),
            source: ShaderSource::Wgsl(include_str!("post.wgsl").into()),
        });

        // Define the camera transform
        let camera_transform_layout = camera::create_camera_transform_bind_group_layout(&device);
        let camera_transform = camera::CameraTransform::new(
            &device,
            &camera_transform_layout,
            &[0; std::mem::size_of::<camera::CameraUniform>()],
        );

        // Create a depth texture view of the same size as the surface
        let depth_view = textures::create_depth_view(&device, size.width, size.height);

        // Create the offscreen scene texture view of the same size as the
        // surface, in the surface format so the pipelines' targets match
        let scene_view = textures::create_scene_view(&device, surface_format, size.width, size.height);

        // Describes what bind groups the scene pipeline expects
        let scene_pipeline_layout = scene::create_pipeline_layout(&device, camera_transform_layout);

        // Bake the shader stages, layout, and config into an
        // immutable pipeline object
        let scene_pipeline = scene::create_pipeline(
            &device,
            &scene_shader_module,
            &scene_pipeline_layout,
            surface_format,
            textures::DEPTH_FORMAT,
        );

        // Create the post-process pipeline and bind the scene texture to it
        let post = post::PostProcess::new(&device, &post_shader_module, surface_format, &scene_view);

        // Store the wgpu context
        self.wgpu_context = Some(WgpuContext {
            device,
            queue,
            surface,
            surface_config,
            vertex_buffer,
            num_vertices: vertices.len(),
            camera_transform,
            depth_view,
            scene_view,
            scene_pipeline,
            post,
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _window_id: winit::window::WindowId, event: WindowEvent) {
        // Any window event will initiate this match statement, allowing us to
        // handle specific events
        match event {
            // Handle window close event
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }

            // Handle window resize event
            WindowEvent::Resized(size) => {
                // A minimised window reports a size of zero, which wgpu
                // can't configure a surface or create textures for
                if size.width == 0 || size.height == 0 {
                    return;
                }

                if let Some(wgpu_context) = &mut self.wgpu_context {
                    wgpu_context.surface_config.width = size.width;
                    wgpu_context.surface_config.height = size.height;
                    wgpu_context
                        .surface
                        .configure(&wgpu_context.device, &wgpu_context.surface_config);

                    // Recreate the textures sized to the surface
                    wgpu_context.depth_view =
                        textures::create_depth_view(&wgpu_context.device, size.width, size.height);
                    wgpu_context.scene_view = textures::create_scene_view(
                        &wgpu_context.device,
                        wgpu_context.surface_config.format,
                        size.width,
                        size.height,
                    );

                    // The post bind group points at the old scene view, so
                    // rebind it to the new one
                    wgpu_context
                        .post
                        .set_scene_view(&wgpu_context.device, &wgpu_context.scene_view);
                }
            }

            // Main render task
            WindowEvent::RedrawRequested => {
                if let Some(wgpu_context) = &self.wgpu_context {
                    // Acquire the next frame for rendering
                    let frame = wgpu_context.surface.get_current_texture();

                    // Return the surface texture if frame acquisition is
                    // successful
                    let surface_texture = match frame {
                        CurrentSurfaceTexture::Success(frame) => frame,
                        CurrentSurfaceTexture::Suboptimal(frame) => frame,
                        // Discard other cases (Timeout, Occluded, Outdated) for now
                        _ => {
                            eprintln!("Failed to acquire next surface texture: {:?}", frame);
                            return;
                        }
                    };

                    // Render passes need a view, not the raw texture, so create
                    // one covering the whole thing
                    let surface_view = surface_texture
                        .texture
                        .create_view(&wgpu::TextureViewDescriptor::default());

                    // Start recording GPU commands (nothing actually runs until
                    // we submit this later)
                    let mut encoder = wgpu_context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

                    // Scene pass: draw the lit mesh into the offscreen scene
                    // texture
                    {
                        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            // Color attachments define what we are rendering to
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                // The offscreen scene texture is the target
                                view: &wgpu_context.scene_view,

                                // 3D-texture layer and multisample-resolve target - not used here
                                depth_slice: None,
                                resolve_target: None,

                                // Operations define what to do with the attachment before and after
                                // rendering
                                ops: wgpu::Operations {
                                    // Load operation clears the attachment to a specific color before
                                    // rendering
                                    // Pure black, so the background has zero
                                    // luminance and the dither leaves it fully
                                    // in the dark colour
                                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),

                                    // Store operation saves the rendered content to the attachment
                                    // after rendering
                                    store: StoreOp::Store,
                                },
                            })],

                            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                                view: &wgpu_context.depth_view,
                                depth_ops: Some(wgpu::Operations {
                                    // Set all pixels to far plane in clip-space depth
                                    load: wgpu::LoadOp::Clear(1.0),

                                    // We only need depth during this pass so can discard after use
                                    store: StoreOp::Discard,
                                }),
                                stencil_ops: None,
                            }),

                            // Set other fields to default
                            ..Default::default()
                        });

                        // Bind the pipeline,
                        render_pass.set_pipeline(&wgpu_context.scene_pipeline);

                        // Bind the vertex buffer to slot 0
                        render_pass.set_vertex_buffer(0, wgpu_context.vertex_buffer.slice(..));

                        // Bind the camera transform
                        render_pass.set_bind_group(0, &wgpu_context.camera_transform.bind_group, &[]);
                        let run_time = self.start_time.elapsed();
                        let aspect_ratio =
                            wgpu_context.surface_config.width as f32 / wgpu_context.surface_config.height as f32;
                        let camera_uniform = camera::camera_uniform_at_time(run_time, aspect_ratio);
                        let camera_uniform_bytes = bytemuck::bytes_of(&camera_uniform);
                        wgpu_context
                            .queue
                            .write_buffer(&wgpu_context.camera_transform.buffer, 0, camera_uniform_bytes);

                        // Draw every vertex in the buffer as a non-indexed
                        // triangle list (one instance)
                        render_pass.draw(0..wgpu_context.num_vertices as u32, 0..1);
                    }

                    // Post pass: draw the scene texture to the surface with
                    // one fullscreen triangle
                    {
                        let mut post_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                // The surface is the target
                                view: &surface_view,
                                depth_slice: None,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    // Every pixel is overwritten, so clear rather than load
                                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                    store: StoreOp::Store,
                                },
                            })],

                            // No depth in this pass, set other fields to default
                            ..Default::default()
                        });

                        // Bind the pipeline and the scene texture it reads
                        post_pass.set_pipeline(&wgpu_context.post.pipeline);
                        post_pass.set_bind_group(0, &wgpu_context.post.bind_group, &[]);

                        // Three vertices, no vertex buffer: the vertex shader
                        // builds the triangle from the vertex index
                        post_pass.draw(0..3, 0..1);
                    }

                    // Submit the recorded commands to the GPU queue
                    wgpu_context.queue.submit(Some(encoder.finish()));
                    wgpu_context.queue.present(surface_texture);

                    // Update the FPS counter in the window title once a second
                    self.fps_frame_count += 1;
                    let since_last_update = self.fps_last_update.elapsed();
                    if since_last_update >= Duration::from_secs(1) {
                        let fps = self.fps_frame_count as f32 / since_last_update.as_secs_f32();
                        self.window
                            .as_ref()
                            .unwrap()
                            .set_title(&format!("ReflecDith - {:.0} FPS", fps));
                        self.fps_frame_count = 0;
                        self.fps_last_update = Instant::now();
                    }

                    // FPS cap
                    if TARGET_FPS > 0.0 {
                        let target_frame_time = Duration::from_secs_f64(1.0 / TARGET_FPS);
                        let elapsed = self.fps_last_frame_start.elapsed();
                        if elapsed < target_frame_time {
                            std::thread::sleep(target_frame_time - elapsed);
                        }
                        self.fps_last_frame_start = Instant::now();
                    }

                    // Trigger the next redraw
                    self.window.as_ref().unwrap().request_redraw();
                }
            }
            // Fall through for other events
            _ => {}
        }
    }
}

fn main() {
    // Create the event loop
    let event_loop = EventLoop::new().expect("Failed to create event loop");

    // Create the application handler
    let mut app = App {
        window: None,
        wgpu_context: None,
        start_time: Instant::now(),
        fps_frame_count: 0,
        fps_last_update: Instant::now(),
        fps_last_frame_start: Instant::now(),
    };

    // Run the event loop with the application handler
    event_loop.run_app(&mut app).expect("Failed to run event loop");
}
