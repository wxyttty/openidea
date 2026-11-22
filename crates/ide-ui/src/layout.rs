//! Persisted workbench layout: left tool-window width and per-tab bottom heights.
//!
//! Stored next to the theme in `<config>/ui.json` under `layout`, so resizing
//! panels survives restarts without touching project files.

use std::path::Path;

const FILE: &str = "ui.json";

#[derive(Clone)]
pub struct Layout {
    pub left_width: f32,
    /// Problems, Terminal, Diff, Run, Debug, Database, HTTP, Notebook, Agents,
    /// Notifications. Indices are stable; new tabs append at the end so saved
    /// layouts keep their meaning.
    pub heights: [f32; 16],
    /// The bottom dock's pane tree (drag-to-split), saved as JSON so a custom
    /// arrangement survives a restart. `None` uses the default tab layout.
    pub dock: Option<serde_json::Value>,
}

impl Default for Layout {
    fn default() -> Self {
        Self { left_width: 280., dock: None, heights: [180., 366., 420., 320., 420., 420., 420., 420., 420., 420., 420., 420., 420., 420., 420., 320.] }
    }
}

impl Layout {
    pub fn load(config_dir: &Path) -> Self {
        let mut layout = Layout::default();
        let Some(value) = read(config_dir) else { return layout };
        let Some(saved) = value.get("layout") else { return layout };
        if let Some(width) = saved.get("leftWidth").and_then(|value| value.as_f64()) {
            layout.left_width = (width as f32).clamp(160., 600.);
        }
        layout.dock = saved.get("dock").cloned().filter(|value| !value.is_null());
        if let Some(heights) = saved.get("bottomHeights").and_then(|value| value.as_object()) {
            for (index, key) in KEYS.iter().enumerate() {
                if let Some(height) = heights.get(*key).and_then(|value| value.as_f64()) {
                    layout.heights[index] = (height as f32).clamp(120., 760.);
                }
            }
        }
        layout
    }

    pub fn save(&self, config_dir: &Path) {
        let mut value = read(config_dir).unwrap_or_else(|| serde_json::json!({}));
        if !value.is_object() {
            value = serde_json::json!({});
        }
        let heights = KEYS
            .iter()
            .enumerate()
            .map(|(index, key)| ((*key).to_owned(), serde_json::json!(self.heights[index].round() as u32)))
            .collect::<serde_json::Map<_, _>>();
        let mut layout = serde_json::json!({
            "leftWidth": self.left_width.round() as u32,
            "bottomHeights": heights,
        });
        if let Some(dock) = &self.dock {
            layout["dock"] = dock.clone();
        }
        value["layout"] = layout;
        if let Some(parent) = config_dir.join(FILE).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&value) {
            let _ = std::fs::write(config_dir.join(FILE), bytes);
        }
    }
}

/// The main window's saved geometry (Tauri restored this too; a fresh install
/// opens centred at the default size).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowState {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Whether the window was maximized when it closed.
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self { x: f32::NAN, y: f32::NAN, width: 1360., height: 880., maximized: false }
    }
}

impl WindowState {
    /// Reads the window geometry from `<config>/ui.json` (`window` section).
    pub fn load(config_dir: &Path) -> Self {
        let mut state = WindowState::default();
        let Some(value) = read(config_dir) else { return state };
        let Some(saved) = value.get("window") else { return state };
        let number = |key: &str| saved.get(key).and_then(|value| value.as_f64()).map(|value| value as f32);
        if let (Some(x), Some(y)) = (number("x"), number("y")) {
            state.x = x;
            state.y = y;
        }
        if let Some(width) = number("width") {
            state.width = width.clamp(640., 8192.);
        }
        if let Some(height) = number("height") {
            state.height = height.clamp(480., 8192.);
        }
        state.maximized = saved.get("maximized").and_then(|value| value.as_bool()).unwrap_or(false);
        state
    }

    /// Writes the geometry into `<config>/ui.json`, keeping the layout and theme
    /// sections intact.
    pub fn save(&self, config_dir: &Path) {
        let mut value = read(config_dir).unwrap_or_else(|| serde_json::json!({}));
        if !value.is_object() {
            value = serde_json::json!({});
        }
        let mut window = serde_json::json!({
            "width": self.width.round() as u32,
            "height": self.height.round() as u32,
            "maximized": self.maximized,
        });
        // `NaN` means "never positioned": leave x/y out so the next launch
        // centres the window instead of restoring a junk origin.
        if self.x.is_finite() && self.y.is_finite() {
            window["x"] = serde_json::json!(self.x.round() as i32);
            window["y"] = serde_json::json!(self.y.round() as i32);
        }
        value["window"] = window;
        if let Some(parent) = config_dir.join(FILE).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&value) {
            let _ = std::fs::write(config_dir.join(FILE), bytes);
        }
    }
}

const KEYS: [&str; 16] = [
    "problems",
    "terminal",
    "diff",
    "run",
    "debug",
    "database",
    "http",
    "notebook",
    "agents",
    "duplicates",
    "structural",
    "structure",
    "ssh",
    "deploy",
    "remote-host",
    "notifications",
];

fn read(config_dir: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(config_dir.join(FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod window_state_tests {
    use super::WindowState;

    #[test]
    fn a_fresh_install_has_no_position_and_the_default_size() {
        let state = WindowState::default();
        assert!(state.x.is_nan() && state.y.is_nan(), "no saved origin");
        assert_eq!(state.width, 1360.);
        assert_eq!(state.height, 880.);
    }

    #[test]
    fn geometry_round_trips_through_the_ui_file() {
        let dir = std::env::temp_dir().join(format!("openidea-window-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let saved = WindowState { x: 120., y: 64., width: 1400., height: 900., maximized: true };
        saved.save(&dir);
        let loaded = WindowState::load(&dir);
        assert_eq!(loaded.x, 120.);
        assert_eq!(loaded.y, 64.);
        assert_eq!(loaded.width, 1400.);
        assert_eq!(loaded.height, 900.);
        assert!(loaded.maximized);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_absurd_size_is_clamped_and_a_missing_origin_stays_centred() {
        let dir = std::env::temp_dir().join(format!("openidea-window-clamp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ui.json"),
            r#"{"window":{"width":10,"height":99999,"maximized":false}}"#,
        )
        .unwrap();
        let loaded = WindowState::load(&dir);
        assert_eq!(loaded.width, 640., "a tiny window grows to the minimum");
        assert_eq!(loaded.height, 8192., "an enormous window is capped");
        assert!(loaded.x.is_nan(), "x stays unset when only the size was saved");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod dock_state_tests {
    use super::Layout;

    #[test]
    fn a_dock_tree_round_trips_through_the_ui_file() {
        let dir = std::env::temp_dir().join(format!("openidea-dock-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut layout = Layout::default();
        layout.left_width = 300.;
        layout.dock = Some(serde_json::json!({"version": 1, "center": {"kind": "tabs", "panels": []}}));
        layout.save(&dir);
        let loaded = Layout::load(&dir);
        assert_eq!(loaded.left_width, 300.);
        let dock = loaded.dock.expect("the dock tree is restored");
        assert_eq!(dock["version"], 1);
        assert_eq!(dock["center"]["kind"], "tabs");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_layout_without_a_dock_tree_leaves_the_default() {
        let dir = std::env::temp_dir().join(format!("openidea-dock-default-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A file written before the dock tree existed.
        std::fs::write(dir.join("ui.json"), r#"{"layout":{"leftWidth":260,"bottomHeights":{}}}"#).unwrap();
        let loaded = Layout::load(&dir);
        assert_eq!(loaded.left_width, 260.);
        assert!(loaded.dock.is_none(), "no saved dock tree means the default tabs");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_theme_and_window_sections_survive_a_layout_save() {
        let dir = std::env::temp_dir().join(format!("openidea-layout-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ui.json"),
            r#"{"theme":"islands-dark","window":{"width":1200,"height":800}}"#,
        )
        .unwrap();
        Layout::default().save(&dir);
        let text = std::fs::read_to_string(dir.join("ui.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["theme"], "islands-dark");
        assert_eq!(value["window"]["width"], 1200);
        assert!(value["layout"]["leftWidth"].is_number());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
