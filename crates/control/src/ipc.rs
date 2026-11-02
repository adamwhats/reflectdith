use crate::Settings;
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
    pub models: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<Output>,
    #[serde(default)]
    pub target_output: Option<String>,
    #[serde(default)]
    pub spanning: bool,
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
    pub reset_boids: bool,
}

impl Store {
    pub fn new(settings: Settings, output: String) -> Self {
        Self {
            state: State {
                revision: 0,
                settings,
                output,
                models: Vec::new(),
                outputs: Vec::new(),
                target_output: None,
                spanning: false,
            },
            dirty_since: None,
            stop: false,
            reset_boids: false,
        }
    }
    pub fn patch(&mut self, patch: &str) -> Result<String, String> {
        let next = self.state.settings.patched(patch)?;
        if let Some(name) = &next.scene.mesh_model {
            if !self.state.models.is_empty() && !self.state.models.contains(name) {
                return Err(format!("Unknown mesh model: {name}"));
            }
        }
        if next != self.state.settings {
            self.state.settings = next;
            self.state.revision += 1;
            self.dirty_since = Some(Instant::now());
        }
        self.json()
    }
    pub fn reset_boids(&mut self) -> Result<String, String> {
        self.state.settings.scene.boids = Default::default();
        self.reset_boids = true;
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
        assert!(store.reset_boids);
        assert_eq!(store.state.revision, 1);
        store.reset_boids = false;
        store.reset_boids().unwrap();
        assert!(store.reset_boids);
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
        assert!(store.lock().unwrap().reset_boids);
        assert_eq!(reset.settings.levels, 5);
        stop().unwrap();
        assert!(store.lock().unwrap().stop);
        drop(service);
    }
}
