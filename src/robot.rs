//! URDF visuals and forward kinematics, kept independent of ROS transport.
use crate::{
    camera,
    vertices::{self, Vertex},
};
use clap::Parser;
use glam::{Mat4, Quat, Vec3};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser, Clone, Debug)]
#[command(about = "Dithered mesh / ROS 2 robot viewer")]
pub struct Options {
    /// Load an expanded URDF (run xacro separately).
    #[arg(long)]
    pub urdf: Option<PathBuf>,
    /// Connect to ROS 2. Requires a build with --features ros2.
    #[arg(long)]
    pub ros: bool,
    #[arg(long, default_value = "auto")]
    pub description_topic: String,
    #[arg(long, default_value = "auto")]
    pub joint_topic: String,
    #[arg(long, default_value = "/tf")]
    pub tf_topic: String,
    #[arg(long, default_value = "/tf_static")]
    pub tf_static_topic: String,
    /// TF frame for root motion; auto selects a connected odom/map frame.
    #[arg(long, default_value = "auto")]
    pub fixed_frame: Option<String>,
    /// Follow this TF frame; unqualified names inherit the URDF root namespace.
    #[arg(long, default_value = "base_footprint")]
    pub follow_frame: String,
    /// Resolve package://NAME/... using NAME=/path/to/package; repeatable.
    #[arg(long, value_name = "NAME=DIR")]
    pub package: Vec<String>,
    /// Base directory for relative meshes in a description received over ROS.
    #[arg(long, default_value = ".")]
    pub asset_dir: PathBuf,
    /// List discovered robot topics and TF frames without opening a window.
    #[arg(long)]
    pub list_topics: bool,
    /// Listen for joint states without opening a window; exit after this many seconds.
    #[arg(long)]
    pub probe_ros: Option<u64>,
    /// Start with the controls hidden; press Tab to show them.
    #[arg(long)]
    pub hide_gui: bool,
    /// Start with floor shadows disabled (also toggleable in the GUI).
    #[arg(long)]
    pub no_shadows: bool,
    /// Save a screenshot and exit after this many seconds (requires a display).
    #[arg(long)]
    pub capture_after: Option<u64>,
    /// Check URDF and mesh loading without opening a window.
    #[arg(long)]
    pub check_robot: bool,
}

#[derive(Default)]
pub struct LiveState {
    pub topics: crate::topics::TopicCatalog,
    pub requested: Option<crate::topics::TopicSelection>,
    pub active: Option<crate::topics::TopicSelection>,
    pub generation: u64,
    pub robot_root: Option<String>,
    pub description: Option<String>,
    pub positions: HashMap<String, f64>,
    // child -> (parent, parent-to-child transform)
    pub tf: HashMap<String, (String, Mat4)>,
    pub last_joint: Option<Instant>,
    pub error: Option<String>,
}
pub type SharedState = Arc<Mutex<LiveState>>;

pub struct Visual {
    link: usize,
    origin: Mat4,
    pub vertices: Vec<Vertex>,
}

pub struct Robot {
    pub name: String,
    pub root: String,
    links: Vec<String>,
    joints: Vec<urdf_rs::Joint>,
    pub visuals: Vec<Visual>,
    pub warnings: Vec<String>,
    // One display transform for the entire robot, never per-mesh normalization.
    display: Mat4,
}

fn pose(p: &urdf_rs::Pose) -> Mat4 {
    let [r, pch, y] = p.rpy.0.map(|v| v as f32);
    Mat4::from_rotation_translation(
        Quat::from_rotation_z(y) * Quat::from_rotation_y(pch) * Quat::from_rotation_x(r),
        Vec3::from_array(p.xyz.0.map(|v| v as f32)),
    )
}

pub fn resolve_mesh(uri: &str, base: &Path, options: &Options) -> Result<PathBuf> {
    if let Some(rest) = uri.strip_prefix("package://") {
        let (package, path) = rest.split_once('/').ok_or("Invalid package mesh URI")?;
        for entry in &options.package {
            let (name, dir) = entry.split_once('=').ok_or("--package expects NAME=DIR")?;
            if name == package {
                return Ok(Path::new(dir).join(path));
            }
        }
        for prefix in std::env::split_paths(&std::env::var_os("AMENT_PREFIX_PATH").unwrap_or_default()) {
            let candidate = prefix.join("share").join(package).join(path);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        return Err(format!("Cannot resolve {uri}; pass --package {package}=/path/to/{package}").into());
    }
    let path = Path::new(uri.strip_prefix("file://").unwrap_or(uri));
    Ok(if path.is_absolute() {
        path.into()
    } else {
        base.join(path)
    })
}

fn triangle(out: &mut Vec<Vertex>, a: Vec3, b: Vec3, c: Vec3) {
    let n = (b - a).cross(c - a).normalize_or_zero();
    if n == Vec3::ZERO {
        return;
    }
    for p in [a, b, c] {
        out.push(Vertex {
            position: p.to_array(),
            normal: n.to_array(),
        });
    }
}

fn geometry(g: &urdf_rs::Geometry, base: &Path, options: &Options) -> Result<Vec<Vertex>> {
    use urdf_rs::Geometry;
    let mut out = Vec::new();
    match g {
        Geometry::Mesh { filename, scale } => {
            let scale = Vec3::from_array(scale.as_ref().map_or([1.; 3], |v| v.0.map(|x| x as f32)));
            let path = resolve_mesh(filename, base, options)?;
            let scene = mesh_loader::Loader::default()
                .load(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            for mesh in scene.meshes {
                for face in mesh.faces {
                    let [a, b, c] = face.map(|i| Vec3::from_array(mesh.vertices[i as usize]) * scale);
                    triangle(&mut out, a, b, c);
                }
            }
        }
        Geometry::Box { size } => {
            let h = Vec3::from_array(size.0.map(|x| x as f32)) * 0.5;
            let p: Vec<_> = (0..8)
                .map(|i| {
                    Vec3::new(
                        if i & 1 == 0 { -h.x } else { h.x },
                        if i & 2 == 0 { -h.y } else { h.y },
                        if i & 4 == 0 { -h.z } else { h.z },
                    )
                })
                .collect();
            for [a, b, c, d] in [
                [0, 4, 6, 2],
                [1, 3, 7, 5],
                [0, 1, 5, 4],
                [2, 6, 7, 3],
                [0, 2, 3, 1],
                [4, 5, 7, 6],
            ] {
                triangle(&mut out, p[a], p[b], p[c]);
                triangle(&mut out, p[a], p[c], p[d]);
            }
        }
        Geometry::Cylinder { radius, length } => {
            let r = *radius as f32;
            let h = *length as f32 * 0.5;
            for i in 0..32 {
                let a = i as f32 * std::f32::consts::TAU / 32.;
                let b = (i + 1) as f32 * std::f32::consts::TAU / 32.;
                let p = Vec3::new(r * a.cos(), r * a.sin(), -h);
                let q = Vec3::new(r * b.cos(), r * b.sin(), -h);
                let up = Vec3::Z * (2. * h);
                triangle(&mut out, p, q, q + up);
                triangle(&mut out, p, q + up, p + up);
                triangle(&mut out, Vec3::new(0., 0., -h), q, p);
                triangle(&mut out, Vec3::new(0., 0., h), p + up, q + up);
            }
        }
        Geometry::Sphere { radius } => {
            for j in 0..16 {
                for i in 0..32 {
                    let point = |u: usize, v: usize| {
                        let lon = u as f32 * std::f32::consts::TAU / 32.;
                        let lat = v as f32 * std::f32::consts::PI / 16.;
                        Vec3::new(lat.sin() * lon.cos(), lat.sin() * lon.sin(), lat.cos()) * *radius as f32
                    };
                    let (a, b, c, d) = (point(i, j), point(i, j + 1), point(i + 1, j + 1), point(i + 1, j));
                    triangle(&mut out, a, b, c);
                    triangle(&mut out, a, c, d);
                }
            }
        }
        Geometry::Capsule { .. } => return Err("Capsule visuals are not supported yet".into()),
    }
    if out.is_empty() {
        return Err("Visual has no triangles".into());
    }
    Ok(out)
}

impl Robot {
    pub fn load(xml: &str, base: &Path, options: &Options) -> Result<Self> {
        let urdf = urdf_rs::read_from_string(xml)?;
        let links: Vec<_> = urdf.links.iter().map(|l| l.name.clone()).collect();
        let names: HashSet<_> = links.iter().cloned().collect();
        if names.len() != links.len() {
            return Err("Duplicate link names".into());
        }
        let mut children = HashSet::new();
        let mut joint_names = HashSet::new();
        for joint in &urdf.joints {
            if !names.contains(&joint.parent.link) || !names.contains(&joint.child.link) {
                return Err(format!("Joint {} references a missing link", joint.name).into());
            }
            if !children.insert(joint.child.link.clone()) || !joint_names.insert(joint.name.clone()) {
                return Err("Duplicate joint name or multiple parents for a link".into());
            }
            if !matches!(
                joint.joint_type,
                urdf_rs::JointType::Fixed
                    | urdf_rs::JointType::Revolute
                    | urdf_rs::JointType::Continuous
                    | urdf_rs::JointType::Prismatic
            ) {
                return Err(format!("Unsupported joint type on {}", joint.name).into());
            }
            if joint.joint_type != urdf_rs::JointType::Fixed
                && Vec3::from_array(joint.axis.xyz.0.map(|x| x as f32)).length_squared() < 1e-12
            {
                return Err(format!("Zero joint axis on {}", joint.name).into());
            }
        }
        let roots: Vec<_> = links.iter().filter(|l| !children.contains(*l)).cloned().collect();
        if roots.len() != 1 {
            return Err("URDF must have exactly one root".into());
        }
        let root = roots[0].clone();
        let mut ordered = Vec::new();
        let mut reached = HashSet::from([root.clone()]);
        while ordered.len() < urdf.joints.len() {
            let before = ordered.len();
            for j in &urdf.joints {
                if reached.contains(&j.parent.link) && !reached.contains(&j.child.link) {
                    reached.insert(j.child.link.clone());
                    ordered.push(j.clone());
                }
            }
            if before == ordered.len() {
                return Err("Disconnected or cyclic URDF".into());
            }
        }
        // Validate mimic references and cycles before accepting a live description.
        for j in &ordered {
            let mut seen = HashSet::new();
            let mut cursor = j;
            while let Some(m) = &cursor.mimic {
                if !seen.insert(cursor.name.clone()) {
                    return Err("Cyclic mimic joints".into());
                }
                cursor = ordered
                    .iter()
                    .find(|other| other.name == m.joint)
                    .ok_or("Mimic references a missing joint")?;
            }
        }
        let mut visuals = Vec::new();
        let mut warnings = Vec::new();
        for (link, l) in urdf.links.iter().enumerate() {
            for visual in &l.visual {
                match geometry(&visual.geometry, base, options) {
                    Ok(vertices) => visuals.push(Visual {
                        link,
                        origin: pose(&visual.origin),
                        vertices,
                    }),
                    Err(e) => warnings.push(format!("{}: {e}", l.name)),
                }
            }
        }
        if visuals.is_empty() {
            return Err(format!("No renderable visuals: {}", warnings.join("; ")).into());
        }
        let mut robot = Self {
            name: urdf.name,
            root,
            links,
            joints: ordered,
            visuals,
            warnings,
            display: Mat4::IDENTITY,
        };
        let transforms = robot.transforms(&LiveState::default(), None);
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for (v, t) in robot.visuals.iter().zip(transforms) {
            for vertex in &v.vertices {
                let p = t.transform_point3(Vec3::from_array(vertex.position));
                min = min.min(p);
                max = max.max(p);
            }
        }
        let centre = (min + max) * 0.5;
        let radius = (max - min).length() * 0.5;
        robot.display = Mat4::from_scale(Vec3::splat(1. / radius.max(0.01))) * Mat4::from_translation(-centre);
        Ok(robot)
    }

    fn joint_position(&self, joint: &urdf_rs::Joint, state: &LiveState) -> f32 {
        if let Some(m) = &joint.mimic {
            let source = self.joints.iter().find(|j| j.name == m.joint).unwrap();
            self.joint_position(source, state) * m.multiplier.unwrap_or(1.) as f32 + m.offset.unwrap_or(0.) as f32
        } else {
            state.positions.get(&joint.name).copied().unwrap_or(0.) as f32
        }
    }

    pub fn transforms(&self, state: &LiveState, fixed_frame: Option<&str>) -> Vec<Mat4> {
        let root_pose = fixed_frame
            .and_then(|frame| tf_transform(&state.tf, frame, &self.root))
            .unwrap_or(Mat4::IDENTITY);
        let mut world = HashMap::from([(self.root.as_str(), root_pose)]);
        for j in &self.joints {
            let q = self.joint_position(j, state);
            let axis = Vec3::from_array(j.axis.xyz.0.map(|x| x as f32)).normalize_or_zero();
            let motion = match j.joint_type {
                urdf_rs::JointType::Revolute | urdf_rs::JointType::Continuous => {
                    Mat4::from_quat(Quat::from_axis_angle(axis, q))
                }
                urdf_rs::JointType::Prismatic => Mat4::from_translation(axis * q),
                _ => Mat4::IDENTITY,
            };
            world.insert(&j.child.link, world[j.parent.link.as_str()] * pose(&j.origin) * motion);
        }
        let ros_to_y_up = Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        self.visuals
            .iter()
            .map(|v| self.display * ros_to_y_up * world[self.links[v.link].as_str()] * v.origin)
            .collect()
    }
}

pub fn tf_transform(tf: &HashMap<String, (String, Mat4)>, fixed: &str, target: &str) -> Option<Mat4> {
    fn ancestors(tf: &HashMap<String, (String, Mat4)>, frame: &str) -> HashMap<String, Mat4> {
        let mut out = HashMap::new();
        let mut frame = frame;
        let mut transform = Mat4::IDENTITY;
        loop {
            if out.insert(frame.to_string(), transform).is_some() {
                break;
            }
            let Some((parent, t)) = tf.get(frame) else {
                break;
            };
            transform = *t * transform;
            frame = parent;
        }
        out
    }
    let fixed = ancestors(tf, fixed.trim_start_matches('/'));
    for (common, t) in ancestors(tf, target.trim_start_matches('/')) {
        if let Some(f) = fixed.get(&common) {
            return Some(f.inverse() * t);
        }
    }
    None
}

pub struct RobotView {
    pub alignment: Mat4,
    pub camera_motion: Mat4,
    pub floor_origin: Vec3,
}

#[derive(Default)]
struct ViewTracking {
    initial_translation: std::sync::OnceLock<Mat4>,
    initial_footprint: std::sync::OnceLock<Mat4>,
}

impl ViewTracking {
    fn sample(&self, robot: &Robot, state: &LiveState, options: &Options, fallback_floor: f32) -> RobotView {
        let basis = robot.display * Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        let fixed = options.fixed_frame.as_deref();
        if let Some(root) = fixed.and_then(|f| tf_transform(&state.tf, f, &robot.root)) {
            self.initial_translation
                .get_or_init(|| Mat4::from_translation(-basis.transform_vector3(root.w_axis.truncate())));
        }
        let alignment = self.initial_translation.get().copied().unwrap_or(Mat4::IDENTITY);
        let frame = follow_frame(robot, options);
        let footprint = fixed.and_then(|f| tf_transform(&state.tf, f, &frame));
        let (camera_motion, floor_origin) = if let Some(footprint) = footprint {
            let displayed = alignment * basis * footprint;
            let initial = self.initial_footprint.get_or_init(|| displayed);
            // The scale cancels between the two frame transforms. Camera
            // translation and heading follow the footprint, excluding body bob/tilt.
            (displayed * initial.inverse(), displayed.w_axis.truncate())
        } else {
            (Mat4::IDENTITY, Vec3::new(0., fallback_floor, 0.))
        };
        RobotView {
            alignment,
            camera_motion,
            floor_origin,
        }
    }
}

pub fn follow_frame(robot: &Robot, options: &Options) -> String {
    let frame = options.follow_frame.trim_start_matches('/');
    if frame.contains('/') || options.follow_frame.starts_with('/') {
        return frame.to_string();
    }
    robot
        .root
        .rsplit_once('/')
        .map_or_else(|| frame.to_string(), |(namespace, _)| format!("{namespace}/{frame}"))
}

pub struct RobotDraw {
    mesh: vertices::Mesh,
    transform: camera::CameraTransform,
}
pub struct RobotScene {
    pub robot: Robot,
    draws: Vec<RobotDraw>,
    pub floor_y: f32,
    tracking: ViewTracking,
}
impl RobotScene {
    pub fn new(robot: Robot, device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> Self {
        let transforms = robot.transforms(&LiveState::default(), None);
        let floor_y = robot
            .visuals
            .iter()
            .zip(transforms)
            .flat_map(|(v, t)| {
                v.vertices
                    .iter()
                    .map(move |p| t.transform_point3(Vec3::from_array(p.position)).y)
            })
            .fold(f32::INFINITY, f32::min);
        let draws = robot
            .visuals
            .iter()
            .map(|v| RobotDraw {
                mesh: vertices::upload(device, &v.vertices),
                transform: camera::CameraTransform::new(
                    device,
                    layout,
                    &[0; std::mem::size_of::<camera::CameraUniform>()],
                ),
            })
            .collect();
        Self {
            robot,
            draws,
            floor_y,
            tracking: ViewTracking::default(),
        }
    }
    pub fn update(&self, queue: &wgpu::Queue, camera: &camera::CameraUniform, state: &LiveState, options: &Options) {
        let alignment = self.view(state, options).alignment;
        for (draw, model) in self
            .draws
            .iter()
            .zip(self.robot.transforms(state, options.fixed_frame.as_deref()))
        {
            let uniform = camera::CameraUniform {
                model: alignment * model,
                ..*camera
            };
            queue.write_buffer(&draw.transform.buffer, 0, bytemuck::bytes_of(&uniform));
        }
    }
    pub fn reset_tracking(&mut self) {
        self.tracking = ViewTracking::default();
    }

    pub fn view(&self, state: &LiveState, options: &Options) -> RobotView {
        self.tracking.sample(&self.robot, state, options, self.floor_y)
    }

    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        for draw in &self.draws {
            pass.set_vertex_buffer(0, draw.mesh.vertex_buffer.slice(..));
            pass.set_bind_group(0, &draw.transform.bind_group, &[]);
            pass.draw(0..draw.mesh.num_vertices as u32, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options() -> Options {
        Options::parse_from(["viewer"])
    }

    #[test]
    fn arm_keeps_link_dimensions_and_moves_the_child_about_its_joint() {
        let options = options();
        let robot = Robot::load(include_str!("../examples/arm.urdf"), Path::new("."), &options).unwrap();
        let mut state = LiveState::default();
        let rest = robot.transforms(&state, None);
        state.positions.insert("shoulder".into(), std::f64::consts::FRAC_PI_2);
        let bent = robot.transforms(&state, None);
        // The base is stationary; the elbow joint follows the upper arm, rather
        // than rotating around the centre of its own visual mesh.
        assert!(rest[0].abs_diff_eq(bent[0], 1e-5));
        let unproject = (robot.display * Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2)).inverse();
        let forearm = unproject * bent[2];
        assert!(
            forearm
                .transform_point3(Vec3::ZERO)
                .abs_diff_eq(Vec3::new(1.1, 0., 0.2), 1e-5)
        );
        assert!(
            (robot.visuals[1]
                .vertices
                .iter()
                .map(|v| v.position[2])
                .fold(f32::NEG_INFINITY, f32::max)
                - 0.4)
                .abs()
                < 1e-5
        );
    }

    #[test]
    fn prismatic_mimic_chain_and_tf_move_the_entire_robot() {
        let xml = r#"<robot name="gripper">
        <link name="base"/><link name="left"><visual><geometry><box size="0.1 0.1 0.1"/></geometry></visual></link>
        <link name="right"><visual><geometry><box size="0.1 0.1 0.1"/></geometry></visual></link>
        <joint name="left_slide" type="prismatic"><parent link="base"/><child link="left"/><axis xyz="1 0 0"/><limit lower="0" upper="1" effort="1" velocity="1"/></joint>
        <joint name="right_slide" type="prismatic"><parent link="base"/><child link="right"/><axis xyz="1 0 0"/><limit lower="-1" upper="0" effort="1" velocity="1"/><mimic joint="left_slide" multiplier="-1" offset="0.1"/></joint>
        </robot>"#;
        let robot = Robot::load(xml, Path::new("."), &options()).unwrap();
        let mut state = LiveState::default();
        state.positions.insert("left_slide".into(), 0.3);
        state.tf.insert(
            "base".into(),
            ("odom".into(), Mat4::from_translation(Vec3::new(2., 3., 0.))),
        );
        state
            .tf
            .insert("odom".into(), ("map".into(), Mat4::from_translation(Vec3::X)));
        let transforms = robot.transforms(&state, Some("map"));
        let unproject = (robot.display * Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2)).inverse();
        assert!(
            (unproject * transforms[0])
                .transform_point3(Vec3::ZERO)
                .abs_diff_eq(Vec3::new(3.3, 3., 0.), 1e-5)
        );
        assert!(
            (unproject * transforms[1])
                .transform_point3(Vec3::ZERO)
                .abs_diff_eq(Vec3::new(2.8, 3., 0.), 1e-5)
        );
        assert!(
            tf_transform(&state.tf, "base", "map")
                .unwrap()
                .transform_point3(Vec3::ZERO)
                .abs_diff_eq(Vec3::new(-3., -3., 0.), 1e-5)
        );
    }

    #[test]
    fn footprint_follow_tracks_ground_motion_without_body_bob_or_tilt() {
        let mut options = options();
        options.fixed_frame = Some("odom".into());
        let robot = Robot::load(include_str!("../examples/arm.urdf"), Path::new("."), &options).unwrap();
        let tracking = ViewTracking::default();
        let mut state = LiveState::default();
        state.tf.insert(
            "base_link".into(),
            ("base_footprint".into(), Mat4::from_translation(Vec3::Z * 0.7)),
        );
        state.tf.insert(
            "base_footprint".into(),
            ("odom".into(), Mat4::from_translation(Vec3::new(10., 20., 0.))),
        );
        let first = tracking.sample(&robot, &state, &options, -100.);
        assert!(first.camera_motion.abs_diff_eq(Mat4::IDENTITY, 1e-5));
        let basis = robot.display * Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2);
        let root = (first.alignment * basis * tf_transform(&state.tf, "odom", "base_link").unwrap())
            .w_axis
            .truncate();
        let scale = basis.transform_vector3(Vec3::Z).length();
        assert!((root.y - first.floor_origin.y - 0.7 * scale).abs() < 1e-5);
        state.tf.insert(
            "base_footprint".into(),
            (
                "odom".into(),
                Mat4::from_rotation_translation(Quat::from_rotation_z(0.5), Vec3::new(12., 20., 0.)),
            ),
        );
        let moved = tracking.sample(&robot, &state, &options, -100.);
        assert!(
            moved
                .camera_motion
                .transform_point3(first.floor_origin)
                .abs_diff_eq(moved.floor_origin, 1e-5)
        );
        assert!((moved.floor_origin.y - first.floor_origin.y).abs() < 1e-5);
        assert!(!moved.camera_motion.abs_diff_eq(Mat4::IDENTITY, 1e-5));
        state.tf.insert(
            "base_link".into(),
            (
                "base_footprint".into(),
                Mat4::from_rotation_translation(Quat::from_rotation_x(0.3), Vec3::Z * 1.2),
            ),
        );
        let bobbed = tracking.sample(&robot, &state, &options, -100.);
        assert!(bobbed.camera_motion.abs_diff_eq(moved.camera_motion, 1e-5));
        assert!(bobbed.floor_origin.abs_diff_eq(moved.floor_origin, 1e-5));
    }

    #[test]
    fn package_mesh_loads_without_normalizing_the_mesh() {
        let mut options = options();
        options.package.push(format!(
            "teapot={}",
            std::env::current_dir().unwrap().join("models").display()
        ));
        let xml = r#"<robot name="mesh"><link name="base"><visual><origin xyz="1 2 3"/><geometry><mesh filename="package://teapot/utah_teapot_low.stl" scale="0.001 0.002 0.003"/></geometry></visual></link></robot>"#;
        let robot = Robot::load(xml, Path::new("."), &options).unwrap();
        let visual = &robot.visuals[0];
        assert!(visual.vertices.len() > 100);
        assert_eq!(visual.origin.transform_point3(Vec3::ZERO), Vec3::new(1., 2., 3.));
        assert!(
            visual
                .vertices
                .iter()
                .all(|v| Vec3::from_array(v.position).length() < 1.)
        );
        let bad = xml.replace("utah_teapot_low.stl", "missing.stl");
        assert!(Robot::load(&bad, Path::new("."), &options).is_err());
    }
}
