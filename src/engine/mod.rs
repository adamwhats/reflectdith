//! The renderer shared by every scene: wgpu setup, the shadow, scene, floor
//! and dither passes, the side menu and screenshots. Scenes plug into it
//! through the Scene trait.
pub mod benchmark;
pub mod blue_noise;
pub mod camera;
pub mod capture;
pub mod gui;
pub mod pipeline;
pub mod post;
#[cfg(test)]
pub(crate) mod render_tests;
pub mod shadows;
pub mod stats;
pub mod textures;
pub mod vertices;

use std::sync::Arc;

use reflecdith_control::Settings;
use wgpu::{
    Device, Instance, Queue, RenderPipeline, ShaderModuleDescriptor, ShaderSource, StoreOp, Surface, SurfaceColorSpace,
    SurfaceConfiguration, TextureView,
};
use winit::window::Window;

use crate::scenes::{Scene, SceneCtx};

// Holds the wgpu context and everything the passes draw with, apart from the
// scene's own geometry
pub struct Engine {
    world_noise: wgpu::BindGroup,
    pub shadows: shadows::Shadows,
    // The layout of the camera transform bind group, which scenes use for
    // their own per-draw transforms
    camera_layout: wgpu::BindGroupLayout,
    // Device is the handle used to interact with the physical GPU (the Adapter)
    pub device: Device,
    // Queue is where we submit commands to be sent to the device for execution
    pub queue: Queue,
    // The viewer owns its window surface here; wallpaper mode moves surfaces
    // into its per-monitor hosts and uses this engine for the shared canvas.
    pub surface: Option<Surface<'static>>,
    pub adapter: wgpu::Adapter,
    // Surface configuration defines how the surface is configured for rendering
    pub surface_config: SurfaceConfiguration,
    // The floor under the scene, a flat square, and the pipeline which draws it
    floor_buffer: wgpu::Buffer,
    floor_pipeline: RenderPipeline,
    // The camera's model and view-projection matrix buffer and its bind group
    camera_transform: camera::CameraTransform,
    // The same for the floor, which has its own model matrix as it doesn't
    // move with the scene
    floor_camera_transform: camera::CameraTransform,
    // View of a texture storing per-pixel depth
    pub depth_view: TextureView,
    // View of the offscreen texture the scene is rendered into, which the
    // post-process pass then reads
    pub scene_view: TextureView,
    // The baked pipeline state for drawing the scene: shader stages, vertex
    // layout, and fixed-function GPU config, bound before every draw call
    scene_pipeline: RenderPipeline,
    // The pipeline and bind group for the pass which draws the scene texture
    // to the surface
    pub post: post::PostProcess,
    pub settings: Settings,
    // Measures GPU time per frame, when the adapter supports timestamps
    pub gpu_timer: Option<stats::GpuTimer>,
    // Per-pass GPU timings, in benchmark mode when timestamps are supported
    pub pass_timer: Option<benchmark::PassTimer>,
}

impl Engine {
    // Set up wgpu for the window. In benchmark mode the surface presents
    // immediately where it can, and the passes are timed
    pub fn new(window: Arc<Window>, benchmark: bool) -> Self {
        // Create a wgpu instance, entry point for the API
        let instance = Instance::default();

        // Match the adapter to the window and honor explicit wgpu overrides.
        let surface = instance
            .create_surface(Arc::clone(&window))
            .expect("Failed to create surface");
        let size = window.inner_size();
        Self::from_surface(instance, surface, size.width, size.height, benchmark)
    }

    pub fn from_surface(
        instance: Instance,
        surface: Surface<'static>,
        width: u32,
        height: u32,
        benchmark: bool,
    ) -> Self {
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
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: stats::gpu_timing_features(&adapter),
            ..Default::default()
        }))
        .expect("Failed to request device");
        let gpu_timer = stats::GpuTimer::new(&device, &queue);
        let pass_timer = benchmark.then(|| benchmark::PassTimer::new(&device, &queue)).flatten();

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps.formats[0];

        // Define and apply the surface configuration
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: width,
            height: height,
            present_mode: if benchmark && surface_caps.present_modes.contains(&wgpu::PresentMode::Immediate) {
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
        let camera_layout = camera::create_camera_transform_bind_group_layout(&device);
        let camera_transform = camera::CameraTransform::new(
            &device,
            &camera_layout,
            &[0; std::mem::size_of::<camera::CameraUniform>()],
        );

        let floor_camera_transform = camera::CameraTransform::new(
            &device,
            &camera_layout,
            &[0; std::mem::size_of::<camera::CameraUniform>()],
        );

        // Create a depth texture view of the same size as the surface
        let depth_view = textures::create_depth_view(&device, width, height);

        // Create the offscreen scene texture view of the same size as the
        // surface, in the floating point scene format
        let scene_view = textures::create_scene_view(&device, width, height);

        // Describes what bind groups the scene pipeline expects
        let shadows = shadows::Shadows::new(&device, &camera_layout);
        let world_noise_layout = post::PostProcess::world_noise_layout(&device);
        let scene_pipeline_layout =
            pipeline::create_pipeline_layout(&device, &camera_layout, &world_noise_layout, &shadows.scene_layout);

        // Bake the shader stages, layout, and config into an
        // immutable pipeline object. The scene culls its back faces, and the
        // floor is visible from both sides
        let scene_pipeline = pipeline::create_pipeline(
            &device,
            &scene_shader_module,
            &scene_pipeline_layout,
            textures::SCENE_FORMAT,
            textures::DEPTH_FORMAT,
            "fragment_main",
            Some(wgpu::Face::Back),
        );
        let floor_pipeline = pipeline::create_pipeline(
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

        Self {
            world_noise,
            shadows,
            camera_layout,
            device,
            queue,
            surface: Some(surface),
            adapter,
            surface_config,
            floor_buffer,
            floor_pipeline,
            camera_transform,
            floor_camera_transform,
            depth_view,
            scene_view,
            scene_pipeline,
            post,
            settings: Settings::default(),
            gpu_timer,
            pass_timer,
        }
    }

    // What a scene is given to create and update its GPU resources
    pub fn scene_ctx(&self) -> SceneCtx<'_> {
        SceneCtx {
            device: &self.device,
            camera_layout: &self.camera_layout,
        }
    }

    // Resize the surface and the textures sized to it
    pub fn resize(&mut self, width: u32, height: u32) {
        self.surface_config.width = width;
        self.surface_config.height = height;
        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &self.surface_config);
        }

        // Recreate the textures sized to the surface
        self.depth_view = textures::create_depth_view(&self.device, width, height);
        self.scene_view = textures::create_scene_view(&self.device, width, height);

        // The post bind group points at the old scene view, so rebind it to
        // the new one
        self.post.set_scene_view(&self.device, &self.scene_view);
    }

    // Keep the camera above the scene's floor, if it has one, in the orbit's
    // frame
    pub fn constrain_orbit(&self, orbit: &camera::Orbit, scene: &dyn Scene) -> camera::Orbit {
        if !scene.has_floor() {
            return *orbit;
        }
        let view = scene.view();
        orbit.above_floor(view.camera_motion.inverse().transform_point3(view.floor_origin).y)
    }

    // Write this frame's camera, floor and light uniforms for the scene seen
    // from the orbit, and let the scene upload what depends on the camera
    pub fn prepare_frame(&self, scene: &mut dyn Scene, orbit: &camera::Orbit, aspect: f32, offset: f32) {
        let view = scene.view();
        let orbit = self.constrain_orbit(orbit, scene);
        let mut camera_uniform = camera::camera_uniform(
            &orbit,
            view.model,
            self.settings.controls.light_angle,
            &self.settings.shading,
            aspect,
            offset,
        )
        .follow(view.camera_motion);
        camera_uniform.fog = view.fog.extend(0.0).extend(0.0);
        self.queue
            .write_buffer(&self.camera_transform.buffer, 0, bytemuck::bytes_of(&camera_uniform));
        scene.prepare(&self.queue, &camera_uniform);
        self.queue.write_buffer(
            &self.floor_camera_transform.buffer,
            0,
            bytemuck::bytes_of(&camera_uniform.for_floor_at(view.floor_origin)),
        );
        self.shadows.update(
            &self.queue,
            &camera_uniform,
            view.floor_origin,
            self.settings.shading.shadow_strength,
        );
    }

    // Record the light's depth pass, if the scene's casters or the light have
    // changed since the map was last drawn
    fn encode_shadow_pass(&self, encoder: &mut wgpu::CommandEncoder, scene: &dyn Scene, scene_id: usize) {
        if self.settings.shading.shadow_strength <= 0.0 {
            return;
        }
        let casters = shadows::Casters {
            scene: scene_id,
            revision: scene.shadow_revision(),
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
        scene.draw_shadow(&mut pass);
    }

    // Record the scene pass: draw the lit scene, and the floor if it has one,
    // into the given colour and depth views, which are the same size as each
    // other. scene_id identifies the scene to the shadow cache
    pub fn encode_scene_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        colour_view: &TextureView,
        depth_view: &TextureView,
        scene: &dyn Scene,
        scene_id: usize,
    ) {
        self.encode_shadow_pass(encoder, scene, scene_id);
        if let Some(timer) = &self.pass_timer {
            timer.mark(encoder, 1);
        }
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

        // Bind the pipeline and the camera transform; the scene binds its own
        // vertex buffers
        render_pass.set_pipeline(&self.scene_pipeline);
        render_pass.set_bind_group(1, &self.world_noise, &[]);
        render_pass.set_bind_group(2, &self.shadows.scene_group, &[]);
        render_pass.set_bind_group(0, &self.camera_transform.bind_group, &[]);
        scene.draw(&mut render_pass);

        // Draw the floor the same way, with its own pipeline and camera
        if scene.has_floor() {
            render_pass.set_pipeline(&self.floor_pipeline);
            render_pass.set_vertex_buffer(0, self.floor_buffer.slice(..));
            render_pass.set_bind_group(0, &self.floor_camera_transform.bind_group, &[]);
            render_pass.draw(0..vertices::FLOOR_VERTEX_COUNT, 0..1);
        }
    }

    // Record the post pass: draw the scene texture, bound in bind_group, into
    // the target view with one fullscreen triangle
    pub fn encode_post_pass(
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
