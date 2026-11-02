//! Persisted workbench layout: left tool-window width and per-tab bottom heights.
//!
//! Stored next to the theme in `<config>/ui.json` under `layout`, so resizing
//! panels survives restarts without touching project files.

use std::path::Path;

const FILE: &str = "ui.json";

#[derive(Clone, Copy)]
pub struct Layout {
    pub left_width: f32,
    /// Problems, Terminal, Diff, Run, Debug, Database, HTTP, Notebook, Agents,
    /// Notifications. Indices are stable; new tabs append at the end so saved
    /// layouts keep their meaning.
    pub heights: [f32; 10],
}

impl Default for Layout {
    fn default() -> Self {
        Self { left_width: 280., heights: [180., 366., 420., 320., 420., 420., 420., 420., 420., 320.] }
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
        value["layout"] = serde_json::json!({
            "leftWidth": self.left_width.round() as u32,
            "bottomHeights": heights,
        });
        if let Some(parent) = config_dir.join(FILE).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&value) {
            let _ = std::fs::write(config_dir.join(FILE), bytes);
        }
    }
}

const KEYS: [&str; 10] = [
    "problems",
    "terminal",
    "diff",
    "run",
    "debug",
    "database",
    "http",
    "notebook",
    "agents",
    "notifications",
];

fn read(config_dir: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(config_dir.join(FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}
