//! One simulation and canvas, presented through a click-through surface per output.
mod layout;
mod present;

use crate::{
    cli::Cli,
    engine::{Engine, camera::Orbit, timing},
    scenes::Scenes,
};
use layout::{Desktop, Rect};
use present::{Canvas, Mapping, Presenter};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use reflecdith_control::{
    Settings, WallpaperMode,
    ipc::{self, Output, SharedStore, Store},
    settings,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry,
    output::{OutputHandler, OutputInfo, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
        },
    },
};
use std::{
    error::Error,
    ptr::NonNull,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_surface},
};

pub fn run(cli: Cli, scenes: Scenes) -> Result<(), Box<dyn Error>> {
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, stopped.clone())?;
    }
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<Wallpaper>(&conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)?;
    let shell = LayerShell::bind(&globals, &qh)
        .map_err(|_| "This compositor does not support wlr-layer-shell wallpaper surfaces")?;
    let instance = wgpu::Instance::default();
    let mut app = Wallpaper {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        monitors: Vec::new(),
        canvas: None,
        engine: None,
        scenes,
        settings: Settings::default(),
        target: cli.output.clone(),
        reference: None,
        topology_dirty: true,
        dirty: true,
        frame_id: 0,
        generation: 0,
        next_frame: Instant::now(),
        last_frame: Instant::now(),
        revision: 0,
    };
    queue.roundtrip(&mut app)?;
    if cli.list_outputs {
        for output in app.outputs.outputs() {
            if let Some(info) = app.outputs.info(&output) {
                println!(
                    "{}\t{}\tscale {}\tposition {:?}\tsize {:?}",
                    info.name.unwrap_or_default(),
                    info.description.unwrap_or_default(),
                    info.scale_factor,
                    info.logical_position,
                    info.logical_size
                );
            }
        }
        return Ok(());
    }
    if let Some(target) = &cli.output {
        if !app
            .outputs
            .outputs()
            .any(|o| app.outputs.info(&o).is_some_and(|i| i.name.as_ref() == Some(target)))
        {
            return Err(format!("Output not found: {target} (use --list-outputs)").into());
        }
    }
    let config_path = settings::config_path()?;
    app.settings = settings::load(&config_path)?;
    if let Some(fps) = cli.fps {
        app.settings.fps = fps;
    }
    if cli.no_shadows {
        app.settings.shading.shadow_strength = 0.;
    }
    app.settings.validate()?;
    let initial_store = Store::wallpaper(app.settings.clone(), app.scenes.wallpaper_scenes())?;
    app.settings = initial_store.state.settings.clone();
    let store: SharedStore = Arc::new(Mutex::new(initial_store));
    let _service = ipc::serve(store.clone())?;
    app.reconcile(&compositor, &shell, &qh)?;
    let mut event_loop = calloop::EventLoop::<Wallpaper>::try_new()?;
    calloop_wayland_source::WaylandSource::new(conn.clone(), queue).insert(event_loop.handle())?;
    let result = (|| -> Result<(), Box<dyn Error>> {
        while !stopped.load(std::sync::atomic::Ordering::Relaxed) {
            let ready = app.monitors.iter().any(|m| m.configured && !m.closed && m.frame_ready);
            let wait = if ready && (app.dirty || !app.settings.paused) {
                app.next_frame
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(100))
            } else {
                Duration::from_millis(100)
            };
            event_loop.dispatch(Some(wait), &mut app)?;
            {
                let mut store = store.lock().unwrap();
                if store.stop {
                    break;
                }
                if store.state.revision != app.revision {
                    let angle = app.settings.controls.light_angle;
                    app.settings = store.state.settings.clone();
                    app.settings.controls.light_angle = angle;
                    app.revision = store.state.revision;
                    app.invalidate();
                }
                if let Some(id) = store.reset_scene.take() {
                    app.scenes.reset(id);
                    app.invalidate();
                }
                store.flush(&config_path, false)?;
            }
            if app.topology_dirty {
                app.reconcile(&compositor, &shell, &qh)?;
            }
            app.prepare_targets(&conn, &instance)?;
            app.publish_layout(&store);
            app.draw(&qh);
        }
        Ok(())
    })();
    let saved = store.lock().unwrap().flush(&config_path, true);
    // Raw-handle surfaces must die before their Wayland hosts and connection.
    app.monitors.clear();
    app.canvas.take();
    app.engine.take();
    result?;
    saved?;
    Ok(())
}

struct OutputGpu {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    presenter: Option<Presenter>,
}
struct Monitor {
    // Field order also guarantees the GPU surface drops before the layer role.
    gpu: Option<OutputGpu>,
    layer: LayerSurface,
    output: wl_output::WlOutput,
    name: String,
    rect: Rect,
    logical_size: (u32, u32),
    scale: i32,
    configured: bool,
    closed: bool,
    frame_ready: bool,
    presented: u64,
}
impl Monitor {
    fn pixel_size(&self) -> (u32, u32) {
        (
            self.logical_size.0.saturating_mul(self.scale as u32),
            self.logical_size.1.saturating_mul(self.scale as u32),
        )
    }
    fn update_info(&mut self, info: &OutputInfo) {
        let position = info.logical_position.unwrap_or(info.location);
        self.rect.x = position.0;
        self.rect.y = position.1;
        if let Some((width, height)) = info.logical_size {
            self.rect.width = width.max(1) as u32;
            self.rect.height = height.max(1) as u32;
        }
        self.scale = info.scale_factor.max(1);
        self.layer.wl_surface().set_buffer_scale(self.scale);
    }
}

struct Wallpaper {
    registry: RegistryState,
    outputs: OutputState,
    monitors: Vec<Monitor>,
    canvas: Option<Canvas>,
    engine: Option<Engine>,
    scenes: Scenes,
    settings: Settings,
    target: Option<String>,
    reference: Option<String>,
    topology_dirty: bool,
    dirty: bool,
    frame_id: u64,
    generation: u64,
    next_frame: Instant,
    last_frame: Instant,
    revision: u64,
}
impl Wallpaper {
    fn invalidate(&mut self) {
        self.dirty = true;
        self.next_frame = Instant::now();
    }
    fn desktop(&self) -> Option<Desktop> {
        Desktop::new(
            self.monitors
                .iter()
                .filter(|m| m.configured && !m.closed)
                .map(|m| m.rect),
        )
    }
    fn reconcile(
        &mut self,
        compositor: &CompositorState,
        shell: &LayerShell,
        qh: &QueueHandle<Self>,
    ) -> Result<(), Box<dyn Error>> {
        let available: Vec<_> = self
            .outputs
            .outputs()
            .filter_map(|output| {
                let info = self.outputs.info(&output)?;
                if self
                    .target
                    .as_ref()
                    .is_some_and(|name| info.name.as_ref() != Some(name))
                {
                    return None;
                }
                Some((output, info))
            })
            .collect();
        self.monitors
            .retain(|m| available.iter().any(|(output, _)| *output == m.output));
        for (output, info) in available {
            if let Some(monitor) = self.monitors.iter_mut().find(|m| m.output == output) {
                monitor.update_info(&info);
                continue;
            }
            let name = info.name.clone().unwrap_or_else(|| format!("output-{}", info.id));
            let surface = compositor.create_surface(qh);
            let region = Region::new(compositor)?;
            surface.set_input_region(Some(region.wl_region()));
            let layer = shell.create_layer_surface(qh, surface, Layer::Background, Some("reflecdith"), Some(&output));
            layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
            layer.set_size(0, 0);
            layer.set_exclusive_zone(-1);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            let mut monitor = Monitor {
                gpu: None,
                layer,
                output,
                name,
                rect: Rect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                logical_size: (1, 1),
                scale: 1,
                configured: false,
                closed: false,
                frame_ready: true,
                presented: 0,
            };
            monitor.update_info(&info);
            monitor.layer.commit();
            eprintln!(
                "Wallpaper output added: {} at ({}, {})",
                monitor.name, monitor.rect.x, monitor.rect.y
            );
            self.monitors.push(monitor);
        }
        self.monitors.sort_by(|a, b| a.name.cmp(&b.name));
        self.topology_dirty = false;
        self.invalidate();
        Ok(())
    }
    fn prepare_targets(&mut self, conn: &Connection, instance: &wgpu::Instance) -> Result<(), Box<dyn Error>> {
        let Some(first) = self.monitors.iter().position(|m| m.configured && !m.closed) else {
            return Ok(());
        };
        if self.engine.is_none() {
            let monitor = &mut self.monitors[first];
            let surface = gpu_surface(instance, conn, &monitor.layer)?;
            let size = monitor.pixel_size();
            let mut engine = Engine::from_surface(instance.clone(), surface, size.0, size.1, false);
            monitor.gpu = Some(OutputGpu {
                surface: engine.surface.take().unwrap(),
                config: engine.surface_config.clone(),
                presenter: None,
            });
            engine.settings = self.settings.clone();
            self.scenes.activate(self.settings.selected_scene, &engine.scene_ctx());
            self.engine = Some(engine);
        }
        if self.reference.as_ref().is_none_or(|name| {
            !self
                .monitors
                .iter()
                .any(|m| m.name == *name && m.configured && !m.closed)
        }) {
            self.reference = Some(self.monitors[first].name.clone());
        }
        let desktop = self.desktop().unwrap();
        let reference = self
            .monitors
            .iter()
            .find(|m| Some(&m.name) == self.reference.as_ref())
            .unwrap();
        let (width, height) = if self.scenes.wallpaper_mode(self.settings.selected_scene) == WallpaperMode::Span {
            (desktop.width, desktop.height)
        } else {
            (reference.rect.width as f64, reference.rect.height as f64)
        };
        let scale = self
            .monitors
            .iter()
            .filter(|m| m.configured && !m.closed)
            .map(|m| m.scale)
            .max()
            .unwrap_or(1);
        let engine = self.engine.as_mut().unwrap();
        let size = layout::texture_size(width, height, scale, engine.device.limits().max_texture_dimension_2d);
        if self.canvas.as_ref().is_none_or(|canvas| canvas.size != size) {
            engine.resize(size.0, size.1);
            self.generation += 1;
            self.canvas = Some(Canvas::new(
                &engine.device,
                engine.surface_config.format,
                size,
                self.generation,
            ));
            self.dirty = true;
            self.next_frame = Instant::now();
        }
        let canvas = self.canvas.as_ref().unwrap();
        for monitor in self.monitors.iter_mut().filter(|m| m.configured && !m.closed) {
            let size = monitor.pixel_size();
            if monitor.gpu.is_none() {
                let surface = gpu_surface(instance, conn, &monitor.layer)?;
                let caps = surface.get_capabilities(&engine.adapter);
                let format = caps
                    .formats
                    .iter()
                    .find(|f| **f == engine.surface_config.format)
                    .copied()
                    .or_else(|| caps.formats.first().copied())
                    .ok_or("A monitor is incompatible with the selected GPU")?;
                let config = wgpu::SurfaceConfiguration {
                    format,
                    width: size.0,
                    height: size.1,
                    alpha_mode: *caps.alpha_modes.first().ok_or("Monitor has no alpha mode")?,
                    ..engine.surface_config.clone()
                };
                surface.configure(&engine.device, &config);
                monitor.gpu = Some(OutputGpu {
                    surface,
                    config,
                    presenter: None,
                });
            }
            let gpu = monitor.gpu.as_mut().unwrap();
            if gpu.config.width != size.0 || gpu.config.height != size.1 {
                gpu.config.width = size.0;
                gpu.config.height = size.1;
                gpu.surface.configure(&engine.device, &gpu.config);
                monitor.presented = 0;
                self.dirty = true;
            }
            if gpu.presenter.is_none() {
                gpu.presenter = Some(Presenter::new(&engine.device, gpu.config.format, canvas));
            }
        }
        Ok(())
    }
    fn publish_layout(&self, shared: &SharedStore) {
        let mut store = shared.lock().unwrap();
        store.state.outputs = self
            .monitors
            .iter()
            .filter(|m| m.configured && !m.closed)
            .map(|m| Output {
                name: m.name.clone(),
                x: m.rect.x,
                y: m.rect.y,
                width: m.rect.width,
                height: m.rect.height,
                scale: m.scale,
            })
            .collect();
        store.state.output = store
            .state
            .outputs
            .iter()
            .map(|o| o.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        store.state.target_output = self.target.clone();
        store.state.wallpaper_mode = self.scenes.wallpaper_mode(self.settings.selected_scene);
    }
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let desktop = self.desktop();
        let Some(engine) = &mut self.engine else {
            return;
        };
        let Some(canvas) = &self.canvas else {
            return;
        };
        let ready = self.monitors.iter().any(|m| m.configured && !m.closed && m.frame_ready);
        let now = Instant::now();
        let render = ready && now >= self.next_frame && (self.dirty || !self.settings.paused);
        if render {
            let dt = if self.settings.paused {
                0.
            } else {
                now.duration_since(self.last_frame).as_secs_f32().min(0.1)
            };
            self.last_frame = now;
            self.next_frame = timing::next_deadline(
                self.next_frame,
                now,
                Duration::from_secs_f64(1. / self.settings.fps as f64),
            );
            if self.scenes.active_id() != self.settings.selected_scene {
                self.scenes.activate(self.settings.selected_scene, &engine.scene_ctx());
            }
            self.settings.controls.advance(dt);
            engine.settings = self.settings.clone();
            let id = self.scenes.active_id();
            let (scene, orbit) = self.scenes.active_with_orbit();
            scene.apply_settings(&self.settings.scene);
            let aspect = canvas.size.0 as f32 / canvas.size.1 as f32;
            let automatic = scene.fit_wallpaper(*orbit, aspect);
            let orbit = self
                .settings
                .camera
                .as_ref()
                .map(Orbit::from_settings)
                .unwrap_or(automatic);
            scene.update(&engine.scene_ctx(), dt);
            engine.prepare_frame(scene, &orbit, aspect, 0.);
            engine.post.write_uniform(
                &engine.queue,
                &self.settings.palette,
                1,
                self.settings.levels,
                self.settings.bayer_density,
                self.settings.shared_grid,
            );
            let mut encoder = engine.device.create_command_encoder(&Default::default());
            engine.encode_scene_pass(&mut encoder, &engine.scene_view, &engine.depth_view, scene, id);
            engine.encode_post_pass(&mut encoder, &engine.post.bind_group, &canvas.view);
            engine.queue.submit(Some(encoder.finish()));
            self.frame_id += 1;
            self.dirty = false;
        } else if self.settings.paused {
            self.last_frame = now;
        }
        if self.frame_id == 0 {
            return;
        }
        for monitor in self
            .monitors
            .iter_mut()
            .filter(|m| m.configured && !m.closed && m.frame_ready)
        {
            if monitor.presented == self.frame_id {
                continue;
            }
            let Some(gpu) = monitor.gpu.as_mut() else {
                continue;
            };
            let texture = match gpu.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
                wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                    gpu.surface.configure(&engine.device, &gpu.config);
                    continue;
                }
                other => {
                    eprintln!("Wallpaper {}: {other:?}", monitor.name);
                    continue;
                }
            };
            let spanning = self.scenes.wallpaper_mode(self.settings.selected_scene) == WallpaperMode::Span;
            let size = (gpu.config.width, gpu.config.height);
            let mapping = Mapping {
                crop: if spanning {
                    desktop.unwrap().crop(monitor.rect)
                } else {
                    [0., 0., 1., 1.]
                },
                destination: if spanning {
                    [0., 0., 1., 1.]
                } else {
                    layout::fit(canvas.size, size)
                },
                target: [size.0 as f32, size.1 as f32, 0., 0.],
                dark: [
                    self.settings.palette.dark[0],
                    self.settings.palette.dark[1],
                    self.settings.palette.dark[2],
                    1.,
                ],
            };
            let view = texture.texture.create_view(&Default::default());
            let mut encoder = engine.device.create_command_encoder(&Default::default());
            gpu.presenter
                .as_mut()
                .unwrap()
                .draw(&engine.device, &engine.queue, canvas, mapping, &mut encoder, &view);
            engine.queue.submit(Some(encoder.finish()));
            monitor.layer.wl_surface().frame(qh, monitor.layer.wl_surface().clone());
            monitor.frame_ready = false;
            engine.queue.present(texture);
            monitor.presented = self.frame_id;
        }
    }
}

fn gpu_surface(
    instance: &wgpu::Instance,
    conn: &Connection,
    layer: &LayerSurface,
) -> Result<wgpu::Surface<'static>, Box<dyn Error>> {
    let raw_display_handle = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
        NonNull::new(conn.backend().display_ptr() as *mut _).ok_or("Missing Wayland display")?,
    ));
    let raw_window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(
        NonNull::new(layer.wl_surface().id().as_ptr() as *mut _).ok_or("Missing Wayland surface")?,
    ));
    // SAFETY: every OutputGpu is owned before its layer in Monitor and explicitly
    // dropped at shutdown, while the shared connection remains alive in run().
    Ok(unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(raw_display_handle),
            raw_window_handle,
        })?
    })
}

impl CompositorHandler for Wallpaper {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if let Some(m) = self.monitors.iter_mut().find(|m| m.layer.wl_surface() == surface) {
            m.scale = factor.max(1);
            m.layer.wl_surface().set_buffer_scale(m.scale);
        }
        self.invalidate();
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        if let Some(m) = self.monitors.iter_mut().find(|m| m.layer.wl_surface() == surface) {
            m.frame_ready = true;
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}
impl LayerShellHandler for Wallpaper {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if let Some(m) = self.monitors.iter_mut().find(|m| m.layer == *layer) {
            m.gpu.take();
            m.closed = true;
            m.configured = false;
        }
        self.invalidate();
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        if configure.new_size.0 > 0 && configure.new_size.1 > 0 {
            if let Some(m) = self.monitors.iter_mut().find(|m| m.layer == *layer) {
                m.logical_size = configure.new_size;
                m.rect.width = configure.new_size.0;
                m.rect.height = configure.new_size.1;
                m.configured = true;
            }
            self.invalidate();
        }
    }
}
impl OutputHandler for Wallpaper {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.topology_dirty = true;
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.topology_dirty = true;
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.topology_dirty = true;
    }
}
impl ProvidesRegistryState for Wallpaper {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}
delegate_registry!(Wallpaper);
delegate_compositor!(Wallpaper);
delegate_output!(Wallpaper);
delegate_layer!(Wallpaper);
