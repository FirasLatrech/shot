//! User preferences, persisted as JSON. Every field has a default, so old
//! settings files keep loading as new options are added.

use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    background::Background,
    geom::{Color, Rect},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Action {
    AllInOne,
    CaptureArea,
    CaptureWindow,
    CaptureFullscreen,
    CapturePrevious,
    SelfTimer,
    CaptureText,
    OpenHistory,
    RestoreOverlay,
    ToggleOverlays,
}

impl Action {
    pub const ALL: [Action; 10] = [
        Action::AllInOne,
        Action::CaptureArea,
        Action::CaptureWindow,
        Action::CaptureFullscreen,
        Action::CapturePrevious,
        Action::SelfTimer,
        Action::CaptureText,
        Action::OpenHistory,
        Action::RestoreOverlay,
        Action::ToggleOverlays,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::AllInOne => "All-In-One",
            Action::CaptureArea => "Capture Area",
            Action::CaptureWindow => "Capture Window",
            Action::CaptureFullscreen => "Capture Fullscreen",
            Action::CapturePrevious => "Capture Previous Area",
            Action::SelfTimer => "Self-Timer",
            Action::CaptureText => "Capture Text (OCR)",
            Action::OpenHistory => "Capture History",
            Action::RestoreOverlay => "Restore Recently Closed",
            Action::ToggleOverlays => "Hide/Show Overlays",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Corner {
    #[default]
    BottomLeft,
    BottomRight,
    TopLeft,
    TopRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ImageFormat {
    #[default]
    Png,
    Jpeg,
}

impl ImageFormat {
    pub fn ext(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Where "Save" writes files; `None` means the Desktop.
    pub save_dir: Option<PathBuf>,
    pub format: ImageFormat,
    pub show_overlay: bool,
    pub copy_after_capture: bool,
    pub save_after_capture: bool,
    pub play_sound: bool,
    pub overlay_corner: Corner,
    /// Overlay thumbnail size multiplier.
    pub overlay_scale: f32,
    /// Seconds before the overlay closes itself; `None` keeps it open.
    pub overlay_auto_close: Option<u32>,
    pub show_crosshair: bool,
    pub show_magnifier: bool,
    pub freeze_screen: bool,
    pub window_shadow: bool,
    /// Backdrop for window captures; `None` keeps them transparent.
    pub window_background: Option<Background>,
    pub self_timer_secs: u32,
    pub history_days: u32,
    pub hotkeys: BTreeMap<Action, String>,
    pub custom_colors: Vec<Color>,
    pub background_presets: Vec<(String, Background)>,
    /// Last All-In-One / area selection, in global points.
    pub last_selection: Option<Rect>,
}

impl Default for Settings {
    fn default() -> Self {
        let hotkeys = [
            (Action::AllInOne, "shift+cmd+KeyA"),
            (Action::CaptureArea, "shift+cmd+Digit4"),
            (Action::CaptureFullscreen, "shift+cmd+Digit3"),
            (Action::CaptureWindow, "shift+cmd+Digit5"),
            (Action::CaptureText, "shift+cmd+KeyT"),
            (Action::RestoreOverlay, "shift+cmd+KeyR"),
        ]
        .into_iter()
        .map(|(a, k)| (a, k.to_string()))
        .collect();
        Self {
            save_dir: None,
            format: ImageFormat::Png,
            show_overlay: true,
            copy_after_capture: false,
            save_after_capture: false,
            play_sound: true,
            overlay_corner: Corner::BottomLeft,
            overlay_scale: 1.0,
            overlay_auto_close: None,
            show_crosshair: true,
            show_magnifier: true,
            freeze_screen: true,
            window_shadow: true,
            window_background: None,
            self_timer_secs: 5,
            history_days: 30,
            hotkeys,
            custom_colors: Vec::new(),
            background_presets: Vec::new(),
            last_selection: None,
        }
    }
}

impl Settings {
    /// Missing files fall back to defaults; unreadable ones are kept aside
    /// as `settings.json.bad` so a later save never destroys them.
    pub fn load(path: &std::path::Path) -> Self {
        crate::history::read_json_or_backup(path).unwrap_or_default()
    }

    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::history::write_atomic(path, &serde_json::to_vec_pretty(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_fills_defaults() {
        let s: Settings = serde_json::from_str(r#"{"self_timer_secs": 3}"#).unwrap();
        assert_eq!(s.self_timer_secs, 3);
        assert_eq!(s.history_days, 30);
        assert!(s.hotkeys.contains_key(&Action::CaptureArea));
    }
}
