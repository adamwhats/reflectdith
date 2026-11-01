mod blue_noise;
mod camera;
mod capture;
mod gui;
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
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::Window,
};

use crate::capture::CAPTURE_SCALE;

use wgpu::{
    CurrentSurfaceTexture, Device, Instance, Queue, RenderPipeline, RequestAdapterOptions, ShaderModuleDescriptor,
    ShaderSource, StoreOp, Surface, SurfaceColorSpace, SurfaceConfiguration, TextureView,
};

// Caps the render loop at this many frames per second, 0 means uncapped
const TARGET_FPS: f64 = 0.0;

// Directory the models are listed from, and the name (without extension) of
// the one shown at startup
const MODEL_DIR: &str = "models";
const DEFAULT_MODEL: &str = "utah_teapot_low";

// Top-level struct holding the state of the main application
struct App {
    window: Option<Arc<Window>>,
    wgpu_context: Option<WgpuContext>,
    // When the last frame started, for advancing the rotations by real time
    last_frame_time: Instant,
    fps_frame_count: u32,
    fps_last_update: Instant,
    fps_last_frame_start: Instant,
    // The camera the mouse orbits and zooms
    orbit: camera::Orbit,
    // Whether the left button is held down, having been pressed outside the
    // menu, and where the cursor was last seen
    dragging: bool,
    last_cursor: Option<PhysicalPosition<f64>>,
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
    // The mesh to be rendered
    mesh: vertices::Mesh,
    // Index into the model list of the mesh currently loaded
    loaded_model: usize,
    // The floor under the mesh, a flat square, and the pipeline which draws it
    floor_buffer: wgpu::Buffer,
    floor_pipeline: RenderPipeline,
    // The camera's model and view-projection matrix buffer and its bind group
    camera_transform: camera::CameraTransform,
    // The same for the floor, which has its own model matrix as it doesn't
    // spin with the mesh
    floor_camera_transform: camera::CameraTransform,
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
    // The side menu for editing the palette
    gui: gui::Gui,
}

impl WgpuContext {
    // Record the scene pass: draw the lit mesh into the given colour and depth
    // views, which are the same size as each other
    fn encode_scene_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        colour_view: &TextureView,
        depth_view: &TextureView,
    ) {
        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            // Color attachments define what we are rendering to
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: colour_view,

                // 3D-texture layer and multisample-resolve target - not used here
                depth_slice: None,
                resolve_target: None,

                // Operations define what to do with the attachment before and after
                // rendering
                ops: wgpu::Operations {
                    // Pure black, so the background has zero luminance and
                    // the dither leaves it fully in the dark colour
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),

                    // Store operation saves the rendered content to the attachment
                    // after rendering
                    store: StoreOp::Store,
                },
            })],

            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth_view,
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

        // Bind the pipeline, the vertex buffer to slot 0, and the camera
        // transform
        render_pass.set_pipeline(&self.scene_pipeline);
        render_pass.set_vertex_buffer(0, self.mesh.vertex_buffer.slice(..));
        render_pass.set_bind_group(0, &self.camera_transform.bind_group, &[]);

        // Draw every vertex in the buffer as a non-indexed triangle list (one
        // instance)
        render_pass.draw(0..self.mesh.num_vertices as u32, 0..1);

        // Draw the floor the same way, with its own pipeline and camera
        render_pass.set_pipeline(&self.floor_pipeline);
        render_pass.set_vertex_buffer(0, self.floor_buffer.slice(..));
        render_pass.set_bind_group(0, &self.floor_camera_transform.bind_group, &[]);
        render_pass.draw(0..vertices::FLOOR_VERTEX_COUNT, 0..1);
    }

    // Record the post pass: draw the scene texture, bound in bind_group, into
    // the target view with one fullscreen triangle
    fn encode_post_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind_group: &wgpu::BindGroup,
        target_view: &TextureView,
    ) {
        let mut post_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target_view,
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
        post_pass.set_pipeline(&self.post.pipeline);
        post_pass.set_bind_group(0, bind_group, &[]);

        // Three vertices, no vertex buffer: the vertex shader builds the
        // triangle from the vertex index
        post_pass.draw(0..3, 0..1);
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Size the window to 1/CAPTURE_SCALE of the monitor in each direction,
        // so a screenshot, which is CAPTURE_SCALE times the window, is exactly
        // the monitor's resolution. The window is fixed in size to keep it so.
        // Falls back to a 1080p monitor if the size can't be found
        let monitor_size = event_loop
            .primary_monitor()
            .or_else(|| event_loop.available_monitors().next())
            .map(|monitor| monitor.size())
            .unwrap_or(PhysicalSize::new(1920, 1080));
        let window_size = PhysicalSize::new(monitor_size.width / CAPTURE_SCALE, monitor_size.height / CAPTURE_SCALE);

        // Define window attributes
        let window_attributes = Window::default_attributes()
            .with_title("ReflecDith")
            .with_inner_size(window_size)
            .with_resizable(false);

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

        // Find the available models and load the default one into the vertex
        // buffer, falling back to the first if the default isn't there
        let models = vertices::list_models(MODEL_DIR);
        let loaded_model = models
            .iter()
            .position(|path| path.file_stem().is_some_and(|stem| stem == DEFAULT_MODEL))
            .unwrap_or(0);
        let mesh = vertices::create_mesh(&device, &models[loaded_model]);
        let floor_buffer = vertices::create_floor_buffer(&device);

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

        let floor_camera_transform = camera::CameraTransform::new(
            &device,
            &camera_transform_layout,
            &[0; std::mem::size_of::<camera::CameraUniform>()],
        );

        // Create a depth texture view of the same size as the surface
        let depth_view = textures::create_depth_view(&device, size.width, size.height);

        // Create the offscreen scene texture view of the same size as the
        // surface, in the floating point scene format
        let scene_view = textures::create_scene_view(&device, size.width, size.height);

        // Describes what bind groups the scene pipeline expects
        let scene_pipeline_layout = scene::create_pipeline_layout(&device, &camera_transform_layout);

        // Bake the shader stages, layout, and config into an
        // immutable pipeline object. The mesh culls its back faces, and the
        // floor is visible from both sides
        let scene_pipeline = scene::create_pipeline(
            &device,
            &scene_shader_module,
            &scene_pipeline_layout,
            textures::SCENE_FORMAT,
            textures::DEPTH_FORMAT,
            "fragment_main",
            Some(wgpu::Face::Back),
        );
        let floor_pipeline = scene::create_pipeline(
            &device,
            &scene_shader_module,
            &scene_pipeline_layout,
            textures::SCENE_FORMAT,
            textures::DEPTH_FORMAT,
            "fragment_floor",
            None,
        );

        // Create the post-process pipeline and bind the scene texture to it
        let post = post::PostProcess::new(&device, &queue, &post_shader_module, surface_format, &scene_view);

        // Create the side menu, drawn straight to the surface
        let gui = gui::Gui::new(
            &device,
            self.window.as_ref().unwrap(),
            surface_format,
            models,
            loaded_model,
        );

        // Store the wgpu context
        self.wgpu_context = Some(WgpuContext {
            device,
            queue,
            surface,
            surface_config,
            mesh,
            loaded_model,
            floor_buffer,
            floor_pipeline,
            camera_transform,
            floor_camera_transform,
            depth_view,
            scene_view,
            scene_pipeline,
            post,
            gui,
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _window_id: winit::window::WindowId, event: WindowEvent) {
        // Let the side menu see every event first, for mouse and keyboard
        // input. If it used the event, such as a click on a slider, the camera
        // shouldn't also react to it
        let mut consumed_by_gui = false;
        if let (Some(window), Some(wgpu_context)) = (&self.window, &mut self.wgpu_context) {
            consumed_by_gui = wgpu_context.gui.on_window_event(window, &event);
        }

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
                    wgpu_context.scene_view =
                        textures::create_scene_view(&wgpu_context.device, size.width, size.height);

                    // The post bind group points at the old scene view, so
                    // rebind it to the new one
                    wgpu_context
                        .post
                        .set_scene_view(&wgpu_context.device, &wgpu_context.scene_view);
                }
            }

            // Start a camera drag with the left button, unless the click was on
            // the menu, and end it when the button is released
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                self.dragging = state == ElementState::Pressed && !consumed_by_gui;
            }

            // Orbit the camera by how far the cursor moved during a drag
            WindowEvent::CursorMoved { position, .. } => {
                if let (true, Some(last)) = (self.dragging, self.last_cursor) {
                    self.orbit
                        .drag((position.x - last.x) as f32, (position.y - last.y) as f32);
                }
                self.last_cursor = Some(position);
            }

            // Zoom with the scroll wheel, unless it's over the menu
            WindowEvent::MouseWheel { delta, .. } => {
                if !consumed_by_gui {
                    let steps = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        // Touchpads report pixels, about 40 to a wheel step
                        MouseScrollDelta::PixelDelta(position) => position.y as f32 / 40.0,
                    };
                    self.orbit.zoom(steps);
                }
            }

            // Main render task
            WindowEvent::RedrawRequested => {
                if let Some(wgpu_context) = &mut self.wgpu_context {
                    let window = self.window.as_ref().unwrap();

                    // Lay out the side menu first, so any palette change is
                    // uploaded in time for this frame's post pass
                    let gui_output = wgpu_context.gui.run(window);

                    // Advance the model and light rotation by the time since
                    // the last frame. This is after the menu, so an angle just
                    // set from a slider isn't undone
                    let now = Instant::now();
                    wgpu_context
                        .gui
                        .controls
                        .advance(now.duration_since(self.last_frame_time).as_secs_f32());
                    self.last_frame_time = now;

                    // Load the mesh again if a different model was picked in
                    // the menu
                    if wgpu_context.gui.selected_model != wgpu_context.loaded_model {
                        wgpu_context.mesh =
                            vertices::create_mesh(&wgpu_context.device, wgpu_context.gui.selected_model_path());
                        wgpu_context.loaded_model = wgpu_context.gui.selected_model;
                    }

                    // Take a screenshot if the button was clicked. It submits
                    // its own commands, so it has to come before this frame's
                    // uniform writes below, which would otherwise be picked up
                    // by the screenshot's submit
                    if std::mem::take(&mut wgpu_context.gui.capture_requested) {
                        wgpu_context.gui.status = match wgpu_context.save_screenshot(&self.orbit) {
                            Ok((path, width, height)) => format!("Saved {} ({width}x{height})", path.display()),
                            Err(error) => format!("Capture failed: {error}"),
                        };
                    }

                    // Upload this frame's uniforms. The camera is shifted so the
                    // model is centred in the part of the window the menu
                    // doesn't cover
                    wgpu_context.post.write_uniform(
                        &wgpu_context.queue,
                        &wgpu_context.gui.palette,
                        1,
                        wgpu_context.gui.levels,
                    );
                    let width = wgpu_context.surface_config.width as f32;
                    let aspect_ratio = width / wgpu_context.surface_config.height as f32;
                    let ndc_offset_x = wgpu_context.gui.menu_width_px / width;
                    let camera_uniform = camera::camera_uniform(
                        &self.orbit,
                        wgpu_context.gui.controls.model_angle,
                        wgpu_context.gui.controls.light_angle,
                        &wgpu_context.gui.shading,
                        aspect_ratio,
                        ndc_offset_x,
                    );
                    wgpu_context.queue.write_buffer(
                        &wgpu_context.camera_transform.buffer,
                        0,
                        bytemuck::bytes_of(&camera_uniform),
                    );
                    wgpu_context.queue.write_buffer(
                        &wgpu_context.floor_camera_transform.buffer,
                        0,
                        bytemuck::bytes_of(&camera_uniform.for_floor(wgpu_context.mesh.min_y)),
                    );

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
                    wgpu_context.encode_scene_pass(&mut encoder, &wgpu_context.scene_view, &wgpu_context.depth_view);

                    // Post pass: dither the scene texture into the surface
                    wgpu_context.encode_post_pass(&mut encoder, &wgpu_context.post.bind_group, &surface_view);

                    // Menu pass: draw the side menu over the dithered image
                    wgpu_context.gui.draw(
                        &wgpu_context.device,
                        &wgpu_context.queue,
                        &mut encoder,
                        &surface_view,
                        [wgpu_context.surface_config.width, wgpu_context.surface_config.height],
                        gui_output,
                    );

                    // Submit the recorded commands to the GPU queue
                    wgpu_context.queue.submit(Some(encoder.finish()));
                    wgpu_context.queue.present(surface_texture);

                    // Update the FPS counter in the window title once a second
                    self.fps_frame_count += 1;
                    let since_last_update = self.fps_last_update.elapsed();
                    if since_last_update >= Duration::from_secs(1) {
                        let fps = self.fps_frame_count as f32 / since_last_update.as_secs_f32();
                        window.set_title(&format!("ReflecDith - {:.0} FPS", fps));
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
                    window.request_redraw();
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
        last_frame_time: Instant::now(),
        fps_frame_count: 0,
        fps_last_update: Instant::now(),
        fps_last_frame_start: Instant::now(),
        orbit: camera::Orbit::default(),
        dragging: false,
        last_cursor: None,
    };

    // Run the event loop with the application handler
    event_loop.run_app(&mut app).expect("Failed to run event loop");
}
