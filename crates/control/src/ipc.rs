use crate::{Settings, WallpaperMode};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const BUS_NAME: &str = "io.github.reflecdith.Wallpaper";
pub const OBJECT_PATH: &str = "/io/github/reflecdith/Wallpaper";
pub const INTERFACE: &str = "io.github.reflecdith.Wallpaper1";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub revision: u64,
    pub settings: Settings,
    pub output: String,
    #[serde(default)]
    pub scenes: Vec<WallpaperScene>,
    #[serde(default)]
    pub outputs: Vec<Output>,
    #[serde(default)]
    pub target_output: Option<String>,
    #[serde(default)]
    pub wallpaper_mode: WallpaperMode,
    /// Whether the active scene renders a floor.
    #[serde(default)]
    pub has_floor: bool,
}

/// Scenes explicitly offered by the wallpaper application, with stable IDs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WallpaperScene {
    pub id: usize,
    pub name: String,
    pub mode: WallpaperMode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: i32,
}

pub struct Store {
    pub state: State,
    dirty_since: Option<Instant>,
    pub stop: bool,
    pub reset_scene: Option<usize>,
}

impl Store {
    pub fn new(settings: Settings, output: String) -> Self {
        Self {
            state: State {
                revision: 0,
                settings,
                output,
                scenes: Vec::new(),
                outputs: Vec::new(),
                target_output: None,
                wallpaper_mode: WallpaperMode::None,
                has_floor: false,
            },
            dirty_since: None,
            stop: false,
            reset_scene: None,
        }
    }
    pub fn wallpaper(mut settings: Settings, mut scenes: Vec<WallpaperScene>) -> Result<Self, String> {
        scenes.retain(|s| s.mode != WallpaperMode::None);
        let old_selection = settings.selected_scene;
        let first = scenes.first().ok_or("No scenes support wallpaper mode")?;
        if !scenes.iter().any(|s| s.id == settings.selected_scene) {
            settings.selected_scene = first.id;
        }
        let mut store = Self::new(settings, String::new());
        store.state.wallpaper_mode = scenes
            .iter()
            .find(|s| s.id == store.state.settings.selected_scene)
            .unwrap()
            .mode;
        store.state.scenes = scenes;
        if old_selection != store.state.settings.selected_scene {
            store.dirty_since = Some(Instant::now());
        }
        Ok(store)
    }

    pub fn patch(&mut self, patch: &str) -> Result<String, String> {
        let next = self.state.settings.patched(patch)?;
        if !self.state.scenes.is_empty() && !self.state.scenes.iter().any(|s| s.id == next.selected_scene) {
            return Err("Scene is not available as a wallpaper".into());
        }
        if next != self.state.settings {
            self.state.settings = next;
            if let Some(scene) = self
                .state
                .scenes
                .iter()
                .find(|s| s.id == self.state.settings.selected_scene)
            {
                self.state.wallpaper_mode = scene.mode;
            }
            self.state.revision += 1;
            self.dirty_since = Some(Instant::now());
        }
        self.json()
    }
    pub fn reset_boids(&mut self) -> Result<String, String> {
        if !self.state.scenes.is_empty() && !self.state.scenes.iter().any(|s| s.id == 1) {
            return Err("Boids is not available as a wallpaper".into());
        }
        self.state.settings.scene.boids = Default::default();
        self.reset_scene = Some(1);
        self.state.revision += 1;
        self.dirty_since = Some(Instant::now());
        self.json()
    }
    pub fn json(&self) -> Result<String, String> {
        serde_json::to_string(&self.state).map_err(|e| e.to_string())
    }
    pub fn flush(&mut self, path: &std::path::Path, force: bool) -> Result<(), String> {
        if self
            .dirty_since
            .is_some_and(|t| force || t.elapsed() >= Duration::from_millis(500))
        {
            crate::settings::save(path, &self.state.settings)?;
            self.dirty_since = None;
        }
        Ok(())
    }
}

pub type SharedStore = Arc<Mutex<Store>>;
struct Service(SharedStore);

#[zbus::interface(name = "io.github.reflecdith.Wallpaper1")]
impl Service {
    fn get_state(&self) -> zbus::fdo::Result<String> {
        self.0.lock().unwrap().json().map_err(zbus::fdo::Error::Failed)
    }
    async fn apply_patch(
        &self,
        changes: &str,
        #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::fdo::Result<String> {
        let state = self
            .0
            .lock()
            .unwrap()
            .patch(changes)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        let _ = Self::state_changed(&emitter, &state).await;
        Ok(state)
    }
    async fn reset_boids(
        &self,
        #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::fdo::Result<String> {
        let state = self.0.lock().unwrap().reset_boids().map_err(zbus::fdo::Error::Failed)?;
        let _ = Self::state_changed(&emitter, &state).await;
        Ok(state)
    }
    fn stop(&self) {
        self.0.lock().unwrap().stop = true;
    }
    #[zbus(signal)]
    async fn state_changed(emitter: &zbus::object_server::SignalEmitter<'_>, state: &str) -> zbus::Result<()>;
}

/// Keep this connection alive for as long as the wallpaper runs. Name ownership
/// also prevents a second wallpaper process from silently replacing the first.
pub fn serve(store: SharedStore) -> zbus::Result<zbus::blocking::Connection> {
    let connection = zbus::blocking::connection::Builder::session()?
        .serve_at(OBJECT_PATH, Service(store))?
        .build()?;
    // Request explicitly: a queued name must never let a second renderer start.
    connection.request_name_with_flags(BUS_NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into())?;
    Ok(connection)
}

pub fn get_state() -> Result<State, String> {
    request("GetState", None)
}
pub fn reset_boids() -> Result<State, String> {
    request("ResetBoids", None)
}

pub fn apply_patch(changes: &str) -> Result<State, String> {
    request("ApplyPatch", Some(changes))
}

fn request(method: &str, patch: Option<&str>) -> Result<State, String> {
    let connection = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let proxy = zbus::blocking::Proxy::new(&connection, BUS_NAME, OBJECT_PATH, INTERFACE).map_err(|e| e.to_string())?;
    let json: String = if let Some(patch) = patch {
        proxy.call(method, &(patch,))
    } else {
        proxy.call(method, &())
    }
    .map_err(|e| e.to_string())?;
    serde_json::from_str(&json).map_err(|e| e.to_string())
}

pub fn stop() -> Result<(), String> {
    let connection = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let proxy = zbus::blocking::Proxy::new(&connection, BUS_NAME, OBJECT_PATH, INTERFACE).map_err(|e| e.to_string())?;
    proxy.call("Stop", &()).map_err(|e| e.to_string())
}

pub async fn request_async(patch: Option<String>) -> Result<State, String> {
    let connection = zbus::Connection::session().await.map_err(|e| e.to_string())?;
    let proxy = zbus::Proxy::new(&connection, BUS_NAME, OBJECT_PATH, INTERFACE)
        .await
        .map_err(|e| e.to_string())?;
    let json: String = if let Some(patch) = patch {
        proxy.call("ApplyPatch", &(patch,)).await
    } else {
        proxy.call("GetState", &()).await
    }
    .map_err(|e| e.to_string())?;
    serde_json::from_str(&json).map_err(|e| e.to_string())
}

pub async fn reset_boids_async() -> Result<State, String> {
    let connection = zbus::Connection::session().await.map_err(|e| e.to_string())?;
    let proxy = zbus::Proxy::new(&connection, BUS_NAME, OBJECT_PATH, INTERFACE)
        .await
        .map_err(|e| e.to_string())?;
    let json: String = proxy.call("ResetBoids", &()).await.map_err(|e| e.to_string())?;
    serde_json::from_str(&json).map_err(|e| e.to_string())
}

pub async fn stop_async() -> Result<(), String> {
    let connection = zbus::Connection::session().await.map_err(|e| e.to_string())?;
    let proxy = zbus::Proxy::new(&connection, BUS_NAME, OBJECT_PATH, INTERFACE)
        .await
        .map_err(|e| e.to_string())?;
    proxy.call("Stop", &()).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wallpaper_catalog_migrates_old_selection_and_rejects_viewer_only_scenes() {
        let mut store = Store::wallpaper(
            Settings::default(),
            vec![WallpaperScene {
                id: 1,
                name: "Boids".into(),
                mode: WallpaperMode::Span,
            }],
        )
        .unwrap();
        assert_eq!(store.state.settings.selected_scene, 1);
        for id in [0, 2, 99] {
            let before = store.state.settings.clone();
            assert!(store.patch(&format!(r#"{{"selected_scene":{id},"fps":10}}"#)).is_err());
            assert_eq!(store.state.settings, before);
        }
        assert!(store.patch(r#"{"selected_scene":1,"fps":10}"#).is_ok());
        assert!(Store::wallpaper(Settings::default(), vec![]).is_err());
    }

    #[test]
    fn single_mode_and_future_scene_ids_are_supported_without_enabling_none() {
        let mut store = Store::wallpaper(
            Settings::default(),
            vec![
                WallpaperScene {
                    id: 0,
                    name: "Viewer only".into(),
                    mode: WallpaperMode::None,
                },
                WallpaperScene {
                    id: 7,
                    name: "Future wallpaper".into(),
                    mode: WallpaperMode::Single,
                },
            ],
        )
        .unwrap();
        assert_eq!(store.state.scenes.len(), 1);
        assert_eq!(store.state.settings.selected_scene, 7);
        assert!(store.patch(r#"{"selected_scene":0}"#).is_err());
        assert!(store.patch(r#"{"selected_scene":7}"#).is_ok());
        assert_eq!(store.state.scenes[0].mode, WallpaperMode::Single);
    }

    #[test]
    fn rejected_patch_does_not_mutate_state() {
        let mut store = Store::new(Settings::default(), "DP-1".into());
        assert!(store.patch(r#"{"levels":4,"fps":0}"#).is_err());
        assert_eq!(store.state.revision, 0);
        assert_eq!(store.state.settings.levels, 3);
        store.patch(r#"{"levels":4}"#).unwrap();
        assert_eq!(store.state.revision, 1);
        store.patch(r#"{"levels":4}"#).unwrap();
        assert_eq!(store.state.revision, 1);
    }

    #[test]
    fn boid_reset_restores_only_boid_settings_and_always_requests_a_restart() {
        let mut settings = Settings::default();
        settings.scene.boids.time_scale = 0.4;
        settings.scene.boids.trail_length = 2.;
        settings.levels = 7;
        settings.paused = true;
        let mut store = Store::new(settings.clone(), "DP-1".into());
        store.reset_boids().unwrap();
        settings.scene.boids = Default::default();
        assert_eq!(store.state.settings, settings);
        assert_eq!(store.reset_scene, Some(1));
        assert_eq!(store.state.revision, 1);
        store.reset_scene = None;
        store.reset_boids().unwrap();
        assert_eq!(store.reset_scene, Some(1));
        assert_eq!(store.state.revision, 2);
    }

    #[test]
    fn changes_are_saved_on_shutdown_without_waiting_for_debounce() {
        let dir = std::env::temp_dir().join(format!("reflecdith-persistence-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut store = Store::new(Settings::default(), "DP-1".into());
        store
            .patch(r#"{"paused":true,"scene":{"boids":{"trail_length":2.5}}}"#)
            .unwrap();
        store.flush(&path, true).unwrap();
        let restored = crate::settings::load(&path).unwrap();
        assert!(restored.paused);
        assert_eq!(restored.scene.boids.trail_length, 2.5);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Run under dbus-run-session so this never claims the user's actual service.
    #[test]
    #[ignore = "requires an isolated D-Bus session"]
    fn session_bus_roundtrip() {
        let store = Arc::new(Mutex::new(Store::new(Settings::default(), "TEST-1".into())));
        let service = serve(store.clone()).unwrap();
        assert!(serve(Arc::new(Mutex::new(Store::new(Settings::default(), "TEST-2".into())))).is_err());
        let client = zbus::blocking::Connection::session().unwrap();
        let proxy = zbus::blocking::Proxy::new(&client, BUS_NAME, OBJECT_PATH, INTERFACE).unwrap();
        let mut signals = proxy.receive_signal("StateChanged").unwrap();
        assert_eq!(get_state().unwrap().output, "TEST-1");
        assert!(apply_patch(r#"{"levels":100}"#).is_err());
        let next = apply_patch(r#"{"levels":5,"paused":true}"#).unwrap();
        assert_eq!(next.revision, 1);
        let signal = signals.next().unwrap();
        let received: State = serde_json::from_str(&signal.body().deserialize::<String>().unwrap()).unwrap();
        assert_eq!(received.settings.levels, 5);
        assert!(received.settings.paused);
        let reset = reset_boids().unwrap();
        assert_eq!(reset.revision, 2);
        assert_eq!(store.lock().unwrap().reset_scene, Some(1));
        assert_eq!(reset.settings.levels, 5);
        stop().unwrap();
        assert!(store.lock().unwrap().stop);
        drop(service);
    }
}
