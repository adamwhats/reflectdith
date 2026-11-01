//! ROS graph choices shared by the worker and GUI; selection policy is transport-independent.
#[cfg(any(feature = "ros2", test))]
use crate::robot::LiveState;
use crate::robot::Options;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopicCatalog {
    pub descriptions: Vec<String>,
    pub joints: Vec<String>,
    pub transforms: Vec<String>,
    pub frames: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicSelection {
    pub description: String,
    pub joints: String,
    pub transforms: String,
    pub static_transforms: String,
    pub fixed_frame: Option<String>,
    pub follow_frame: String,
}

impl From<&Options> for TopicSelection {
    fn from(o: &Options) -> Self {
        Self {
            description: o.description_topic.clone(),
            joints: o.joint_topic.clone(),
            transforms: o.tf_topic.clone(),
            static_transforms: o.tf_static_topic.clone(),
            fixed_frame: o.fixed_frame.clone(),
            follow_frame: o.follow_frame.clone(),
        }
    }
}
impl TopicSelection {
    #[cfg(feature = "ros2")]
    pub fn topic_key(&self) -> [String; 4] {
        [
            self.description.clone(),
            self.joints.clone(),
            self.transforms.clone(),
            self.static_transforms.clone(),
        ]
    }

    #[cfg(any(feature = "ros2", test))]
    pub fn resolve(&self, state: &LiveState) -> Self {
        let mut resolved = self.clone();
        if self.description == "auto" {
            let candidates: Vec<_> = state
                .topics
                .descriptions
                .iter()
                .filter(|s| s.rsplit('/').next() == Some("robot_description"))
                .cloned()
                .collect();
            resolved.description = choose(&candidates, state.active.as_ref().map(|a| a.description.as_str()));
        }
        if self.joints == "auto" {
            let namespace = resolved.description.rsplit_once('/').map(|(ns, _)| ns);
            let matching: Vec<_> = state
                .topics
                .joints
                .iter()
                .filter(|s| s.rsplit_once('/').map(|(ns, _)| ns) == namespace)
                .cloned()
                .collect();
            let candidates = if matching.is_empty() {
                &state.topics.joints
            } else {
                &matching
            };
            resolved.joints = choose(candidates, state.active.as_ref().map(|a| a.joints.as_str()));
        }
        if self.fixed_frame.as_deref() == Some("auto") {
            resolved.fixed_frame = state.robot_root.as_deref().and_then(|root| {
                let namespace = root
                    .rsplit_once('/')
                    .map(|(ns, _)| format!("{ns}/"))
                    .unwrap_or_default();
                for name in [
                    format!("{namespace}odom"),
                    "odom".into(),
                    format!("{namespace}map"),
                    "map".into(),
                ] {
                    if state.topics.frames.contains(&name)
                        && crate::robot::tf_transform(&state.tf, &name, root).is_some()
                    {
                        return Some(name);
                    }
                }
                // For fixed-base robots, use the highest connected ancestor.
                let mut frame = root;
                let mut seen = std::collections::HashSet::new();
                while seen.insert(frame) {
                    let Some((parent, _)) = state.tf.get(frame) else {
                        break;
                    };
                    frame = parent;
                }
                (frame != root).then(|| frame.to_string())
            });
        }
        resolved
    }
}
#[cfg(any(feature = "ros2", test))]
fn choose(candidates: &[String], previous: Option<&str>) -> String {
    if let [only] = candidates {
        return only.clone();
    }
    previous
        .filter(|old| candidates.is_empty() || candidates.iter().any(|c| c == old))
        .unwrap_or("")
        .to_string()
}

#[derive(Clone)]
pub struct TopicPanel {
    pub catalog: TopicCatalog,
    pub requested: TopicSelection,
    pub active: Option<TopicSelection>,
    pub changed: bool,
}
impl TopicPanel {
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let before = self.requested.clone();
        let mut selected = if self.requested.description == "auto" {
            self.active
                .as_ref()
                .filter(|a| !a.description.is_empty())
                .map_or_else(String::new, |a| a.description.clone())
        } else {
            self.requested.description.clone()
        };
        let previous_selected = selected.clone();
        let label = if selected.is_empty() {
            "Select robot".into()
        } else {
            robot_name(&selected)
        };
        ui.horizontal(|ui| {
            ui.label("Robot");
            egui::ComboBox::from_id_salt("ros_robot")
                .width(200.)
                .truncate()
                .selected_text(label)
                .show_ui(ui, |ui| {
                    if !selected.is_empty() && !self.catalog.descriptions.contains(&selected) {
                        let current = selected.clone();
                        ui.selectable_value(
                            &mut selected,
                            current.clone(),
                            format!("{} (offline)", robot_name(&current)),
                        );
                    }
                    for topic in &self.catalog.descriptions {
                        if topic.rsplit('/').next() == Some("robot_description") {
                            ui.selectable_value(&mut selected, topic.clone(), robot_name(topic));
                        }
                    }
                });
        });
        if !selected.is_empty() && selected != previous_selected {
            self.requested.select_robot(selected);
        }
        if self
            .catalog
            .descriptions
            .iter()
            .all(|t| t.rsplit('/').next() != Some("robot_description"))
        {
            ui.small("Looking for robots…");
        }
        self.changed |= before != self.requested;
    }
}

fn robot_name(topic: &str) -> String {
    let name = topic.trim_end_matches("/robot_description").trim_matches('/');
    if name.is_empty() { "Robot".into() } else { name.into() }
}

impl TopicSelection {
    pub fn select_robot(&mut self, description: String) {
        self.description = description;
        self.joints = "auto".into();
        self.fixed_frame = Some("auto".into());
        self.follow_frame = "base_footprint".into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use glam::{Mat4, Vec3};
    #[test]
    fn showing_discovered_robot_does_not_reset_explicit_topic_overrides() {
        let mut requested = TopicSelection::from(&Options::parse_from(["viewer", "--joint-topic", "/custom/joints"]));
        requested.fixed_frame = Some("custom_world".into());
        let mut active = requested.clone();
        active.description = "/m20/robot_description".into();
        let mut panel = TopicPanel {
            catalog: TopicCatalog {
                descriptions: vec![active.description.clone()],
                ..Default::default()
            },
            requested: requested.clone(),
            active: Some(active),
            changed: false,
        };
        let context = egui::Context::default();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| panel.ui(ui));
        output.textures_delta.clear();
        assert_eq!(panel.requested, requested);
        assert!(!panel.changed);
    }

    #[test]
    fn auto_pairs_robot_topics_and_waits_when_the_robot_is_ambiguous() {
        let request = TopicSelection::from(&Options::parse_from(["viewer"]));
        let mut state = LiveState::default();
        state.topics.descriptions = vec![
            "/m20/robot_description".into(),
            "/m20/robot_description_semantic".into(),
        ];
        state.topics.joints = vec!["/other/joint_states".into(), "/m20/joint_states".into()];
        state.robot_root = Some("m20/base_link".into());
        state.topics.frames = vec!["m20/odom".into(), "m20/base_link".into()];
        state.tf.insert(
            "m20/base_link".into(),
            ("m20/odom".into(), Mat4::from_translation(Vec3::Z)),
        );
        let active = request.resolve(&state);
        assert_eq!(active.description, "/m20/robot_description");
        assert_eq!(active.joints, "/m20/joint_states");
        assert_eq!(active.fixed_frame.as_deref(), Some("m20/odom"));
        state.topics.descriptions.push("/other/robot_description".into());
        assert!(request.resolve(&state).description.is_empty());
        state.active = Some(active.clone());
        assert_eq!(request.resolve(&state).description, active.description);
        let mut explicit = request;
        explicit.select_robot("/other/robot_description".into());
        assert_eq!(explicit.resolve(&state).joints, "/other/joint_states");
    }
}
