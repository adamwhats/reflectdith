use crate::post::Palette;

use egui::{Context, FullOutput, TextureId, ViewportId};
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use wgpu::{CommandEncoder, Device, Queue, StoreOp, TextureFormat, TextureView};
use winit::{event::WindowEvent, window::Window};

// Width of the side menu in points
const PANEL_WIDTH: f32 = 180.0;

// The side menu, drawn with egui on top of the dithered image
pub struct Gui {
    // egui's UI state, carried between frames
    context: Context,
    // Translates winit events into egui input
    state: egui_winit::State,
    // Draws egui's triangles with wgpu
    renderer: Renderer,
    // Textures egui released last frame, freed once that frame is submitted
    textures_to_free: Vec<TextureId>,
    // The colours being edited
    pub palette: Palette,
}

impl Gui {
    pub fn new(device: &Device, window: &Window, surface_format: TextureFormat) -> Self {
        let context = Context::default();
        let state = egui_winit::State::new(
            context.clone(),
            ViewportId::ROOT,
            window,
            Some(window.scale_factor() as f32),
            None,
            Some(device.limits().max_texture_dimension_2d as usize),
        );
        let renderer = Renderer::new(device, surface_format, RendererOptions::default());

        Self {
            context,
            state,
            renderer,
            textures_to_free: Vec::new(),
            palette: Palette::default(),
        }
    }

    // Pass a window event on to egui. The response says whether egui used the
    // event, which doesn't matter yet as nothing else handles input
    pub fn on_window_event(&mut self, window: &Window, event: &WindowEvent) {
        let _ = self.state.on_window_event(window, event);
    }

    // Lay out the menu for this frame, which may edit the palette. Returns
    // what egui wants drawn, to be passed to draw
    pub fn run(&mut self, window: &Window) -> FullOutput {
        let input = self.state.take_egui_input(window);
        let palette = &mut self.palette;
        let mut output = self.context.run_ui(input, |ui| {
            egui::Panel::left("side_menu")
                .resizable(false)
                .default_size(PANEL_WIDTH)
                .show(ui, |ui| {
                    ui.heading("Palette");
                    egui::Grid::new("palette_grid").show(ui, |ui| {
                        ui.label("Dark");
                        ui.color_edit_button_rgb(&mut palette.dark);
                        ui.end_row();
                        ui.label("Light");
                        ui.color_edit_button_rgb(&mut palette.light);
                        ui.end_row();
                    });
                    if ui.button("Reset").clicked() {
                        *palette = Palette::default();
                    }
                });
        });

        // Cursor changes, clipboard etc. are applied to the window here
        let platform_output = std::mem::take(&mut output.platform_output);
        self.state.handle_platform_output(window, platform_output);

        output
    }

    // Record a render pass drawing the menu on top of whatever is already in
    // the target view
    pub fn draw(
        &mut self,
        device: &Device,
        queue: &Queue,
        encoder: &mut CommandEncoder,
        target_view: &TextureView,
        size_in_pixels: [u32; 2],
        mut output: FullOutput,
    ) {
        // The previous frame has been submitted, so its released textures are
        // no longer needed by the GPU
        for id in self.textures_to_free.drain(..) {
            self.renderer.free_texture(&id);
        }
        // egui checks every texture change is handled, so drain rather than
        // just read them
        self.textures_to_free.extend(output.textures_delta.free.drain());

        // Upload new or changed textures, mostly the font atlas. Each texture
        // can have several partial updates queued
        for (id, image_deltas) in output.textures_delta.set.drain() {
            for image_delta in &image_deltas {
                self.renderer.update_texture(device, queue, id, image_delta);
            }
        }

        // Turn egui's shapes into triangles and upload them
        let paint_jobs = self.context.tessellate(output.shapes, output.pixels_per_point);
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels,
            pixels_per_point: output.pixels_per_point,
        };
        // Extra command buffers only come from custom paint callbacks, which
        // the menu doesn't use
        let _ = self
            .renderer
            .update_buffers(device, queue, encoder, &paint_jobs, &screen_descriptor);

        let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // Load rather than clear, to keep the image underneath
                    load: wgpu::LoadOp::Load,
                    store: StoreOp::Store,
                },
            })],
            ..Default::default()
        });

        // egui's renderer needs a 'static render pass. This only means the
        // encoder can't be touched until the pass is dropped at the end of
        // this function
        let mut render_pass = render_pass.forget_lifetime();
        self.renderer.render(&mut render_pass, &paint_jobs, &screen_descriptor);
    }
}
