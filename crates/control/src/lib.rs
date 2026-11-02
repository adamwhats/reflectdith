//! Settings and session-bus protocol shared by the renderer and its controllers.
pub mod ipc;
pub mod settings;
pub use settings::Settings;

/// How a scene participates in wallpaper presentation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WallpaperMode {
    #[default]
    None,
    /// Repeat one view on each output.
    Single,
    /// Render one desktop-wide view, cropped per output.
    Span,
}
