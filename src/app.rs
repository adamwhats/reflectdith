//! The winit application: owns the window, the engine and the scenes, and
//! turns input and redraws into calls on them.
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use wgpu::CurrentSurfaceTexture;
use winit::{
    application::ApplicationHandler,
    dpi::{PhysicalPosition, PhysicalSize},
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow},
    keyboard::{KeyCode, PhysicalKey},
    window::Window,
};

use crate::{
    cli::Cli,
    engine::{Engine, benchmark, capture::CAPTURE_SCALE, stats, timing},
    scenes::Scenes,
};

// Top-level struct holding the state of the main application
pub struct App {
    window: Option<Arc<Window>>,
    engine: Option<Engine>,
    gui: Option<crate::engine::gui::Gui>,
    // Every scene, each with its own camera, and the one shown at startup
    scenes: Scenes,
    initial_scene: usize,
    // When the last frame started, for advancing animations by real time
    last_frame_time: Instant,
    fps_frame_count: u32,
    fps_last_update: Instant,
    usage_stats: stats::UsageStats,
    next_frame_time: Instant,
    fps_limit: Option<u32>,
    // Whether the left button is held down, having been pressed outside the
    // menu, and where the cursor was last seen
    dragging: bool,
    last_cursor: Option<PhysicalPosition<f64>>,
    options: Cli,
    started_at: Instant,
    benchmark: Option<benchmark::Benchmark>,
}

impl App {
    pub fn new(options: Cli, scenes: Scenes, initial_scene: usize) -> Self {
        Self {
            window: None,
            engine: None,
            gui: None,
            scenes,
            initial_scene,
            last_frame_time: Instant::now(),
            fps_frame_count: 0,
            fps_last_update: Instant::now(),
            usage_stats: stats::UsageStats::new(),
            next_frame_time: Instant::now(),
            fps_limit: Some(60),
            dragging: false,
            last_cursor: None,
            benchmark: options.benchmark_frames.map(|n| benchmark::Benchmark::new(n as usize)),
            options,
            started_at: Instant::now(),
        }
    }
}

impl ApplicationHandler for App {
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // egui's clipboard worker borrows the Wayland display. Stop it before
        // run_app consumes the event loop and destroys that display connection.
        self.gui.take();
        self.engine.take();
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
            .with_title("ReflecDith - Tab: controls")
            .with_inner_size(window_size)
            .with_resizable(false);

        // Match the desktop file so Wayland can resolve the launcher and icon.
        #[cfg(target_os = "linux")]
        let window_attributes = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            window_attributes.with_name("reflectdith", "reflectdith")
        };

        // Create a window with the specified attributes
        let window = Arc::new(
            event_loop
                .create_window(window_attributes)
                .expect("Failed to create window"),
        );
        self.window = Some(Arc::clone(&window));

        let mut engine = Engine::new(window, self.options.benchmark_frames.is_some());
        if self.benchmark.is_none() {
            engine.set_vsync(true);
        }
        // A live or loaded robot holds the light still
        if self.options.robot.ros || self.options.robot.urdf.is_some() {
            engine.settings.controls.light_speed = 0.;
        }
        let mut gui = crate::engine::gui::Gui::new(
            &engine.device,
            self.window.as_ref().unwrap(),
            engine.surface_config.format,
        );
        gui.visible = !self.options.hide_gui;
        self.gui = Some(gui);
        if self.options.no_shadows {
            engine.settings.shading.shadow_strength = 0.0;
        }

        // Build and show the startup scene
        engine.settings.selected_scene = self.initial_scene;
        self.scenes.activate(self.initial_scene, &engine.scene_ctx());
        self.engine = Some(engine);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _window_id: winit::window::WindowId, event: WindowEvent) {
        // This shortcut is global so egui keyboard focus cannot swallow it.
        if let WindowEvent::KeyboardInput { event, .. } = &event
            && event.physical_key == PhysicalKey::Code(KeyCode::Tab)
            && event.state == ElementState::Pressed
            && !event.repeat
        {
            if let Some(gui) = &mut self.gui {
                gui.visible = !gui.visible;
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
        if let (Some(window), Some(gui)) = (&self.window, &mut self.gui) {
            consumed_by_gui = gui.on_window_event(window, &event);
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

                if let Some(engine) = &mut self.engine {
                    engine.resize(size.width, size.height);
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
                if let (true, Some(last), Some(engine)) = (self.dragging, self.last_cursor, &self.engine) {
                    let (scene, orbit) = self.scenes.active_with_orbit();
                    orbit.drag((position.x - last.x) as f32, (position.y - last.y) as f32);
                    *orbit = engine.constrain_orbit(orbit, scene);
                }
                self.last_cursor = Some(position);
            }

            // Zoom with the scroll wheel, unless it's over the menu
            WindowEvent::MouseWheel { delta, .. } => {
                if !consumed_by_gui && let Some(engine) = &self.engine {
                    let steps = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        // Touchpads report pixels, about 40 to a wheel step
                        MouseScrollDelta::PixelDelta(position) => position.y as f32 / 40.0,
                    };
                    let (scene, orbit) = self.scenes.active_with_orbit();
                    orbit.zoom(steps);
                    *orbit = engine.constrain_orbit(orbit, scene);
                }
            }

            // Main render task
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                if self.benchmark.is_none() && self.fps_limit.is_some() && now < self.next_frame_time {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame_time));
                    return;
                }
                let Some(engine) = &mut self.engine else {
                    return;
                };
                let window = self.window.as_ref().unwrap();

                let benchmark_start = self.benchmark.as_ref().map(|_| Instant::now());
                // Lay out the side menu first, so any palette or scene setting
                // change is applied in time for this frame
                let scene_names = self.scenes.names();
                let gui = self.gui.as_mut().unwrap();
                let old_limit = self.fps_limit;
                let gui_output = gui.run(
                    window,
                    &scene_names,
                    self.scenes.active_mut(),
                    &mut engine.settings,
                    &mut self.fps_limit,
                );
                if self.benchmark.is_none() {
                    if old_limit != self.fps_limit {
                        engine.set_vsync(self.fps_limit.is_some());
                    }
                    if let Some(fps) = self.fps_limit {
                        let interval = Duration::from_secs_f64(1. / fps as f64);
                        self.next_frame_time = if old_limit != self.fps_limit {
                            now + interval
                        } else {
                            timing::next_deadline(self.next_frame_time, now, interval)
                        };
                    } else {
                        event_loop.set_control_flow(ControlFlow::Poll);
                    }
                }
                if engine.settings.selected_scene != self.scenes.active_id() {
                    self.scenes
                        .activate(engine.settings.selected_scene, &engine.scene_ctx());
                    self.dragging = false;
                }

                // Advance the light and the scene by the time since the last
                // frame. This is after the menu, so an angle just set from a
                // slider isn't undone
                let now = Instant::now();
                let dt = now.duration_since(self.last_frame_time).as_secs_f32();
                self.last_frame_time = now;
                engine.settings.controls.advance(dt);
                let scene_id = self.scenes.active_id();
                let (scene, orbit) = self.scenes.active_with_orbit();
                scene.apply_settings(&engine.settings.scene);
                scene.update(&engine.scene_ctx(), dt);

                let auto_capture = self
                    .options
                    .capture_after
                    .is_some_and(|s| self.started_at.elapsed() >= Duration::from_secs(s));
                if auto_capture {
                    gui.capture_requested = true;
                }
                // Take a screenshot if the button was clicked. It submits its
                // own commands, so it has to come before this frame's uniform
                // writes below, which would otherwise be picked up by the
                // screenshot's submit
                if std::mem::take(&mut gui.capture_requested) {
                    gui.status = match engine.save_screenshot(scene, scene_id, orbit) {
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
                // scene is centred in the part of the window the menu doesn't
                // cover
                engine.post.write_uniform(
                    &engine.queue,
                    &engine.settings.palette,
                    1,
                    engine.settings.levels,
                    engine.settings.bayer_density,
                    engine.settings.shared_grid,
                );
                let width = engine.surface_config.width as f32;
                let aspect_ratio = width / engine.surface_config.height as f32;
                let ndc_offset_x = gui.menu_width_px / width;
                engine.prepare_frame(scene, orbit, aspect_ratio, ndc_offset_x);

                let prepare_time = benchmark::elapsed(benchmark_start);
                // Acquire the next frame for rendering
                let frame = engine.surface.as_ref().unwrap().get_current_texture();

                // Return the surface texture if frame acquisition is successful
                let surface_texture = match frame {
                    CurrentSurfaceTexture::Success(frame) => frame,
                    CurrentSurfaceTexture::Suboptimal(frame) => frame,
                    // Discard other cases (Timeout, Occluded, Outdated) for now
                    _ => {
                        eprintln!("Failed to acquire next surface texture: {:?}", frame);
                        return;
                    }
                };

                // Render passes need a view, not the raw texture, so create one
                // covering the whole thing
                let surface_view = surface_texture
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());

                let encode_start = self.benchmark.as_ref().map(|_| Instant::now());
                // Start recording GPU commands (nothing actually runs until we
                // submit this later)
                let mut encoder = engine
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
                // Fire any finished timestamp readbacks without blocking
                let _ = engine.device.poll(wgpu::PollType::Poll);
                if let Some(timer) = &mut engine.gpu_timer {
                    timer.begin(&mut encoder);
                }
                if let Some(timer) = &engine.pass_timer {
                    timer.mark(&mut encoder, 0);
                }

                // Scene pass: draw the lit scene into the offscreen scene
                // texture
                engine.encode_scene_pass(&mut encoder, &engine.scene_view, &engine.depth_view, scene, scene_id);

                // Post pass: dither the scene texture into the surface
                if let Some(timer) = &engine.pass_timer {
                    timer.mark(&mut encoder, 2);
                }
                engine.encode_post_pass(&mut encoder, &engine.post.bind_group, &surface_view);
                if let Some(timer) = &engine.pass_timer {
                    timer.mark(&mut encoder, 3);
                }

                // Menu pass: draw the side menu over the dithered image
                gui.draw(
                    &engine.device,
                    &engine.queue,
                    &mut encoder,
                    &surface_view,
                    [engine.surface_config.width, engine.surface_config.height],
                    gui_output,
                );

                if let Some(timer) = &engine.pass_timer {
                    timer.mark(&mut encoder, 4);
                    timer.resolve(&mut encoder);
                }
                if let Some(timer) = &mut engine.gpu_timer {
                    timer.end(&mut encoder);
                }
                // Submit the recorded commands to the GPU queue
                engine.queue.submit(Some(encoder.finish()));
                if let Some(timer) = &mut engine.gpu_timer {
                    timer.submitted();
                }
                let encode_time = benchmark::elapsed(encode_start);
                if let Some(benchmark) = &mut self.benchmark {
                    let gpu_wait_start = Instant::now();
                    engine
                        .device
                        .poll(wgpu::PollType::wait_indefinitely())
                        .expect("Benchmark GPU wait failed");
                    let gpu_wait = gpu_wait_start.elapsed();
                    if let Some(timer) = &mut engine.pass_timer {
                        timer.read(&engine.device);
                    }
                    if benchmark.record(prepare_time, encode_time, prepare_time + encode_time + gpu_wait) {
                        if let Some(timer) = &engine.pass_timer {
                            timer.report();
                        }
                        eprintln!(
                            "BENCH shadow_maps_rendered={} (including warmup)",
                            engine.shadows.rendered_maps.get()
                        );
                        event_loop.exit();
                    }
                }
                engine.queue.present(surface_texture);

                // Update the FPS counter and usage stats overlay once a second
                self.fps_frame_count += 1;
                let since_last_update = self.fps_last_update.elapsed();
                if since_last_update >= Duration::from_secs(1) {
                    let fps = self.fps_frame_count as f32 / since_last_update.as_secs_f32();
                    gui.stats_text = self.usage_stats.sample(fps, &engine.device, engine.gpu_timer.as_mut());
                    self.fps_frame_count = 0;
                    self.fps_last_update = Instant::now();
                }
            }
            // Fall through for other events
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            if self.benchmark.is_some() || self.fps_limit.is_none() || Instant::now() >= self.next_frame_time {
                window.request_redraw();
            } else {
                // Wait without blocking input or spinning between frames.
                event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame_time));
            }
        }
    }
}
