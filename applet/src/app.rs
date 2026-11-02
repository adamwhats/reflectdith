use cosmic::iced::futures::{SinkExt, StreamExt};
use cosmic::iced::platform_specific::shell::wayland::commands::popup::{destroy_popup, get_popup};
use cosmic::iced::{Color, Length, Limits, Subscription, window::Id};
use cosmic::widget::color_picker::{ColorPickerModel, ColorPickerUpdate};
use cosmic::{prelude::*, widget};
use palette::{FromColor, Srgb};
use reflecdith_control::{
    Settings,
    ipc::{self, State},
    settings::Palette,
};
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Default)]
pub struct Applet {
    core: cosmic::Core,
    popup: Option<Id>,
    state: Option<State>,
    settings: Settings,
    error: Option<String>,
    busy: bool,
    pending: Value,
    advanced: bool,
    standalone: bool,
    editing_colour: Option<ColourSlot>,
    picker: Option<ColorPickerModel>,
    output: String,
}

#[derive(Clone, Debug)]
pub enum Message {
    TogglePopup,
    Closed(Id),
    Refresh,
    Loaded(Result<State, String>),
    Changed(State),
    Patch(Value),
    Start,
    Started(Result<(), String>),
    Stop,
    Stopped(Result<(), String>),
    Random,
    ResetBoids,
    Swap,
    Advanced,
    EditColour(ColourSlot),
    Picker(ColorPickerUpdate),
    Output(String),
    Model(usize),
}

fn merge(target: &mut Value, patch: Value) {
    if let (Some(target), Some(patch)) = (target.as_object_mut(), patch.as_object()) {
        for (key, value) in patch {
            if let Some(old) = target.get_mut(key) {
                merge(old, value.clone());
            } else {
                target.insert(key.clone(), value.clone());
            }
        }
    } else {
        *target = patch;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColourSlot {
    Dark,
    Light,
}

impl ColourSlot {
    fn value(self, palette: &Palette) -> [f32; 3] {
        match self {
            Self::Dark => palette.dark,
            Self::Light => palette.light,
        }
    }
    fn patch(self, colour: Color) -> Value {
        let value = linear(colour);
        match self {
            Self::Dark => json!({"palette":{"dark":value}}),
            Self::Light => json!({"palette":{"light":value}}),
        }
    }
}

// The wallpaper stores linear light; COSMIC's picker and swatches use sRGB.
fn srgb(value: [f32; 3]) -> Color {
    let [r, g, b] = value.map(|v| {
        if v <= 0.0031308 {
            v * 12.92
        } else {
            1.055 * v.powf(1. / 2.4) - 0.055
        }
    });
    Color::from_rgb(r, g, b)
}
fn linear(value: Color) -> [f32; 3] {
    [value.r, value.g, value.b].map(|v| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    })
}
fn hex(value: [f32; 3]) -> String {
    let c = srgb(value);
    let [r, g, b] = [c.r, c.g, c.b].map(|v| (v * 255.).round() as u8);
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn slider<'a>(
    label: String,
    value: f32,
    min: f32,
    max: f32,
    patch: impl Fn(f32) -> Value + 'a,
) -> Element<'a, Message> {
    widget::column::with_children(vec![
        widget::text(label).into(),
        widget::slider(min..=max, value, move |v| Message::Patch(patch(v)))
            .step((max - min) / 100.)
            .into(),
    ])
    .spacing(4)
    .into()
}

impl Applet {
    fn request(&mut self, patch: Option<Value>) -> Task<cosmic::Action<Message>> {
        self.busy = true;
        Task::perform(ipc::request_async(patch.map(|v| v.to_string())), |result| {
            cosmic::Action::App(Message::Loaded(result))
        })
    }
    fn send_pending(&mut self) -> Task<cosmic::Action<Message>> {
        let patch = std::mem::replace(&mut self.pending, json!({}));
        self.request(Some(patch))
    }
}

impl cosmic::Application for Applet {
    type Executor = cosmic::executor::Default;
    type Flags = bool;
    type Message = Message;
    const APP_ID: &'static str = "io.github.reflecdith.Applet";
    fn core(&self) -> &cosmic::Core {
        &self.core
    }
    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }
    fn init(mut core: cosmic::Core, standalone: bool) -> (Self, Task<cosmic::Action<Message>>) {
        core.set_header_title("ReflecDith settings".into());
        let mut app = Self {
            core,
            standalone,
            pending: json!({}),
            ..Self::default()
        };
        let task = if standalone { app.request(None) } else { Task::none() };
        (app, task)
    }
    fn on_close_requested(&self, id: Id) -> Option<Message> {
        if self.standalone {
            None
        } else {
            Some(Message::Closed(id))
        }
    }
    fn view(&self) -> Element<'_, Message> {
        if self.standalone {
            return self.view_window(Id::RESERVED);
        }
        self.core
            .applet
            .icon_button("preferences-desktop-wallpaper-symbolic")
            .on_press(Message::TogglePopup)
            .into()
    }
    fn view_window(&self, _: Id) -> Element<'_, Message> {
        let s = &self.settings;
        let mut rows: Vec<Element<'_, Message>> = vec![widget::text::heading("ReflecDith").into()];
        if let Some(state) = &self.state {
            rows.push(
                widget::text(if state.output.is_empty() {
                    "Waiting for a monitor".into()
                } else {
                    format!("Wallpaper on {}", state.output)
                })
                .into(),
            );
            rows.push(
                widget::row::with_children(vec![
                    widget::button::text(if s.paused { "Resume" } else { "Pause" })
                        .on_press(Message::Patch(json!({"paused":!s.paused})))
                        .into(),
                    widget::button::text("Stop").on_press(Message::Stop).into(),
                ])
                .spacing(8)
                .into(),
            );
            rows.push(
                widget::row::with_children(
                    ["Mesh", "Boids", "Robot"]
                        .into_iter()
                        .enumerate()
                        .map(|(index, name)| {
                            let label = if index == s.selected_scene {
                                format!("• {name}")
                            } else {
                                name.into()
                            };
                            widget::button::text(label)
                                .on_press(Message::Patch(json!({"selected_scene":index})))
                                .into()
                        })
                        .collect::<Vec<Element<'_, Message>>>(),
                )
                .spacing(4)
                .into(),
            );
            if s.selected_scene == 0 {
                let selected = s
                    .scene
                    .mesh_model
                    .as_ref()
                    .and_then(|name| state.models.iter().position(|model| model == name))
                    .or_else(|| state.models.iter().position(|name| name == "utah_teapot_low"));
                let names = &state.models;
                if !names.is_empty() {
                    rows.push(widget::dropdown(names, selected, Message::Model).into());
                }
                rows.push(slider(
                    format!("Mesh spin: {:.0}°/s", s.scene.mesh_speed),
                    s.scene.mesh_speed,
                    -180.,
                    180.,
                    |v| json!({"scene":{"mesh_speed":v}}),
                ));
            }
            if s.selected_scene == 1 {
                rows.push(
                    widget::text(if state.spanning {
                        "One flock across the monitor layout"
                    } else {
                        "One flock on this monitor"
                    })
                    .into(),
                );
                rows.push(slider(
                    format!("Simulation speed: {:.2}×", s.scene.boids.time_scale),
                    s.scene.boids.time_scale,
                    0.,
                    2.,
                    |v| json!({"scene":{"boids":{"time_scale":v}}}),
                ));
            }
            rows.push(widget::text::heading("Colours").into());
            let swatches = [(ColourSlot::Dark, "Dark"), (ColourSlot::Light, "Light")]
                .into_iter()
                .map(|(slot, label)| {
                    let value = slot.value(&s.palette);
                    widget::column::with_children(vec![
                        widget::text(label).into(),
                        widget::color_picker::color_button(
                            Some(Message::EditColour(slot)),
                            Some(srgb(value)),
                            Length::Fill,
                        )
                        .width(Length::Fill)
                        .height(40)
                        .into(),
                        widget::text(hex(value)).into(),
                    ])
                    .spacing(4)
                    .width(Length::Fill)
                    .into()
                })
                .collect::<Vec<Element<'_, Message>>>();
            rows.push(widget::row::with_children(swatches).spacing(12).into());
            if let (Some(slot), Some(picker)) = (self.editing_colour, &self.picker) {
                rows.push(
                    widget::row::with_children(vec![
                        widget::text(match slot {
                            ColourSlot::Dark => "Dark colour",
                            ColourSlot::Light => "Light colour",
                        })
                        .into(),
                        widget::button::text("Done").on_press(Message::EditColour(slot)).into(),
                    ])
                    .spacing(12)
                    .into(),
                );
                rows.push(widget::text("Changes apply live").into());
                rows.push(
                    picker
                        .builder(Message::Picker)
                        .build("Recent colours", "Copy colour", "Copied")
                        .into(),
                );
            }
            rows.push(
                widget::row::with_children(vec![
                    widget::button::text("Random colours").on_press(Message::Random).into(),
                    widget::button::text("Swap").on_press(Message::Swap).into(),
                ])
                .spacing(8)
                .into(),
            );
            rows.push(slider(
                format!("Dither levels: {}", s.levels),
                s.levels as f32,
                2.,
                16.,
                |v| json!({"levels":v.round() as u32}),
            ));
            rows.push(slider(
                format!("Bayer density: {:.2}×", s.bayer_density),
                s.bayer_density,
                0.125,
                1.,
                |v| json!({"bayer_density":v}),
            ));
            rows.push(
                widget::settings::item(
                    "Shared pixel grid",
                    widget::toggler(s.shared_grid).on_toggle(|v| Message::Patch(json!({"shared_grid":v}))),
                )
                .into(),
            );
            rows.push(slider(
                format!("Frame limit: {} FPS", s.fps),
                s.fps as f32,
                1.,
                120.,
                |v| json!({"fps":v.round() as u32}),
            ));
            rows.push(
                widget::button::text(if self.advanced {
                    "Hide more settings"
                } else {
                    "More settings"
                })
                .on_press(Message::Advanced)
                .into(),
            );
            if self.advanced {
                rows.push(slider(
                    format!("Light rotation: {:.0}°/s", s.controls.light_speed),
                    s.controls.light_speed,
                    -180.,
                    180.,
                    |v| json!({"controls":{"light_speed":v}}),
                ));
                rows.push(slider(
                    format!("Shadows: {:.2}", s.shading.shadow_strength),
                    s.shading.shadow_strength,
                    0.,
                    1.,
                    |v| json!({"shading":{"shadow_strength":v}}),
                ));
                rows.push(slider(
                    format!("Shininess: {:.0}", s.shading.shininess),
                    s.shading.shininess,
                    1.,
                    64.,
                    |v| json!({"shading":{"shininess":v}}),
                ));
                rows.push(slider(
                    format!("Specular: {:.2}", s.shading.specular_strength),
                    s.shading.specular_strength,
                    0.,
                    3.,
                    |v| json!({"shading":{"specular_strength":v}}),
                ));
                rows.push(slider(
                    format!("Floor brightness: {:.2}", s.shading.floor_brightness),
                    s.shading.floor_brightness,
                    0.,
                    2.,
                    |v| json!({"shading":{"floor_brightness":v}}),
                ));
                rows.push(slider(
                    format!("Floor density: {:.0}", s.shading.floor_noise_density),
                    s.shading.floor_noise_density,
                    16.,
                    256.,
                    |v| json!({"shading":{"floor_noise_density":v}}),
                ));
                rows.push(slider(
                    format!("Floor fade start: {:.1}", s.shading.floor_fade_start),
                    s.shading.floor_fade_start,
                    0.,
                    6.,
                    |v| json!({"shading":{"floor_fade_start":v}}),
                ));
                rows.push(slider(
                    format!("Floor fade end: {:.1}", s.shading.floor_fade_end),
                    s.shading.floor_fade_end,
                    0.,
                    32.,
                    |v| json!({"shading":{"floor_fade_end":v}}),
                ));
                if s.selected_scene == 1 {
                    let b = &s.scene.boids;
                    rows.push(widget::text::heading("Boids").into());
                    rows.push(
                        widget::button::text("Reset boids")
                            .on_press_maybe((!self.busy).then_some(Message::ResetBoids))
                            .into(),
                    );

                    rows.push(slider(
                        format!("Trails: {:.2}s", s.scene.boids.trail_length),
                        s.scene.boids.trail_length,
                        0.,
                        3.,
                        |v| json!({"scene":{"boids":{"trail_length":v}}}),
                    ));

                    rows.push(slider(
                        format!("View distance: {:.2}", b.view_distance),
                        b.view_distance,
                        0.,
                        2.,
                        |v| json!({"scene":{"boids":{"view_distance":v}}}),
                    ));
                    rows.push(slider(
                        format!("Neighbours: {}", b.neighbours),
                        b.neighbours as f32,
                        1.,
                        20.,
                        |v| json!({"scene":{"boids":{"neighbours":v.round() as usize}}}),
                    ));
                    rows.push(slider(
                        format!("Separation: {:.2}", b.separation),
                        b.separation,
                        0.,
                        1.,
                        |v| json!({"scene":{"boids":{"separation":v}}}),
                    ));
                    rows.push(slider(
                        format!("Alignment: {:.1}", b.alignment),
                        b.alignment,
                        0.,
                        10.,
                        |v| json!({"scene":{"boids":{"alignment":v}}}),
                    ));
                    rows.push(slider(
                        format!("Cohesion: {:.1}", b.cohesion),
                        b.cohesion,
                        0.,
                        10.,
                        |v| json!({"scene":{"boids":{"cohesion":v}}}),
                    ));
                }
            }
        } else {
            rows.push(widget::text("Wallpaper is stopped").into());
            rows.push(
                widget::text_input("Output (blank = all monitors)", &self.output)
                    .on_input(Message::Output)
                    .into(),
            );
            rows.push(widget::button::text("Start wallpaper").on_press(Message::Start).into());
            rows.push(widget::button::text("Reconnect").on_press(Message::Refresh).into());
        }
        if let Some(error) = &self.error {
            rows.push(widget::text(error).into());
        }
        let content = widget::column::with_children(rows).spacing(12).padding(16).width(340);
        if self.standalone {
            widget::scrollable(content).height(cosmic::iced::Length::Fill).into()
        } else {
            self.core
                .applet
                .popup_container(widget::scrollable(content).height(if self.state.is_some() {
                    Length::Fixed(640.)
                } else {
                    Length::Shrink
                }))
                .into()
        }
    }
    fn subscription(&self) -> Subscription<Message> {
        if self.popup.is_none() && !self.standalone {
            return Subscription::none();
        }
        Subscription::batch([
            // Reconnect/status fallback, active only while the popup is open.
            cosmic::iced::time::every(Duration::from_secs(1)).map(|_| Message::Refresh),
            Subscription::run(|| {
                cosmic::iced::stream::channel(
                    8,
                    |mut sender: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
                        let _ = async {
                            let connection = zbus::Connection::session().await.ok()?;
                            let proxy = zbus::Proxy::new(&connection, ipc::BUS_NAME, ipc::OBJECT_PATH, ipc::INTERFACE)
                                .await
                                .ok()?;
                            let mut signals = proxy.receive_signal("StateChanged").await.ok()?;
                            while let Some(message) = signals.next().await {
                                let json = message.body().deserialize::<String>().ok()?;
                                let state = serde_json::from_str(&json).ok()?;
                                sender.send(Message::Changed(state)).await.ok()?;
                            }
                            Some(())
                        }
                        .await;
                        cosmic::iced::futures::future::pending::<()>().await;
                    },
                )
            }),
        ])
    }
    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::TogglePopup => {
                if let Some(id) = self.popup.take() {
                    return destroy_popup(id);
                }
                let id = Id::unique();
                self.popup = Some(id);
                let mut settings =
                    self.core
                        .applet
                        .get_popup_settings(self.core.main_window_id().unwrap(), id, None, None, None);
                settings.positioner.size_limits = Limits::NONE
                    .min_width(340.)
                    .max_width(380.)
                    .min_height(200.)
                    .max_height(800.);
                let load = self.request(None);
                return Task::batch([get_popup(settings), load]);
            }
            Message::Closed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                }
            }
            Message::Refresh => {
                if !self.busy {
                    return self.request(None);
                }
            }
            Message::Changed(state) => {
                if !self.busy && self.state.as_ref().is_none_or(|old| state.revision >= old.revision) {
                    self.settings = state.settings.clone();
                    self.state = Some(state);
                    self.error = None;
                }
            }
            Message::Loaded(result) => {
                self.busy = false;
                match result {
                    Ok(state) => {
                        // Keep the optimistic slider positions while newer edits are queued.
                        let next = state
                            .settings
                            .patched(&self.pending.to_string())
                            .unwrap_or_else(|_| state.settings.clone());
                        self.output = state.target_output.clone().unwrap_or_default();
                        self.settings = next;
                        self.state = Some(state);
                        self.error = None;
                    }
                    Err(error) => {
                        self.error = if error.contains("ServiceUnknown") || error.contains("NameHasNoOwner") {
                            None
                        } else {
                            Some(error)
                        };
                        self.state = None;
                        self.pending = json!({});
                    }
                }
                if self.pending.as_object().is_some_and(|p| !p.is_empty()) {
                    return self.send_pending();
                }
            }
            Message::Patch(patch) => {
                match self.settings.patched(&patch.to_string()) {
                    Ok(next) => self.settings = next,
                    Err(error) => {
                        self.error = Some(error);
                        return Task::none();
                    }
                }
                merge(&mut self.pending, patch);
                if !self.busy {
                    return self.send_pending();
                }
            }
            Message::ResetBoids => {
                if !self.busy {
                    self.busy = true;
                    return Task::perform(ipc::reset_boids_async(), |result| {
                        cosmic::Action::App(Message::Loaded(result))
                    });
                }
            }
            Message::Random => {
                self.editing_colour = None;
                self.picker = None;
                return self.update(Message::Patch(json!({"palette":Palette::random_muted()})));
            }
            Message::Swap => {
                self.editing_colour = None;
                self.picker = None;
                return self.update(Message::Patch(
                    json!({"palette":{"dark":self.settings.palette.light,"light":self.settings.palette.dark}}),
                ));
            }
            Message::Advanced => self.advanced = !self.advanced,
            Message::EditColour(slot) => {
                if self.editing_colour == Some(slot) {
                    self.editing_colour = None;
                    self.picker = None;
                } else {
                    self.editing_colour = Some(slot);
                    self.picker = Some(ColorPickerModel::new(
                        "Hex",
                        "RGB",
                        None,
                        Some(srgb(slot.value(&self.settings.palette))),
                    ));
                }
            }
            Message::Picker(update) => {
                if let (Some(slot), Some(picker)) = (self.editing_colour, &mut self.picker) {
                    let live = match &update {
                        ColorPickerUpdate::ActiveColor(hsv) => Some(Color::from(Srgb::from_color(*hsv))),
                        _ => None,
                    };
                    let input = matches!(update, ColorPickerUpdate::Input(_));
                    let finished = input
                        || matches!(
                            update,
                            ColorPickerUpdate::ActionFinished | ColorPickerUpdate::AppliedColor
                        );
                    let task = picker.update(update);
                    if input {
                        // The model retains the last valid parsed colour while text is incomplete.
                        let _ = picker.update::<Message>(ColorPickerUpdate::ActionFinished);
                    }
                    let colour = live.or_else(|| if finished { picker.get_applied_color() } else { None });
                    if let Some(colour) = colour.filter(|c| linear(*c) != slot.value(&self.settings.palette)) {
                        let patch = self.update(Message::Patch(slot.patch(colour)));
                        return Task::batch([task, patch]);
                    }
                    return task;
                }
            }
            Message::Output(value) => self.output = value,
            Message::Model(index) => {
                if let Some(name) = self.state.as_ref().and_then(|state| state.models.get(index)).cloned() {
                    return self.update(Message::Patch(json!({"scene":{"mesh_model":name}})));
                }
            }
            Message::Start => {
                let output = self.output.clone();
                self.busy = true;
                return Task::perform(
                    async move {
                        let executable = std::env::var_os("REFLECDITH_BIN")
                            .map(std::path::PathBuf::from)
                            .or_else(|| {
                                std::env::current_exe()
                                    .ok()
                                    .and_then(|p| p.parent().map(|p| p.join("reflecdith")))
                                    .filter(|p| p.is_file())
                            })
                            .unwrap_or_else(|| "reflecdith".into());
                        let mut command = std::process::Command::new(executable);
                        command.env_remove("WAYLAND_SOCKET");
                        command.arg("--wallpaper");
                        if !output.trim().is_empty() {
                            command.args(["--output", output.trim()]);
                        }
                        let mut child = command.spawn().map_err(|e| e.to_string())?;
                        // Reap the child without tying its lifetime to the popup or panel.
                        std::thread::spawn(move || {
                            let _ = child.wait();
                        });
                        Ok(())
                    },
                    |result| cosmic::Action::App(Message::Started(result)),
                );
            }
            Message::Started(result) => {
                self.busy = false;
                self.error = result.err();
            }
            Message::Stop => {
                self.busy = true;
                return Task::perform(ipc::stop_async(), |r| cosmic::Action::App(Message::Stopped(r)));
            }
            Message::Stopped(result) => {
                self.busy = false;
                self.error = result.err();
                if self.error.is_none() {
                    self.state = None;
                }
            }
        }
        Task::none()
    }
    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        if self.standalone {
            None
        } else {
            Some(cosmic::applet::style())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn picker_colours_roundtrip_without_changing_brightness() {
        for value in [[0., 0., 0.], [1., 1., 1.], [0.01, 0.25, 0.8]] {
            let actual = linear(srgb(value));
            for (a, b) in actual.into_iter().zip(value) {
                assert!((a - b).abs() < 1e-6);
            }
        }
        assert_eq!(hex(linear(Color::from_rgb8(37, 48, 74))), "#25304a");
        assert_eq!(
            ColourSlot::Dark.patch(Color::WHITE),
            json!({"palette":{"dark":[1.,1.,1.]}})
        );
        assert_eq!(
            ColourSlot::Light.patch(Color::BLACK),
            json!({"palette":{"light":[0.,0.,0.]}})
        );
    }
    #[test]
    fn coalescing_preserves_sibling_fields() {
        let mut pending = json!({"shading":{"shininess":10},"levels":4});
        merge(&mut pending, json!({"shading":{"shadow_strength":0.5,"shininess":20}}));
        assert_eq!(
            pending,
            json!({"shading":{"shininess":20,"shadow_strength":0.5},"levels":4})
        );
    }
}
