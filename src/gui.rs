use crate::{camera, post, post::Palette};

use std::{
    ops::RangeInclusive,
    path::{Path, PathBuf},
};

use egui::{Context, FontDefinitions, FontFamily, FullOutput, TextureId, ViewportId};
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use wgpu::{CommandEncoder, Device, Queue, StoreOp, TextureFormat, TextureView};
use winit::{event::WindowEvent, window::Window};

// Width of the side menu in points
const PANEL_WIDTH: f32 = 300.0;

// Width in points of the number box beside each slider, the same for all of
// them whatever their units
const VALUE_WIDTH: f32 = 70.0;

// The rotation of the model and the light about the world Y axis. Each angle
// advances at its speed every frame; only light speed is exposed in the menu
pub struct SceneControls {
    // Angles in degrees
    pub model_angle: f32,
    pub light_angle: f32,
    // Speeds in degrees per second
    pub model_speed: f32,
    pub light_speed: f32,
}

impl Default for SceneControls {
    fn default() -> Self {
        Self {
            model_angle: 0.0,
            light_angle: 0.0,
            model_speed: camera::ROT_VEL.to_degrees(),
            light_speed: camera::LIGHT_ROT_VEL.to_degrees(),
        }
    }
}

impl SceneControls {
    pub fn reset_light(&mut self) {
        self.light_angle = 0.;
        self.light_speed = 0.;
    }

    // Move both angles on by dt seconds, wrapped to 0 to 360
    pub fn advance(&mut self, dt: f32) {
        self.model_angle = (self.model_angle + self.model_speed * dt).rem_euclid(360.0);
        self.light_angle = (self.light_angle + self.light_speed * dt).rem_euclid(360.0);
    }
}

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
    // The number of tones the dither quantises to, from the dark colour to the
    // light colour
    pub levels: u32,
    // Animation state; the menu exposes light speed
    pub controls: SceneControls,
    // The specular and floor settings being edited
    pub shading: camera::Shading,
    // The models which can be picked from the dropdown, and the index of the
    // one picked. The caller loads the model when the index changes
    models: Vec<PathBuf>,
    pub selected_model: usize,
    // Width in physical pixels of the strip on the left of the window which
    // the menu covers, as of the last run
    pub menu_width_px: f32,
    // Set when the capture button is clicked, and cleared by the caller once
    // it has taken the screenshot
    pub capture_requested: bool,
    // The result of the last capture, shown under the button
    pub status: String,
    pub robot_status: Option<String>,
    pub visible: bool,
    pub topics: Option<crate::topics::TopicPanel>,
}

// A slider followed by a number box of a fixed width, so the boxes line up
// whatever the value or suffix. The two edit the same value, shown with the
// given number of decimal places
fn value_slider<N: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    value: &mut N,
    range: RangeInclusive<N>,
    decimals: usize,
    suffix: &str,
) {
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(value, range.clone()).show_value(false));
        ui.add_sized(
            [VALUE_WIDTH, ui.spacing().interact_size.y],
            egui::DragValue::new(value)
                .range(range)
                .fixed_decimals(decimals)
                .suffix(suffix),
        );
    });
}

// The name shown in the dropdown for a model file
fn model_name(path: &Path) -> String {
    path.file_stem().unwrap_or_default().to_string_lossy().into_owned()
}

impl Gui {
    pub fn new(
        device: &Device,
        window: &Window,
        surface_format: TextureFormat,
        models: Vec<PathBuf>,
        selected_model: usize,
    ) -> Self {
        let context = Context::default();

        // Use egui's bundled monospace font (Hack) for all text by putting it
        // first in the proportional family too
        let mut fonts = FontDefinitions::default();
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "Hack".to_owned());
        context.set_fonts(fonts);

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
            levels: post::DEFAULT_LEVELS,
            controls: SceneControls::default(),
            shading: camera::Shading::default(),
            models,
            selected_model,
            menu_width_px: 0.0,
            capture_requested: false,
            status: String::new(),
            robot_status: None,
            visible: true,
            topics: None,
        }
    }

    // The file of the model picked in the dropdown
    pub fn selected_model_path(&self) -> &Path {
        &self.models[self.selected_model]
    }

    // Pass a window event on to egui. Returns whether egui used the event, for
    // example a click on the menu, so it shouldn't also move the camera
    pub fn on_window_event(&mut self, window: &Window, event: &WindowEvent) -> bool {
        let response = self.state.on_window_event(window, event);
        self.visible && response.consumed
    }

    // Lay out the menu for this frame, which may edit the palette. Returns
    // what egui wants drawn, to be passed to draw
    pub fn run(&mut self, window: &Window) -> FullOutput {
        let input = self.state.take_egui_input(window);
        let palette = &mut self.palette;
        let levels = &mut self.levels;
        let controls = &mut self.controls;
        let shading = &mut self.shading;
        let models = &self.models;
        let selected_model = &mut self.selected_model;
        let capture_requested = &mut self.capture_requested;
        let status = &self.status;
        let robot_status = &self.robot_status;
        let visible = &mut self.visible;
        let topics = &mut self.topics;
        let mut menu_width_points = 0.0;
        let mut output = self.context.run_ui(input, |ui| {
            if !*visible {
                return;
            }
            let panel = egui::Panel::left("side_menu")
                .resizable(false)
                .default_size(PANEL_WIDTH)
                .show(ui, |ui| {
                    // The window can be small, so let the menu scroll rather than
                    // be cut off
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if ui.button("Hide controls (Tab)").clicked() {
                            *visible = false;
                        }
                        ui.separator();
                        if let Some(topics) = topics {
                            topics.ui(ui);
                        }
                        // Each section is a grid with the labels in one column and
                        // the controls in the next, so they line up
                        if let Some(status) = robot_status {
                            ui.label(status);
                        }
                        if robot_status.is_none() {
                            ui.heading("Model");
                            egui::Grid::new("model_grid").show(ui, |ui| {
                                ui.label("Mesh");
                                egui::ComboBox::from_id_salt("model_select")
                                    .selected_text(model_name(&models[*selected_model]))
                                    .show_ui(ui, |ui| {
                                        for (index, model) in models.iter().enumerate() {
                                            ui.selectable_value(selected_model, index, model_name(model));
                                        }
                                    });
                                ui.end_row();
                            });
                            ui.separator();
                        }
                        ui.heading("Lighting");
                        egui::Grid::new("light_grid").show(ui, |ui| {
                            ui.label("Light speed");
                            value_slider(ui, &mut controls.light_speed, -180.0..=180.0, 0, "°/s");
                            ui.end_row();
                        });
                        if ui.button("Reset light").clicked() {
                            controls.reset_light();
                        }
                        ui.separator();

                        ui.heading("Specular");
                        egui::Grid::new("specular_grid").show(ui, |ui| {
                            ui.label("Shininess");
                            value_slider(ui, &mut shading.shininess, 1.0..=64.0, 0, "");
                            ui.end_row();
                            ui.label("Strength");
                            value_slider(ui, &mut shading.specular_strength, 0.0..=3.0, 2, "");
                            ui.end_row();
                        });
                        ui.separator();

                        ui.heading("Floor");
                        ui.checkbox(&mut shading.shadows, "Shadows");
                        egui::Grid::new("floor_grid").show(ui, |ui| {
                            ui.label("Brightness");
                            value_slider(ui, &mut shading.floor_brightness, 0.0..=1.0, 2, "");
                            ui.end_row();
                            ui.label("Fade start");
                            value_slider(ui, &mut shading.floor_fade_start, 0.0..=6.0, 1, "");
                            ui.end_row();
                            ui.label("Fade end");
                            value_slider(
                                ui,
                                &mut shading.floor_fade_end,
                                0.0..=crate::vertices::FLOOR_HALF_EXTENT,
                                1,
                                "",
                            );
                            ui.end_row();
                        });
                        if ui.button("Reset shading").clicked() {
                            *shading = camera::Shading::default();
                        }
                        ui.separator();

                        ui.label("Floor noise density (world units)");
                        value_slider(ui, &mut shading.floor_noise_density, 16.0..=256.0, 0, "");
                        ui.separator();
                        ui.heading("Palette");
                        ui.horizontal(|ui| {
                            ui.label("Dark");
                            ui.color_edit_button_rgb(&mut palette.dark);
                            ui.label("Light");
                            ui.color_edit_button_rgb(&mut palette.light);
                        });
                        egui::Grid::new("levels_grid").show(ui, |ui| {
                            ui.label("Levels");
                            value_slider(ui, &mut *levels, 2..=16, 0, "");
                            ui.end_row();
                        });
                        if ui.button("Reset palette").clicked() {
                            *palette = Palette::default();
                            *levels = post::DEFAULT_LEVELS;
                        }
                        ui.separator();

                        ui.heading("Capture");
                        if ui.button("Save screenshot").clicked() {
                            *capture_requested = true;
                        }
                        if !status.is_empty() {
                            ui.label(status);
                        }
                    });
                });

            // The space the whole panel takes, including its margins
            menu_width_points = panel.response.rect.width();
        });
        self.menu_width_px = if self.visible {
            menu_width_points * output.pixels_per_point
        } else {
            0.0
        };

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
