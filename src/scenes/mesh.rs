//! A single STL model spinning on the floor, picked from the models directory.
use std::path::{Path, PathBuf};

use glam::{Mat4, Vec3};

use super::{Scene, SceneCtx, SceneView};
use crate::engine::{camera, gui, vertices};

// Directory the models are listed from, and the name (without extension) of
// the one shown at startup
const MODEL_DIR: &str = "models";
const DEFAULT_MODEL: &str = "utah_teapot_low";

pub struct MeshScene {
    // The models which can be picked from the dropdown, the index of the one
    // picked, and the index of the one loaded, which is reloaded when they
    // differ
    models: Vec<PathBuf>,
    selected_model: usize,
    loaded_model: usize,
    mesh: vertices::Mesh,
    // Rotation about the world Y axis in degrees, and its speed in degrees
    // per second
    angle: f32,
    speed: f32,
    // Changed whenever the mesh is reloaded or turns, for the shadow cache
    revision: u64,
}

// The name shown in the dropdown for a model file
fn model_name(path: &Path) -> String {
    path.file_stem().unwrap_or_default().to_string_lossy().into_owned()
}

impl MeshScene {
    // Find the available models and load the default one, falling back to the
    // first if the default isn't there
    pub fn new(ctx: &SceneCtx) -> Self {
        let models = vertices::list_models(MODEL_DIR);
        let loaded_model = models
            .iter()
            .position(|path| path.file_stem().is_some_and(|stem| stem == DEFAULT_MODEL))
            .unwrap_or(0);
        let mesh = vertices::create_mesh(ctx.device, &models[loaded_model]);
        Self {
            models,
            selected_model: loaded_model,
            loaded_model,
            mesh,
            angle: 0.0,
            speed: camera::ROT_VEL.to_degrees(),
            revision: 0,
        }
    }
}

impl Scene for MeshScene {
    fn update(&mut self, ctx: &SceneCtx, dt: f32) {
        // Load the mesh again if a different model was picked in the menu
        if self.selected_model != self.loaded_model {
            self.mesh = vertices::create_mesh(ctx.device, &self.models[self.selected_model]);
            self.loaded_model = self.selected_model;
            self.revision += 1;
        }
        let angle = (self.angle + self.speed * dt).rem_euclid(360.0);
        if angle != self.angle {
            self.angle = angle;
            self.revision += 1;
        }
    }

    // Spin around the world Y axis. The mesh is already centred and oriented
    // when loaded, so this is the only model transform
    fn view(&self) -> SceneView {
        SceneView {
            model: Mat4::from_rotation_y(self.angle.to_radians()),
            floor_origin: Vec3::new(0., self.mesh.min_y, 0.),
            ..Default::default()
        }
    }

    fn shadow_revision(&self) -> u64 {
        self.revision
    }

    fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        self.mesh.draw(pass);
    }

    fn draw_shadow<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        self.mesh.draw_shadow(pass);
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let models = &self.models;
        let selected_model = &mut self.selected_model;
        gui::combo_row(ui, "Mesh", "model_select", model_name(&models[*selected_model]), |ui| {
            for (index, model) in models.iter().enumerate() {
                ui.selectable_value(selected_model, index, model_name(model));
            }
        });
        gui::value_slider(ui, "Spin", &mut self.speed, -180.0..=180.0, 0, "°/s");
    }
}
