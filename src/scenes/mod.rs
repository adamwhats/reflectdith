//! The experiments the engine can show. Each is a Scene: it owns its geometry,
//! simulation and menu settings, and tells the engine how to frame it. Adding
//! one means a new module here and a register call in main.
pub mod boids;
pub mod mesh;
pub mod robot;

use glam::{Mat4, Vec2, Vec3};

use crate::engine::camera::{CameraUniform, Orbit};

// What a scene is given to create and update its GPU resources
pub struct SceneCtx<'a> {
    pub device: &'a wgpu::Device,
    // Layout of the camera transform bind group, for scenes which draw with
    // their own per-draw transforms
    pub camera_layout: &'a wgpu::BindGroupLayout,
}

// How the engine should frame a scene this frame
#[derive(Clone, Copy)]
pub struct SceneView {
    // Model-to-world transform written into the camera uniform
    pub model: Mat4,
    // Moves the camera with the scene, for following a moving robot
    pub camera_motion: Mat4,
    // Where the floor is drawn, and what the light's shadow view is centred
    // above. With no floor it still places the shadow view
    pub floor_origin: Vec3,
    // Distances past the orbit centre where fog starts and finishes fading
    // surfaces to black. Zero turns it off
    pub fog: Vec2,
}

impl Default for SceneView {
    fn default() -> Self {
        Self {
            model: Mat4::IDENTITY,
            camera_motion: Mat4::IDENTITY,
            floor_origin: Vec3::ZERO,
            fog: Vec2::ZERO,
        }
    }
}

pub trait Scene {
    // Whether the engine draws the floor, and shows its menu settings
    fn has_floor(&self) -> bool {
        true
    }

    // Where the camera starts the first time the scene is shown
    fn default_orbit(&self) -> Orbit {
        Orbit::default()
    }

    // Called when the scene becomes the one shown, including the first time
    fn activate(&mut self, _ctx: &SceneCtx) {}

    // Advance the scene by dt seconds. Runs every frame while it is shown,
    // after the menu, so settings edited this frame apply straight away
    fn update(&mut self, _ctx: &SceneCtx, _dt: f32) {}

    // How to frame the scene. Called after update, and again on mouse input
    fn view(&self) -> SceneView {
        SceneView::default()
    }

    // Upload anything which depends on this frame's camera. Runs after
    // update, and again for a screenshot
    fn prepare(&mut self, _queue: &wgpu::Queue, _camera: &CameraUniform) {}

    // Changes whenever the shadow casters move or change shape. The light's
    // depth map is only redrawn when it, or the light, changes
    fn shadow_revision(&self) -> u64;

    // Draw into the scene pass, which has the scene pipeline and the camera
    // transform at group 0 bound
    fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>);

    // Draw into the light's depth pass. Only positions are read
    fn draw_shadow<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        self.draw(pass);
    }

    // The scene's own section of the side menu, under the View row
    fn ui(&mut self, _ui: &mut egui::Ui) {}
}

// Builds a scene the first time it is shown
type Builder = Box<dyn FnOnce(&SceneCtx) -> Box<dyn Scene>>;

struct Slot {
    name: &'static str,
    builder: Option<Builder>,
    scene: Option<Box<dyn Scene>>,
    // The camera for this scene, kept while other scenes are shown
    orbit: Orbit,
}

// Every scene the viewer can show, and which one is shown. Scenes are built
// lazily, then kept so switching back keeps their state
pub struct Scenes {
    slots: Vec<Slot>,
    active: usize,
}

impl Scenes {
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            active: 0,
        }
    }

    // Add a scene under a name, returning its id
    pub fn register(&mut self, name: &'static str, build: impl FnOnce(&SceneCtx) -> Box<dyn Scene> + 'static) -> usize {
        self.slots.push(Slot {
            name,
            builder: Some(Box::new(build)),
            scene: None,
            orbit: Orbit::default(),
        });
        self.slots.len() - 1
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.slots.iter().map(|slot| slot.name).collect()
    }

    pub fn active_id(&self) -> usize {
        self.active
    }

    // Show the scene with this id, building it if it hasn't been shown before
    pub fn activate(&mut self, id: usize, ctx: &SceneCtx) {
        self.active = id;
        let slot = &mut self.slots[id];
        if let Some(build) = slot.builder.take() {
            let scene = build(ctx);
            slot.orbit = scene.default_orbit();
            slot.scene = Some(scene);
        }
        self.active_mut().activate(ctx);
    }

    // The scene being shown. Panics before the first activate
    pub fn active_mut(&mut self) -> &mut dyn Scene {
        self.active_with_orbit().0
    }

    // The scene being shown and its camera, borrowed together
    pub fn active_with_orbit(&mut self) -> (&mut dyn Scene, &mut Orbit) {
        let slot = &mut self.slots[self.active];
        (
            slot.scene.as_deref_mut().expect("Scene shown before it was activated"),
            &mut slot.orbit,
        )
    }
}
