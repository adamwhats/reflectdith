use serde::{Deserialize, Serialize};
use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
};

// Dither endpoints in linear RGB.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Palette {
    pub dark: [f32; 3],
    pub light: [f32; 3],
}

// Tone count, evenly spaced between the palette endpoints.
pub const DEFAULT_LEVELS: u32 = 3;
pub const MAX_LEVELS: u32 = 16;
pub const DEFAULT_BAYER_DENSITY: f32 = 0.5;

// Keep density in saved settings for compatibility, but rasterize whole pixels.
pub fn bayer_cell_size(density: f32) -> u32 {
    (density.clamp(0.125, 1.).recip().round() as u32).clamp(1, 8)
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            // sRGB #170505 and #685841, stored as linear RGB.
            dark: [0.008568126, 0.001517635, 0.001517635],
            light: [0.13843162, 0.09758735, 0.052860647],
        }
    }
}

impl Palette {
    pub fn random_muted() -> Self {
        Self::muted_from_seed(RandomState::new().build_hasher().finish())
    }

    pub fn muted_from_seed(mut seed: u64) -> Self {
        let mut unit = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 40) as f32 / 16777216.
        };
        let hue = unit();
        // Mix related hues with occasional split-complementary pairings.
        let light_hue = hue
            + if unit() < 0.6 {
                (unit() - 0.5) / 3.
            } else {
                1. / 3. + unit() / 3.
            };
        Self {
            dark: muted_hsl(hue, 0.10 + unit() * 0.28, 0.06 + unit() * 0.14),
            light: muted_hsl(light_hue, 0.10 + unit() * 0.28, 0.50 + unit() * 0.22),
        }
    }
}

fn muted_hsl(hue: f32, saturation: f32, lightness: f32) -> [f32; 3] {
    let chroma = (1. - (2. * lightness - 1.).abs()) * saturation;
    let h = hue.rem_euclid(1.) * 6.;
    let x = chroma * (1. - (h % 2. - 1.).abs());
    let rgb = match h as u32 {
        0 => [chroma, x, 0.],
        1 => [x, chroma, 0.],
        2 => [0., chroma, x],
        3 => [0., x, chroma],
        4 => [x, 0., chroma],
        _ => [chroma, 0., x],
    };
    rgb.map(|channel| {
        let srgb = channel + lightness - chroma * 0.5;
        if srgb <= 0.04045 {
            srgb / 12.92
        } else {
            ((srgb + 0.055) / 1.055).powf(2.4)
        }
    })
}

// Surface shading controls.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shading {
    // How tight the specular highlight is, higher is smaller and sharper
    pub shininess: f32,
    // How bright the specular highlight is
    pub specular_strength: f32,
    // The grey level the floor reflects
    pub floor_brightness: f32,
    // Distances from the centre in world units where the floor starts and
    // finishes fading to black
    pub floor_fade_start: f32,
    pub floor_fade_end: f32,
    pub floor_noise_density: f32,
    // How dark shadows are, from 0 (off) to 1 (fully shadowed)
    pub shadow_strength: f32,
}

impl Default for Shading {
    fn default() -> Self {
        Self {
            shininess: 32.0,
            specular_strength: 1.5,
            floor_brightness: 0.5,
            floor_fade_start: 0.6,
            floor_fade_end: 2.5,
            floor_noise_density: 128.0,
            shadow_strength: 1.0,
        }
    }
}

// Light rotation about the world Y axis.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightControls {
    // Angle in degrees
    #[serde(skip)]
    pub light_angle: f32,
    // Speed in degrees per second
    pub light_speed: f32,
}

impl Default for LightControls {
    fn default() -> Self {
        Self {
            light_angle: 0.0,
            light_speed: (-0.4_f32).to_degrees(),
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub palette: Palette,
    pub levels: u32,
    pub bayer_density: f32,
    pub shared_grid: bool,
    pub controls: LightControls,
    pub shading: Shading,
    /// Stable scene IDs: mesh = 0, boids = 1, robot = 2.
    pub selected_scene: usize,
    pub fps: u32,
    pub paused: bool,
    /// None uses each scene's own default orbit.
    pub camera: Option<CameraSettings>,
    pub scene: SceneSettings,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraSettings {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SceneSettings {
    pub mesh_model: Option<String>,
    pub mesh_speed: f32,
    pub boids: BoidsSettings,
}
impl Default for SceneSettings {
    fn default() -> Self {
        Self {
            mesh_model: None,
            mesh_speed: 0.5_f32.to_degrees(),
            boids: BoidsSettings::default(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BoidsSettings {
    pub view_distance: f32,
    pub neighbours: usize,
    pub separation: f32,
    pub alignment: f32,
    pub cohesion: f32,
    pub trail_length: f32,
    /// Simulation seconds per real second; independent of the render frame cap.
    pub time_scale: f32,
}
impl Default for BoidsSettings {
    fn default() -> Self {
        Self {
            view_distance: 0.5,
            neighbours: 7,
            separation: 0.2,
            alignment: 2.,
            cohesion: 2.,
            trail_length: 0.,
            time_scale: 1.,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            palette: Palette::default(),
            levels: DEFAULT_LEVELS,
            bayer_density: DEFAULT_BAYER_DENSITY,
            shared_grid: true,
            controls: LightControls::default(),
            shading: Shading::default(),
            selected_scene: 0,
            fps: 30,
            paused: false,
            camera: None,
            scene: SceneSettings::default(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        fn range(name: &str, value: f32, min: f32, max: f32) -> Result<(), String> {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return Err(format!("{name} must be between {min} and {max}"));
            }
            Ok(())
        }
        if !(2..=MAX_LEVELS).contains(&self.levels) {
            return Err("levels must be between 2 and 16".into());
        }
        if !(1..=120).contains(&self.fps) {
            return Err("fps must be between 1 and 120".into());
        }
        for value in self.palette.dark.iter().chain(&self.palette.light) {
            range("colour channel", *value, 0., 1.)?;
        }
        range("Bayer density", self.bayer_density, 0.125, 1.)?;
        range("light speed", self.controls.light_speed, -180., 180.)?;
        range("shininess", self.shading.shininess, 1., 64.)?;
        range("specular", self.shading.specular_strength, 0., 3.)?;
        range("shadow strength", self.shading.shadow_strength, 0., 1.)?;
        range("floor brightness", self.shading.floor_brightness, 0., 2.)?;
        range("floor fade start", self.shading.floor_fade_start, 0., 6.)?;
        range("floor fade end", self.shading.floor_fade_end, 0., 32.)?;
        range("floor density", self.shading.floor_noise_density, 16., 256.)?;
        range("mesh speed", self.scene.mesh_speed, -180., 180.)?;
        let boids = &self.scene.boids;
        range("boid view distance", boids.view_distance, 0., 2.)?;
        if !(1..=20).contains(&boids.neighbours) {
            return Err("neighbours must be between 1 and 20".into());
        }
        range("boid separation", boids.separation, 0., 1.)?;
        range("boid alignment", boids.alignment, 0., 10.)?;
        range("boid cohesion", boids.cohesion, 0., 10.)?;
        range("boid trails", boids.trail_length, 0., 3.)?;
        range("boid simulation speed", boids.time_scale, 0., 2.)?;
        if self
            .scene
            .mesh_model
            .as_ref()
            .is_some_and(|name| name.is_empty() || name.contains(['/', '\\']) || name.len() > 255)
        {
            return Err("mesh_model must be a model name, without a path or extension".into());
        }
        if let Some(camera) = &self.camera {
            range("camera yaw", camera.yaw, -360., 360.)?;
            range("camera pitch", camera.pitch, -89., 89.)?;
            range("camera distance", camera.distance, 0.1, 100.)?;
        }
        Ok(())
    }

    /// Recursive object patches preserve unrelated fields; null resets the camera.
    pub fn patched(&self, patch: &str) -> Result<Self, String> {
        let patch: serde_json::Value = serde_json::from_str(patch).map_err(|e| e.to_string())?;
        if !patch.is_object() {
            return Err("settings patch must be an object".into());
        }
        fn merge(target: &mut serde_json::Value, patch: serde_json::Value) {
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
        let mut value = serde_json::to_value(self).map_err(|e| e.to_string())?;
        merge(&mut value, patch);
        let next: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        next.validate()?;
        Ok(next)
    }
}

pub fn config_path() -> Result<std::path::PathBuf, String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| std::path::PathBuf::from(p).join(".config")))
        .ok_or("Neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(base.join("reflecdith/settings.json"))
}

pub fn load(path: &std::path::Path) -> Result<Settings, String> {
    match std::fs::read_to_string(path) {
        Ok(json) => {
            let settings: Settings = serde_json::from_str(&json).map_err(|e| e.to_string())?;
            settings.validate()?;
            Ok(settings)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn save(path: &std::path::Path, settings: &Settings) -> Result<(), String> {
    settings.validate()?;
    let parent = path.parent().ok_or("settings path has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_patch_preserves_other_settings() {
        let old = Settings::default();
        let next = old.patched(r#"{"shading":{"shininess":12},"paused":true}"#).unwrap();
        assert_eq!(next.shading.shininess, 12.);
        assert_eq!(next.shading.floor_noise_density, old.shading.floor_noise_density);
        assert_eq!(next.palette, old.palette);
        assert!(next.paused);
        assert!(!old.paused);
    }
    #[test]
    fn invalid_and_unknown_values_are_rejected() {
        for patch in [
            r#"{"fps":0}"#,
            r#"{"levels":99}"#,
            r#"{"selected_scene":-1}"#,
            r#"{"bayer_density":0}"#,
            r#"{"shading":{"shininess":0}}"#,
            r#"{"shading":{"typo":2}}"#,
            r#"{"typo":true}"#,
            "[]",
        ] {
            assert!(Settings::default().patched(patch).is_err(), "{patch}");
        }
    }
    #[test]
    fn playback_speed_is_validated_and_old_settings_keep_real_time() {
        let mut json = serde_json::to_value(Settings::default()).unwrap();
        json["scene"]["boids"].as_object_mut().unwrap().remove("time_scale");
        let old: Settings = serde_json::from_value(json).unwrap();
        assert_eq!(old.scene.boids.time_scale, 1.);
        for scale in [0., 0.25, 1., 2.] {
            let patch = format!(r#"{{"scene":{{"boids":{{"time_scale":{scale}}}}}}}"#);
            assert_eq!(old.patched(&patch).unwrap().scene.boids.time_scale, scale);
        }
        assert!(old.patched(r#"{"scene":{"boids":{"time_scale":-1}}}"#).is_err());
        assert!(old.patched(r#"{"scene":{"boids":{"time_scale":3}}}"#).is_err());
    }

    #[test]
    fn runtime_light_angle_is_not_persisted() {
        let mut settings = Settings::default();
        settings.controls.light_angle = 123.;
        let loaded: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(loaded.controls.light_angle, 0.);
        assert_eq!(loaded.controls.light_speed, settings.controls.light_speed);
    }
}
