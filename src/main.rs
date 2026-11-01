mod benchmark;
#[cfg(test)]
mod render_tests;
mod robot;
#[cfg(feature = "ros2")]
mod ros;
mod shadows;
mod topics;
use clap::Parser;
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
    keyboard::{KeyCode, PhysicalKey},
    window::Window,
};

use crate::capture::CAPTURE_SCALE;

use wgpu::{
    CurrentSurfaceTexture, Device, Instance, Queue, RenderPipeline, ShaderModuleDescriptor, ShaderSource, StoreOp,
    Surface, SurfaceColorSpace, SurfaceConfiguration, TextureView,
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
    alternate_orbit: camera::Orbit,
    #[cfg(feature = "ros2")]
    ros_connection: Option<ros::RosConnection>,
    // Whether the left button is held down, having been pressed outside the
    // menu, and where the cursor was last seen
    dragging: bool,
    last_cursor: Option<PhysicalPosition<f64>>,
    options: robot::Options,
    live: robot::SharedState,
    initial_robot: Option<robot::Robot>,
    started_at: Instant,
    benchmark: Option<benchmark::Benchmark>,
}

// Holds the wgpu context
struct WgpuContext {
    robot: Option<robot::RobotScene>,
    robot_generation: u64,
    robot_scene_revision: u64,
    world_noise: wgpu::BindGroup,
    shadows: shadows::Shadows,
    robot_layout: wgpu::BindGroupLayout,
    options: robot::Options,
    live: robot::SharedState,
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
    fn active_robot(&self) -> Option<&robot::RobotScene> {
        if self.gui.mode == gui::ViewMode::Robot {
            self.robot.as_ref()
        } else {
            None
        }
    }

    fn scene_uniform(
        &self,
        orbit: &camera::Orbit,
        aspect: f32,
        offset: f32,
    ) -> (camera::CameraUniform, glam::Vec3, glam::Mat4) {
        let view = self
            .active_robot()
            .map(|robot| robot.view(&self.live.lock().unwrap(), &self.options));
        let (motion, floor_origin, alignment) = view.map_or(
            (
                glam::Mat4::IDENTITY,
                glam::Vec3::new(0., self.mesh.min_y, 0.),
                glam::Mat4::IDENTITY,
            ),
            |view| (view.camera_motion, view.floor_origin, view.alignment),
        );
        let floor_height = motion.inverse().transform_point3(floor_origin).y;
        let orbit = orbit.above_floor(floor_height);
        let camera = camera::camera_uniform(
            &orbit,
            self.gui.controls.model_angle,
            self.gui.controls.light_angle,
            &self.gui.shading,
            aspect,
            offset,
        );
        (camera.follow(motion), floor_origin, alignment)
    }

    fn constrain_orbit(&self, orbit: &camera::Orbit) -> camera::Orbit {
        let floor_height = self.active_robot().map_or(self.mesh.min_y, |robot| {
            let view = robot.view(&self.live.lock().unwrap(), &self.options);
            view.camera_motion.inverse().transform_point3(view.floor_origin).y
        });
        orbit.above_floor(floor_height)
    }

    fn update_robot(&mut self) {
        if !self.options.ros && self.options.urdf.is_none() {
            return;
        }
        if self.options.ros {
            let state = self.live.lock().unwrap();
            if self.robot_generation != state.generation {
                if self.options.urdf.is_none() {
                    self.robot = None;
                }
                self.robot_generation = state.generation;
            }
            if let Some(active) = &state.active {
                if (self.options.fixed_frame != active.fixed_frame || self.options.follow_frame != active.follow_frame)
                    && let Some(robot) = &mut self.robot
                {
                    robot.reset_tracking();
                }
                self.options.description_topic.clone_from(&active.description);
                self.options.joint_topic.clone_from(&active.joints);
                self.options.fixed_frame.clone_from(&active.fixed_frame);
                self.options.follow_frame.clone_from(&active.follow_frame);
            }
            if let Some(requested) = &state.requested {
                let panel = self.gui.topics.get_or_insert_with(|| topics::TopicPanel {
                    catalog: topics::TopicCatalog::default(),
                    requested: requested.clone(),
                    active: None,
                    changed: false,
                });
                if panel.catalog != state.topics {
                    panel.catalog.clone_from(&state.topics);
                }
                if &panel.requested != requested {
                    panel.requested.clone_from(requested);
                }
                if panel.active != state.active {
                    panel.active.clone_from(&state.active);
                }
            }
        }
        let description = self.live.lock().unwrap().description.take();
        // An explicitly supplied file owns the geometry even if ROS publishes a description.
        if self.options.urdf.is_none()
            && let Some(xml) = description
        {
            eprintln!("Loading robot description and visual meshes...");
            match robot::Robot::load(&xml, &self.options.asset_dir, &self.options) {
                Ok(robot) => {
                    eprintln!(
                        "Loaded {}: {} visuals, root {}",
                        robot.name,
                        robot.visuals.len(),
                        robot.root
                    );
                    for warning in &robot.warnings {
                        eprintln!("Visual skipped: {warning}");
                    }
                    self.robot = Some(robot::RobotScene::new(robot, &self.device, &self.robot_layout));
                    self.robot_scene_revision = self.robot_scene_revision.wrapping_add(1);
                }
                Err(error) => {
                    self.live.lock().unwrap().error = Some(format!("Description: {error}"));
                }
            }
        }
        let state = self.live.lock().unwrap();
        self.gui.robot_status = Some(if let Some(error) = &state.error {
            error.clone()
        } else if let Some(scene) = &self.robot {
            let age = state.last_joint.map_or("waiting for joints".into(), |t| {
                format!("joints {:.1}s ago", t.elapsed().as_secs_f32())
            });
            let tf = if let Some(frame) = &self.options.fixed_frame {
                if robot::tf_transform(&state.tf, frame, &scene.robot.root).is_none() {
                    format!("; waiting for TF {frame} → {}", scene.robot.root)
                } else {
                    let follow = robot::follow_frame(&scene.robot, &self.options);
                    if robot::tf_transform(&state.tf, frame, &follow).is_none() {
                        format!("; waiting for {follow}")
                    } else {
                        String::new()
                    }
                }
            } else {
                String::new()
            };
            format!(
                "{} • {} visuals • {} skipped\n{}{}",
                scene.robot.name,
                scene.robot.visuals.len(),
                scene.robot.warnings.len(),
                age,
                tf
            )
        } else {
            if self.options.description_topic.is_empty() {
                "Choose a robot from the Robot dropdown".into()
            } else {
                format!("Waiting for {}", self.options.description_topic)
            }
        });
    }

    // Record the scene pass: draw the lit mesh into the given colour and depth
    // views, which are the same size as each other
    fn encode_shadow_pass(&self, encoder: &mut wgpu::CommandEncoder) {
        if !self.gui.shading.shadows {
            return;
        }
        let casters = if let Some(robot) = self.active_robot() {
            shadows::Casters::Robot {
                geometry: self.robot_scene_revision,
                pose: robot.pose_revision.get(),
            }
        } else if self.gui.mode == gui::ViewMode::Mesh {
            shadows::Casters::Mesh {
                model: self.loaded_model,
                angle: self.gui.controls.model_angle.to_bits(),
            }
        } else {
            shadows::Casters::Empty
        };
        // Camera and palette changes do not affect the light's depth map.
        if !self.shadows.needs_render(casters) {
            return;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Scene shadow depth pass"),
            color_attachments: &[],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.shadows.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        pass.set_pipeline(&self.shadows.pipeline);
        pass.set_bind_group(0, &self.camera_transform.bind_group, &[]);
        pass.set_bind_group(1, &self.shadows.depth_group, &[]);
        if let Some(robot) = self.active_robot() {
            robot.draw_shadow(&mut pass);
        } else if self.gui.mode == gui::ViewMode::Mesh {
            self.mesh.draw_shadow(&mut pass);
        }
    }

    fn encode_scene_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        colour_view: &TextureView,
        depth_view: &TextureView,
    ) {
        self.encode_shadow_pass(encoder);
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
        render_pass.set_bind_group(1, &self.world_noise, &[]);
        render_pass.set_bind_group(2, &self.shadows.scene_group, &[]);
        render_pass.set_bind_group(0, &self.camera_transform.bind_group, &[]);

        // Draw the selected mesh or every robot visual.
        if let Some(robot) = self.active_robot() {
            robot.draw(&mut render_pass);
        } else if self.gui.mode == gui::ViewMode::Mesh {
            self.mesh.draw(&mut render_pass);
        }

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
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // egui's clipboard worker borrows the Wayland display. Stop it before
        // run_app consumes the event loop and destroys that display connection.
        self.wgpu_context.take();
        self.window.take();
    }

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

        // Match the desktop file so Wayland can resolve the launcher and icon.
        #[cfg(target_os = "linux")]
        let window_attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            window_attributes.with_name("reflectdith", "reflectdith")
        };

        // Create a window with the specified attributes
        self.window = Some(Arc::new(
            event_loop
                .create_window(window_attributes)
                .expect("Failed to create window"),
        ));

        // Create a wgpu instance, entry point for the API
        let instance = Instance::default();

        // Match the adapter to the window and honor explicit wgpu overrides.
        let surface = instance
            .create_surface(Arc::clone(self.window.as_ref().unwrap()))
            .expect("Failed to create surface");
        let adapter = pollster::block_on(wgpu::util::initialize_adapter_from_env_or_default(
            &instance,
            Some(&surface),
        ))
        .expect("Failed to request adapter");
        eprintln!(
            "Renderer: {} ({:?})",
            adapter.get_info().name,
            adapter.get_info().backend
        );
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default())).expect("Failed to request device");

        let size = self.window.as_ref().unwrap().inner_size();
        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps.formats[0];

        // Define and apply the surface configuration
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width,
            height: size.height,
            present_mode: if self.options.benchmark_frames.is_some()
                && surface_caps.present_modes.contains(&wgpu::PresentMode::Immediate)
            {
                wgpu::PresentMode::Immediate
            } else {
                surface_caps.present_modes[0]
            },
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
        let shadows = shadows::Shadows::new(&device, &camera_transform_layout);
        let world_noise_layout = post::PostProcess::world_noise_layout(&device);
        let scene_pipeline_layout = scene::create_pipeline_layout(
            &device,
            &camera_transform_layout,
            &world_noise_layout,
            &shadows.scene_layout,
        );

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

        let world_noise = post.world_noise_bind_group(&device, &world_noise_layout);

        // Create the side menu, drawn straight to the surface
        let mut gui = gui::Gui::new(
            &device,
            self.window.as_ref().unwrap(),
            surface_format,
            models,
            loaded_model,
        );

        let robot = self
            .initial_robot
            .take()
            .map(|robot| robot::RobotScene::new(robot, &device, &camera_transform_layout));
        if self.options.ros || self.options.urdf.is_some() {
            gui.mode = gui::ViewMode::Robot;
            gui.controls.model_speed = 0.;
            gui.controls.light_speed = 0.;
            gui.robot_status = Some("Waiting for robot description".into());
        }
        gui.visible = !self.options.hide_gui;
        gui.shading.shadows = !self.options.no_shadows;
        // Store the wgpu context
        self.wgpu_context = Some(WgpuContext {
            world_noise,
            shadows,
            robot_generation: 0,
            robot_scene_revision: 0,
            robot,
            robot_layout: camera_transform_layout,
            options: self.options.clone(),
            live: self.live.clone(),
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
        // This shortcut is global so egui keyboard focus cannot swallow it.
        if let WindowEvent::KeyboardInput { event, .. } = &event
            && event.physical_key == PhysicalKey::Code(KeyCode::Tab)
            && event.state == ElementState::Pressed
            && !event.repeat
        {
            if let Some(context) = &mut self.wgpu_context {
                context.gui.visible = !context.gui.visible;
            }
            self.dragging = false;
            self.last_cursor = None;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            return;
        }
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
                    if let Some(context) = &self.wgpu_context {
                        self.orbit = context.constrain_orbit(&self.orbit);
                    }
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
                    if let Some(context) = &self.wgpu_context {
                        self.orbit = context.constrain_orbit(&self.orbit);
                    }
                }
            }

            // Main render task
            WindowEvent::RedrawRequested => {
                if let Some(wgpu_context) = &mut self.wgpu_context {
                    let window = self.window.as_ref().unwrap();

                    let benchmark_start = self.benchmark.as_ref().map(|_| Instant::now());
                    wgpu_context.update_robot();
                    // Lay out the side menu first, so any palette change is
                    // uploaded in time for this frame's post pass
                    let previous_mode = wgpu_context.gui.mode;
                    let gui_output = wgpu_context.gui.run(window);
                    if previous_mode != wgpu_context.gui.mode {
                        std::mem::swap(&mut self.orbit, &mut self.alternate_orbit);
                        self.dragging = false;
                        wgpu_context.gui.controls.model_speed = if wgpu_context.gui.mode == gui::ViewMode::Mesh {
                            camera::ROT_VEL.to_degrees()
                        } else {
                            0.
                        };
                        if wgpu_context.gui.mode == gui::ViewMode::Robot
                            && !wgpu_context.options.ros
                            && wgpu_context.options.urdf.is_none()
                        {
                            #[cfg(feature = "ros2")]
                            match ros::RosConnection::start(wgpu_context.options.clone(), self.live.clone()) {
                                Ok(connection) => {
                                    self.ros_connection = Some(connection);
                                    wgpu_context.options.ros = true;
                                    wgpu_context.gui.robot_status = Some("Waiting for robot description".into());
                                }
                                Err(error) => wgpu_context.gui.robot_status = Some(format!("ROS connection: {error}")),
                            }
                            #[cfg(not(feature = "ros2"))]
                            {
                                wgpu_context.gui.robot_status = Some("Live robots require the ROS build: scripts/ros-viewer. You can also launch with --urdf PATH.".into());
                            }
                        }
                    }
                    if let Some(topics) = &mut wgpu_context.gui.topics
                        && std::mem::take(&mut topics.changed)
                    {
                        wgpu_context.live.lock().unwrap().requested = Some(topics.requested.clone());
                    }

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
                    if wgpu_context.gui.mode == gui::ViewMode::Mesh
                        && wgpu_context.gui.selected_model != wgpu_context.loaded_model
                    {
                        wgpu_context.mesh =
                            vertices::create_mesh(&wgpu_context.device, wgpu_context.gui.selected_model_path());
                        wgpu_context.loaded_model = wgpu_context.gui.selected_model;
                    }

                    let auto_capture = self
                        .options
                        .capture_after
                        .is_some_and(|s| self.started_at.elapsed() >= Duration::from_secs(s));
                    if auto_capture {
                        wgpu_context.gui.capture_requested = true;
                    }
                    // Take a screenshot if the button was clicked. It submits
                    // its own commands, so it has to come before this frame's
                    // uniform writes below, which would otherwise be picked up
                    // by the screenshot's submit
                    if std::mem::take(&mut wgpu_context.gui.capture_requested) {
                        wgpu_context.gui.status = match wgpu_context.save_screenshot(&self.orbit) {
                            Ok((path, width, height)) => {
                                let message = format!("Saved {} ({width}x{height})", path.display());
                                eprintln!("{message}");
                                message
                            }
                            Err(error) => format!("Capture failed: {error}"),
                        };
                    }

                    if auto_capture {
                        event_loop.exit();
                    }
                    // Upload this frame's uniforms. The camera is shifted so the
                    // model is centred in the part of the window the menu
                    // doesn't cover
                    wgpu_context.post.write_uniform(
                        &wgpu_context.queue,
                        &wgpu_context.gui.palette,
                        1,
                        wgpu_context.gui.levels,
                        wgpu_context.gui.bayer_density,
                        wgpu_context.gui.shared_grid,
                    );
                    let width = wgpu_context.surface_config.width as f32;
                    let aspect_ratio = width / wgpu_context.surface_config.height as f32;
                    let ndc_offset_x = wgpu_context.gui.menu_width_px / width;
                    let (camera_uniform, floor_origin, alignment) =
                        wgpu_context.scene_uniform(&self.orbit, aspect_ratio, ndc_offset_x);
                    wgpu_context.queue.write_buffer(
                        &wgpu_context.camera_transform.buffer,
                        0,
                        bytemuck::bytes_of(&camera_uniform),
                    );
                    if let Some(robot) = wgpu_context.active_robot() {
                        robot.update(
                            &wgpu_context.queue,
                            &camera_uniform,
                            &wgpu_context.live.lock().unwrap(),
                            &wgpu_context.options,
                            alignment,
                        );
                    }
                    wgpu_context.queue.write_buffer(
                        &wgpu_context.floor_camera_transform.buffer,
                        0,
                        bytemuck::bytes_of(&camera_uniform.for_floor_at(floor_origin)),
                    );

                    wgpu_context.shadows.update(
                        &wgpu_context.queue,
                        &camera_uniform,
                        floor_origin,
                        wgpu_context.gui.shading.shadows,
                    );

                    let prepare_time = benchmark::elapsed(benchmark_start);
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

                    let encode_start = self.benchmark.as_ref().map(|_| Instant::now());
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
                    let encode_time = benchmark::elapsed(encode_start);
                    if let Some(benchmark) = &mut self.benchmark {
                        let gpu_wait_start = Instant::now();
                        wgpu_context
                            .device
                            .poll(wgpu::PollType::wait_indefinitely())
                            .expect("Benchmark GPU wait failed");
                        if benchmark.record(
                            prepare_time,
                            encode_time,
                            prepare_time + encode_time + gpu_wait_start.elapsed(),
                        ) {
                            eprintln!(
                                "BENCH shadow_maps_rendered={} (including warmup)",
                                wgpu_context.shadows.rendered_maps.get()
                            );
                            event_loop.exit();
                        }
                    }
                    wgpu_context.queue.present(surface_texture);

                    // Update the FPS counter in the window title once a second
                    self.fps_frame_count += 1;
                    let since_last_update = self.fps_last_update.elapsed();
                    if since_last_update >= Duration::from_secs(1) {
                        let fps = self.fps_frame_count as f32 / since_last_update.as_secs_f32();
                        window.set_title(&format!("ReflecDith - {:.0} FPS | Tab: controls", fps));
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = robot::Options::parse();
    let initial_robot = if let Some(path) = &options.urdf {
        let xml = std::fs::read_to_string(path)?;
        let robot = robot::Robot::load(&xml, path.parent().unwrap_or(std::path::Path::new(".")), &options)?;
        for warning in &robot.warnings {
            eprintln!("Visual skipped: {warning}");
        }
        eprintln!(
            "Loaded {}: {} visuals, root {}",
            robot.name,
            robot.visuals.len(),
            robot.root
        );
        Some(robot)
    } else {
        None
    };
    if options.check_robot {
        if initial_robot.is_none() {
            return Err("--check-robot requires --urdf".into());
        }
        return Ok(());
    }
    let live = robot::SharedState::default();
    live.lock().unwrap().robot_root = initial_robot.as_ref().map(|r| r.root.clone());
    #[cfg(feature = "ros2")]
    let ros_connection = if options.ros {
        Some(ros::RosConnection::start(options.clone(), live.clone())?)
    } else {
        None
    };
    #[cfg(not(feature = "ros2"))]
    if options.ros {
        return Err("ROS support is disabled; build with --features ros2 inside a sourced ROS 2 environment".into());
    }

    if options.list_topics || options.probe_ros.is_some() {
        let seconds = options.probe_ros.unwrap_or(5);
        if !options.ros {
            return Err("--probe-ros / --list-topics requires --ros".into());
        }
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            let state = live.lock().unwrap();
            if let Some(error) = &state.error {
                return Err(error.clone().into());
            }
        }
        let state = live.lock().unwrap();
        eprintln!(
            "ROS probe: {} named joints, description={}, TF edges={}",
            state.positions.len(),
            state.description.is_some(),
            state.tf.len()
        );
        if options.list_topics {
            for (kind, names) in [
                ("Descriptions (std_msgs/msg/String)", &state.topics.descriptions),
                ("Joint states", &state.topics.joints),
                ("TF topics", &state.topics.transforms),
                ("TF frames", &state.topics.frames),
            ] {
                println!("{kind}:");
                for name in names {
                    println!("  {name}");
                }
            }
            println!("Selected: {:?}", state.active);
            return Ok(());
        }
        if state.last_joint.is_none() {
            return Err("No valid joint positions received during probe".into());
        }
        return Ok(());
    }
    // Create the event loop
    let event_loop = EventLoop::new().expect("Failed to create event loop");

    let mut orbit = camera::Orbit::default();
    if options.ros || options.urdf.is_some() {
        orbit.fit_robot();
    }
    let mut alternate_orbit = camera::Orbit::default();
    if !options.ros && options.urdf.is_none() {
        alternate_orbit.fit_robot();
    }
    // Create the application handler
    let mut app = App {
        window: None,
        wgpu_context: None,
        last_frame_time: Instant::now(),
        fps_frame_count: 0,
        fps_last_update: Instant::now(),
        fps_last_frame_start: Instant::now(),
        orbit,
        alternate_orbit,
        #[cfg(feature = "ros2")]
        ros_connection,
        dragging: false,
        last_cursor: None,
        benchmark: options.benchmark_frames.map(|n| benchmark::Benchmark::new(n as usize)),
        options,
        live,
        initial_robot,
        started_at: Instant::now(),
    };

    // Run the event loop with the application handler
    event_loop.run_app(&mut app).expect("Failed to run event loop");
    eprintln!("Viewer shut down cleanly");
    Ok(())
}
