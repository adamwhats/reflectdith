//! rclrs subscriptions on a worker thread; the renderer only reads snapshots.
use crate::robot::{Options, SharedState};
use glam::{Mat4, Quat, Vec3};
use rclrs::{
    CreateBasicExecutor, DynamicMessage, DynamicMessageView, IntoPrimitiveOptions, SequenceValue, SimpleValue, Value,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub struct RosConnection {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Drop for RosConnection {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl RosConnection {
    pub fn start(options: Options, state: SharedState) -> Result<Self, Box<dyn std::error::Error>> {
        state.lock().unwrap().requested = Some(crate::topics::TopicSelection::from(&options));
        // Report discovery/type-support setup errors before opening the viewer.
        let context = rclrs::Context::default_from_env()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let run = || -> Result<(), Box<dyn std::error::Error>> {
                let mut executor = context.create_basic_executor();
                let node_name = format!("reflecdith_{}", std::process::id());
                let node = executor.create_node(node_name.as_str())?;
                let mut subscriptions = Vec::new();
                let mut current_topics = None;
                let mut last_discovery = Instant::now() - Duration::from_secs(1);
                let mut ready = false;
                while !stop_thread.load(Ordering::Relaxed) {
                    if last_discovery.elapsed() >= Duration::from_secs(1) {
                        let mut catalog = crate::topics::TopicCatalog::default();
                        for (name, types) in node.get_topic_names_and_types()? {
                            if node.count_publishers(&name)? == 0 {
                                continue;
                            }
                            if types.iter().any(|t| t == "std_msgs/msg/String") {
                                catalog.descriptions.push(name.clone());
                            }
                            if types.iter().any(|t| t == "sensor_msgs/msg/JointState") {
                                catalog.joints.push(name.clone());
                            }
                            if types.iter().any(|t| t == "tf2_msgs/msg/TFMessage") {
                                catalog.transforms.push(name);
                            }
                        }
                        let mut shared = state.lock().unwrap();
                        catalog.frames = shared
                            .tf
                            .iter()
                            .flat_map(|(child, (parent, _))| [child.clone(), parent.clone()])
                            .collect();
                        catalog.descriptions.sort();
                        catalog.joints.sort();
                        catalog.transforms.sort();
                        catalog.frames.sort();
                        catalog.frames.dedup();
                        shared.topics = catalog;
                        last_discovery = Instant::now();
                    }
                    let selection = {
                        let shared = state.lock().unwrap();
                        shared.requested.as_ref().unwrap().resolve(&shared)
                    };
                    let key = selection.topic_key();
                    if current_topics.as_ref() != Some(&key) {
                        let generation = {
                            let mut shared = state.lock().unwrap();
                            shared.generation += 1;
                            shared.description = None;
                            shared.positions.clear();
                            shared.last_joint = None;
                            shared.tf.clear();
                            shared.error = None;
                            if options.urdf.is_none() {
                                shared.robot_root = None;
                            }
                            shared.generation
                        };
                        subscriptions.clear();
                        subscriptions =
                            subscribe_topics(&node, &selection, state.clone(), generation, options.urdf.is_none())?;
                        current_topics = Some(key);
                        eprintln!(
                            "ROS topics: description={}, joints={}, TF={} / {}",
                            selection.description, selection.joints, selection.transforms, selection.static_transforms
                        );
                    }
                    state.lock().unwrap().active = Some(selection);
                    if !ready {
                        let _ = ready_tx.send(Ok::<(), String>(()));
                        ready = true;
                    }
                    for error in executor.spin(rclrs::SpinOptions::spin_once().timeout(Duration::from_millis(50))) {
                        if !error.is_timeout() {
                            return Err(error.into());
                        }
                    }
                }
                drop(subscriptions);
                Ok(())
            };
            if let Err(error) = run() {
                let error = error.to_string();
                let _ = ready_tx.send(Err(error.clone()));
                eprintln!("ROS error: {error}");
                state.lock().unwrap().error = Some(error);
            }
        });
        let connection = Self {
            stop,
            thread: Some(thread),
        };
        ready_rx
            .recv()
            .map_err(|e| format!("ROS startup thread failed: {e}"))?
            .map_err(|e| format!("ROS startup: {e}"))?;
        Ok(connection)
    }
}

fn subscribe_topics(
    node: &rclrs::Node,
    selection: &crate::topics::TopicSelection,
    state: SharedState,
    generation: u64,
    use_description_root: bool,
) -> Result<Vec<rclrs::DynamicSubscription>, Box<dyn std::error::Error>> {
    let mut subscriptions = Vec::new();
    if !selection.joints.is_empty() {
        let joint_state = state.clone();
        subscriptions.push(node.create_dynamic_subscription(
            "sensor_msgs/msg/JointState".try_into()?,
            selection.joints.as_str().keep_last(10).best_effort(),
            move |msg: DynamicMessage, _| {
                let (
                    Some(Value::Sequence(SequenceValue::StringSequence(names))),
                    Some(Value::Sequence(SequenceValue::DoubleSequence(positions))),
                ) = (msg.get("name"), msg.get("position"))
                else {
                    return;
                };
                if names.is_empty() || names.len() != positions.len() {
                    return;
                }
                let mut state = joint_state.lock().unwrap();
                if state.generation != generation {
                    return;
                }
                let mut updated = false;
                for (name, position) in names.iter().zip(positions.iter()) {
                    if position.is_finite() && (*position as f32).is_finite() {
                        state.positions.insert(name.to_string(), *position);
                        updated = true;
                    }
                }
                if updated {
                    state.last_joint = Some(Instant::now());
                }
            },
        )?);
    }
    if !selection.description.is_empty() {
        let description_state = state.clone();
        subscriptions.push(node.create_dynamic_subscription(
            "std_msgs/msg/String".try_into()?,
            selection.description.as_str().keep_last(1).reliable().transient_local(),
            move |msg: DynamicMessage, _| {
                if let Some(Value::Simple(SimpleValue::String(xml))) = msg.get("data") {
                    let xml = xml.to_string();
                    let root = if use_description_root {
                        urdf_rs::read_from_string(&xml).ok().and_then(|robot| {
                            robot
                                .links
                                .iter()
                                .find(|link| !robot.joints.iter().any(|j| j.child.link == link.name))
                                .map(|link| link.name.clone())
                        })
                    } else {
                        None
                    };
                    let mut state = description_state.lock().unwrap();
                    if state.generation != generation {
                        return;
                    }
                    if use_description_root {
                        state.robot_root = root;
                    }
                    state.description = Some(xml);
                }
            },
        )?);
    }
    // Discover TF frames even before a fixed frame has been chosen.
    for (topic, is_static) in [(&selection.transforms, false), (&selection.static_transforms, true)] {
        let tf_state = state.clone();
        let qos = topic.as_str().keep_last(100);
        let qos = if is_static {
            qos.reliable().transient_local()
        } else {
            qos.best_effort()
        };
        subscriptions.push(node.create_dynamic_subscription(
            "tf2_msgs/msg/TFMessage".try_into()?,
            qos,
            move |msg: DynamicMessage, _| {
                let Some(Value::Sequence(SequenceValue::MessageSequence(items))) = msg.get("transforms") else {
                    return;
                };
                let mut state = tf_state.lock().unwrap();
                if state.generation != generation {
                    return;
                }
                for item in items.iter() {
                    if let Some((child, parent, transform)) = parse_tf(item) {
                        state.tf.insert(child, (parent, transform));
                    }
                }
            },
        )?);
    }
    Ok(subscriptions)
}

fn string(view: &DynamicMessageView<'_>, name: &str) -> Option<String> {
    match view.get(name)? {
        Value::Simple(SimpleValue::String(s)) => Some(s.to_string().trim_start_matches('/').to_string()),
        _ => None,
    }
}
fn message<'a>(view: &DynamicMessageView<'a>, name: &str) -> Option<DynamicMessageView<'a>> {
    match view.get(name)? {
        Value::Simple(SimpleValue::Message(m)) => Some(m),
        _ => None,
    }
}
fn number(view: &DynamicMessageView<'_>, name: &str) -> Option<f32> {
    match view.get(name)? {
        Value::Simple(SimpleValue::Double(n)) if n.is_finite() && (*n as f32).is_finite() => Some(*n as f32),
        _ => None,
    }
}
fn parse_tf(view: &DynamicMessageView<'_>) -> Option<(String, String, Mat4)> {
    let header = message(view, "header")?;
    let transform = message(view, "transform")?;
    let t = message(&transform, "translation")?;
    let q = message(&transform, "rotation")?;
    let rotation = Quat::from_xyzw(number(&q, "x")?, number(&q, "y")?, number(&q, "z")?, number(&q, "w")?);
    if rotation.length_squared() < 1e-12 {
        return None;
    }
    Some((
        string(view, "child_frame_id")?,
        string(&header, "frame_id")?,
        Mat4::from_rotation_translation(
            rotation.normalize(),
            Vec3::new(number(&t, "x")?, number(&t, "y")?, number(&t, "z")?),
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    #[ignore = "requires a running ROS robot with a robot_description and joint-state publisher"]
    fn discovers_robot_and_switches_joint_subscriptions() {
        fn wait(state: &SharedState, condition: impl Fn(&crate::robot::LiveState) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(12);
            loop {
                {
                    let state = state.lock().unwrap();
                    assert!(state.error.is_none(), "ROS error: {:?}", state.error);
                    if condition(&state) {
                        return;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "Timed out waiting for discovery/subscription switch"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let options = Options::parse_from(["viewer", "--ros"]);
        let state = SharedState::default();
        let _connection = RosConnection::start(options, state.clone()).unwrap();
        wait(&state, |s| s.robot_root.is_some() && !s.positions.is_empty());
        let selected = state.lock().unwrap().active.as_ref().unwrap().joints.clone();
        state.lock().unwrap().requested.as_mut().unwrap().joints = "/reflecdith_test_missing_joints".into();
        wait(&state, |s| {
            s.active
                .as_ref()
                .is_some_and(|a| a.joints == "/reflecdith_test_missing_joints")
                && s.positions.is_empty()
                && s.last_joint.is_none()
        });
        state.lock().unwrap().requested.as_mut().unwrap().joints = selected.clone();
        wait(&state, |s| {
            s.active.as_ref().is_some_and(|a| a.joints == selected) && !s.positions.is_empty() && s.robot_root.is_some()
        });
    }
}
