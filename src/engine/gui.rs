use super::{camera, post, post::Palette};
use crate::scenes::Scene;

use std::ops::RangeInclusive;

use egui::{Context, FontDefinitions, FontFamily, FullOutput, TextureId, ViewportId};
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use wgpu::{CommandEncoder, Device, Queue, StoreOp, TextureFormat, TextureView};
use winit::{event::WindowEvent, window::Window};

// Width of the side menu in points
const PANEL_WIDTH: f32 = 300.0;

// Width in points of the number box beside each slider, the same for all of
// them whatever their units
const VALUE_WIDTH: f32 = 70.0;

// Width in points of the label column every control row starts with, so the
// controls of all sections line up. Fits the longest label, "Bayer density"
const LABEL_WIDTH: f32 = 110.0;

// The rotation of the light about the world Y axis. The angle advances at its
// speed every frame
pub struct LightControls {
    // Angle in degrees
    pub light_angle: f32,
    // Speed in degrees per second
    pub light_speed: f32,
}

impl Default for LightControls {
    fn default() -> Self {
        Self {
            light_angle: 0.0,
            light_speed: camera::LIGHT_ROT_VEL.to_degrees(),
        }
    }
}

impl LightControls {
    pub fn reset_light(&mut self) {
        self.light_angle = 0.;
        self.light_speed = 0.;
    }

    // Move the angle on by dt seconds, wrapped to 0 to 360
    pub fn advance(&mut self, dt: f32) {
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
    pub bayer_density: f32,
    pub shared_grid: bool,
    // Light animation state; the menu exposes its speed
    pub controls: LightControls,
    // The specular and floor settings being edited
    pub shading: camera::Shading,
    // Index of the scene picked in the View row. The caller switches scenes
    // when it differs from the active one
    pub selected_scene: usize,
    // Width in physical pixels of the strip on the left of the window which
    // the menu covers, as of the last run
    pub menu_width_px: f32,
    // Set when the capture button is clicked, and cleared by the caller once
    // it has taken the screenshot
    pub capture_requested: bool,
    // The result of the last capture, shown under the button
    pub status: String,
    pub visible: bool,
    // FPS and resource usage, drawn in the top-left corner of the view
    pub stats_text: String,
}

// A row of controls after a label in the fixed-width label column
pub fn labeled_row<R>(ui: &mut egui::Ui, label: &str, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        let size = egui::vec2(LABEL_WIDTH, ui.spacing().interact_size.y);
        ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_size(size);
            ui.label(label);
        });
        add_contents(ui)
    })
    .inner
}

// A labelled slider followed by a number box of a fixed width, the slider
// stretched so the box sits at the right edge. The two edit the same value,
// shown with the given number of decimal places
pub fn value_slider<N: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut N,
    range: RangeInclusive<N>,
    decimals: usize,
    suffix: &str,
) {
    labeled_row(ui, label, |ui| {
        ui.spacing_mut().slider_width = ui.available_width() - VALUE_WIDTH - ui.spacing().item_spacing.x;
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

// A labelled checkbox, the box in the control column
fn checkbox_row(ui: &mut egui::Ui, label: &str, value: &mut bool) {
    labeled_row(ui, label, |ui| ui.checkbox(value, ""));
}

// A labelled dropdown filling the control column
pub fn combo_row(
    ui: &mut egui::Ui,
    label: &str,
    id: &str,
    selected_text: String,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    labeled_row(ui, label, |ui| {
        egui::ComboBox::from_id_salt(id)
            .width(ui.available_width())
            .truncate()
            .selected_text(selected_text)
            .show_ui(ui, add_contents);
    });
}

// The stats text in the top-left corner of the view, right of the menu, in
// the palette's light colour. It ignores the pointer so it never blocks
// orbiting
fn stats_overlay(ui: &egui::Ui, text: &str, palette: &Palette, left: f32) {
    if text.is_empty() {
        return;
    }
    // The palette is linear RGB, as the colour buttons edit it
    let [r, g, b] = palette.light;
    let colour = egui::Color32::from(egui::Rgba::from_rgb(r, g, b));
    egui::Area::new(egui::Id::unique("stats_overlay"))
        .order(egui::Order::Background)
        .interactable(false)
        .fixed_pos(egui::pos2(left + 8.0, 8.0))
        .show(ui.ctx(), |ui| {
            ui.label(egui::RichText::new(text).monospace().color(colour));
        });
}

impl Gui {
    pub fn new(device: &Device, window: &Window, surface_format: TextureFormat) -> Self {
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
            bayer_density: post::DEFAULT_BAYER_DENSITY,
            shared_grid: true,
            controls: LightControls::default(),
            shading: camera::Shading::default(),
            selected_scene: 0,
            menu_width_px: 0.0,
            capture_requested: false,
            status: String::new(),
            visible: true,
            stats_text: String::new(),
        }
    }

    // Pass a window event on to egui. Returns whether egui used the event, for
    // example a click on the menu, so it shouldn't also move the camera
    pub fn on_window_event(&mut self, window: &Window, event: &WindowEvent) -> bool {
        let response = self.state.on_window_event(window, event);
        self.visible && response.consumed
    }

    // Lay out the menu for this frame, which may edit the palette and the
    // active scene's settings. scene_names lists every scene, for the View
    // row. Returns what egui wants drawn, to be passed to draw
    pub fn run(&mut self, window: &Window, scene_names: &[&str], scene: &mut dyn Scene) -> FullOutput {
        let input = self.state.take_egui_input(window);
        let palette = &mut self.palette;
        let levels = &mut self.levels;
        let bayer_density = &mut self.bayer_density;
        let shared_grid = &mut self.shared_grid;
        let controls = &mut self.controls;
        let shading = &mut self.shading;
        let selected_scene = &mut self.selected_scene;
        let has_floor = scene.has_floor();
        let capture_requested = &mut self.capture_requested;
        let status = &self.status;
        let visible = &mut self.visible;
        let stats_text = &self.stats_text;
        let mut menu_width_points = 0.0;
        let mut output = self.context.run_ui(input, |ui| {
            if !*visible {
                stats_overlay(ui, stats_text, palette, 0.0);
                return;
            }
            let panel = egui::Panel::left("side_menu")
                .resizable(false)
                .exact_size(PANEL_WIDTH)
                .show(ui, |ui| {
                    // The window can be small, so let the menu scroll rather
                    // than be cut off
                    // A solid scroll bar takes its own space, rather than
                    // floating over the number boxes at the
                    // right edge
                    ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if ui.button("Hide controls").clicked() {
                            *visible = false;
                        }
                        ui.separator();
                        ui.heading("Scene");
                        labeled_row(ui, "View", |ui| {
                            for (index, name) in scene_names.iter().enumerate() {
                                ui.selectable_value(selected_scene, index, *name);
                            }
                        });
                        // The active scene's own settings
                        scene.ui(ui);
                        ui.separator();

                        ui.heading("Lighting");
                        value_slider(ui, "Shadows", &mut shading.shadow_strength, 0.0..=1.0, 2, "");
                        value_slider(ui, "Rotation", &mut controls.light_speed, -180.0..=180.0, 0, "°/s");
                        value_slider(ui, "Shininess", &mut shading.shininess, 1.0..=64.0, 0, "");
                        value_slider(ui, "Specular", &mut shading.specular_strength, 0.0..=3.0, 2, "");
                        if ui.button("Reset lighting").clicked() {
                            controls.reset_light();
                            let defaults = camera::Shading::default();
                            shading.shininess = defaults.shininess;
                            shading.specular_strength = defaults.specular_strength;
                            shading.shadow_strength = defaults.shadow_strength;
                        }
                        ui.separator();

                        if has_floor {
                            ui.heading("Floor");
                            // Lit floor peaks at about 0.68 x brightness at the
                            // centre, so the range runs
                            // past 1.0 to reach the light tone there
                            value_slider(ui, "Brightness", &mut shading.floor_brightness, 0.0..=2.0, 2, "");
                            value_slider(ui, "Fade start", &mut shading.floor_fade_start, 0.0..=6.0, 1, "");
                            value_slider(
                                ui,
                                "Fade end",
                                &mut shading.floor_fade_end,
                                0.0..=super::vertices::FLOOR_HALF_EXTENT,
                                1,
                                "",
                            );
                            if ui.button("Reset floor").clicked() {
                                let defaults = camera::Shading::default();
                                shading.floor_brightness = defaults.floor_brightness;
                                shading.floor_fade_start = defaults.floor_fade_start;
                                shading.floor_fade_end = defaults.floor_fade_end;
                            }
                            ui.separator();
                        }

                        ui.heading("Dithering");
                        checkbox_row(ui, "Shared grid", shared_grid);
                        value_slider(ui, "Levels", levels, 2..=post::MAX_LEVELS, 0, "");
                        value_slider(ui, "Bayer density", bayer_density, 0.125..=1.0, 2, "×");
                        if has_floor {
                            value_slider(
                                ui,
                                "Floor density",
                                &mut shading.floor_noise_density,
                                16.0..=256.0,
                                0,
                                "",
                            );
                        }
                        if ui.button("Reset dithering").clicked() {
                            *levels = post::DEFAULT_LEVELS;
                            *bayer_density = post::DEFAULT_BAYER_DENSITY;
                            *shared_grid = true;
                            shading.floor_noise_density = camera::Shading::default().floor_noise_density;
                        }
                        ui.separator();

                        ui.heading("Colours");
                        ui.horizontal(|ui| {
                            ui.label("Dark");
                            ui.color_edit_button_rgb(&mut palette.dark);
                            ui.label("Light");
                            ui.color_edit_button_rgb(&mut palette.light);
                        });
                        // Two equal-width buttons spanning the menu
                        ui.columns(2, |columns| {
                            let size = [columns[0].available_width(), columns[0].spacing().interact_size.y];
                            if columns[0].add_sized(size, egui::Button::new("Random")).clicked() {
                                *palette = Palette::random_muted();
                            }
                            if columns[1].add_sized(size, egui::Button::new("Swap")).clicked() {
                                std::mem::swap(&mut palette.dark, &mut palette.light);
                            }
                        });
                        ui.separator();

                        ui.heading("Capture");
                        if ui.button("Save screenshot").clicked() {
                            *capture_requested = true;
                        }
                        if !status.is_empty() {
                            ui.add(egui::Label::new(status).truncate().show_tooltip_when_elided(false));
                        }
                    });
                });

            // The space the whole panel takes, including its margins
            menu_width_points = panel.response.rect.width();
            stats_overlay(ui, stats_text, palette, menu_width_points);
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
