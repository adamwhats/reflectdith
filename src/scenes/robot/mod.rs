//! A URDF robot, loaded from a file or received over ROS 2, posed by live
//! joint states and TF, with the camera following its footprint.
pub mod model;
#[cfg(feature = "ros2")]
pub mod ros;
pub mod topics;

pub use model::{Options, Robot, SharedState, follow_frame, tf_transform};

use glam::{Mat4, Vec3};

use super::{Scene, SceneCtx, SceneView};
use crate::engine::camera::{CameraUniform, Orbit};

// Where the floor goes before a robot has loaded
const EMPTY_FLOOR_Y: f32 = -0.5;

// What main hands the robot scene: the options, the state the ROS worker
// fills in, a robot loaded from --urdf, and a ROS connection made before the
// window opened
pub struct Startup {
    pub options: Options,
    pub live: SharedState,
    pub robot: Option<Robot>,
    #[cfg(feature = "ros2")]
    pub connection: Option<ros::RosConnection>,
}

pub struct RobotScene {
    options: Options,
    live: SharedState,
    #[cfg(feature = "ros2")]
    connection: Option<ros::RosConnection>,
    // The robot on the GPU, once a description has been loaded
    geometry: Option<model::RobotGeometry>,
    // The worker's generation the geometry belongs to; a new one means the
    // topics changed and the old robot is stale
    generation: u64,
    // This frame's camera follow and alignment, refreshed by update
    view: Option<model::RobotView>,
    // Changed whenever the robot is loaded, removed or moves, for the shadow
    // cache, and the geometry's pose revision when it last moved
    revision: u64,
    last_pose: u64,
    // The robot and topic picker, once the worker has reported a selection,
    // and the status line shown under it
    topics: Option<topics::TopicPanel>,
    status: Option<String>,
}

impl RobotScene {
    pub fn new(ctx: &SceneCtx, startup: Startup) -> Self {
        let status = (startup.options.ros || startup.options.urdf.is_some())
            .then(|| "Waiting for robot description".to_string());
        Self {
            geometry: startup
                .robot
                .map(|robot| model::RobotGeometry::new(robot, ctx.device, ctx.camera_layout)),
            options: startup.options,
            live: startup.live,
            #[cfg(feature = "ros2")]
            connection: startup.connection,
            generation: 0,
            view: None,
            revision: 0,
            last_pose: 0,
            topics: None,
            status,
        }
    }

    // Follow the worker's state: drop a stale robot, mirror the topic
    // selection into the menu, load a newly received description, and
    // describe what is happening in the status line
    fn sync(&mut self, ctx: &SceneCtx) {
        if !self.options.ros && self.options.urdf.is_none() {
            return;
        }
        if self.options.ros {
            let state = self.live.lock().unwrap();
            if self.generation != state.generation {
                if self.options.urdf.is_none() && self.geometry.take().is_some() {
                    self.revision += 1;
                }
                self.generation = state.generation;
            }
            if let Some(active) = &state.active {
                if (self.options.fixed_frame != active.fixed_frame || self.options.follow_frame != active.follow_frame)
                    && let Some(geometry) = &mut self.geometry
                {
                    geometry.reset_tracking();
                }
                self.options.description_topic.clone_from(&active.description);
                self.options.joint_topic.clone_from(&active.joints);
                self.options.fixed_frame.clone_from(&active.fixed_frame);
                self.options.follow_frame.clone_from(&active.follow_frame);
            }
            if let Some(requested) = &state.requested {
                let panel = self.topics.get_or_insert_with(|| topics::TopicPanel {
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
        // An explicitly supplied file owns the geometry even if ROS publishes a
        // description.
        if self.options.urdf.is_none()
            && let Some(xml) = description
        {
            eprintln!("Loading robot description and visual meshes...");
            match Robot::load(&xml, &self.options.asset_dir, &self.options) {
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
                    self.geometry = Some(model::RobotGeometry::new(robot, ctx.device, ctx.camera_layout));
                    self.revision += 1;
                }
                Err(error) => {
                    self.live.lock().unwrap().error = Some(format!("Description: {error}"));
                }
            }
        }
        let state = self.live.lock().unwrap();
        self.status = Some(if let Some(error) = &state.error {
            error.clone()
        } else if let Some(scene) = &self.geometry {
            let age = state.last_joint.map_or("waiting for joints".into(), |t| {
                format!("joints {:.1}s ago", t.elapsed().as_secs_f32())
            });
            let tf = if let Some(frame) = &self.options.fixed_frame {
                if tf_transform(&state.tf, frame, &scene.robot.root).is_none() {
                    format!("; waiting for TF {frame} → {}", scene.robot.root)
                } else {
                    let follow = follow_frame(&scene.robot, &self.options);
                    if tf_transform(&state.tf, frame, &follow).is_none() {
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
}

impl Scene for RobotScene {
    fn default_orbit(&self) -> Orbit {
        let mut orbit = Orbit::default();
        orbit.fit_robot();
        orbit
    }

    // Connect to ROS the first time the scene is shown without a robot source
    fn activate(&mut self, _ctx: &SceneCtx) {
        if self.options.ros || self.options.urdf.is_some() {
            return;
        }
        #[cfg(feature = "ros2")]
        match ros::RosConnection::start(self.options.clone(), self.live.clone()) {
            Ok(connection) => {
                self.connection = Some(connection);
                self.options.ros = true;
                self.status = Some("Waiting for robot description".into());
            }
            Err(error) => self.status = Some(format!("ROS connection: {error}")),
        }
        #[cfg(not(feature = "ros2"))]
        {
            self.status = Some(
                "Live robots require the ROS build: scripts/ros-viewer. You can also launch with --urdf PATH.".into(),
            );
        }
    }

    fn update(&mut self, ctx: &SceneCtx, _dt: f32) {
        // Pass a robot or topic picked in the menu on to the worker
        if let Some(topics) = &mut self.topics
            && std::mem::take(&mut topics.changed)
        {
            self.live.lock().unwrap().requested = Some(topics.requested.clone());
        }
        self.sync(ctx);
        self.view = self
            .geometry
            .as_ref()
            .map(|geometry| geometry.view(&self.live.lock().unwrap(), &self.options));
    }

    fn view(&self) -> SceneView {
        match &self.view {
            Some(view) => SceneView {
                camera_motion: view.camera_motion,
                floor_origin: view.floor_origin,
                ..Default::default()
            },
            None => SceneView {
                floor_origin: Vec3::new(0., EMPTY_FLOOR_Y, 0.),
                ..Default::default()
            },
        }
    }

    // Each visual has its own transform uniform, built from the camera's
    fn prepare(&mut self, queue: &wgpu::Queue, camera: &CameraUniform) {
        let Some(geometry) = &self.geometry else {
            return;
        };
        let alignment = self.view.as_ref().map_or(Mat4::IDENTITY, |view| view.alignment);
        geometry.update(queue, camera, &self.live.lock().unwrap(), &self.options, alignment);
        let pose = geometry.pose_revision.get();
        if pose != self.last_pose {
            self.last_pose = pose;
            self.revision += 1;
        }
    }

    fn shadow_revision(&self) -> u64 {
        self.revision
    }

    fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if let Some(geometry) = &self.geometry {
            geometry.draw(pass);
        }
    }

    fn draw_shadow<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if let Some(geometry) = &self.geometry {
            geometry.draw_shadow(pass);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        if let Some(topics) = &mut self.topics {
            topics.ui(ui);
        }
        if let Some(status) = &self.status {
            ui.add(
                egui::Label::new(status.replace('\n', " · "))
                    .truncate()
                    .show_tooltip_when_elided(false),
            );
        }
    }
}
