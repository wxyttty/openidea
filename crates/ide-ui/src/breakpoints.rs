//! Line breakpoints, persisted per project and mirrored to the debugger.
//!
//! The Tauri frontend kept them in `localStorage`; the native shell stores the
//! same shape in `<config>/breakpoints.json`, keyed by project root, so they
//! survive restarts without writing into the project. Entries carry a
//! condition/enabled flag; the legacy number-only format still loads.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FILE: &str = "breakpoints.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BreakpointEntry {
    /// 0-based line.
    pub line: u32,
    pub enabled: bool,
    pub condition: Option<String>,
    pub log_message: Option<String>,
}

impl BreakpointEntry {
    fn new(line: u32) -> Self {
        Self { line, enabled: true, condition: None, log_message: None }
    }
}

#[derive(Default)]
pub struct Breakpoints {
    root: PathBuf,
    by_file: BTreeMap<PathBuf, Vec<BreakpointEntry>>,
}

impl Breakpoints {
    /// Loads the breakpoints of `root`; a missing or damaged file yields none.
    pub fn load(config_dir: &Path, root: &Path) -> Self {
        let mut breakpoints = Breakpoints { root: root.to_path_buf(), by_file: BTreeMap::new() };
        let Some(all) = read_all(config_dir) else { return breakpoints };
        let Some(files) = all.get(root.to_string_lossy().as_ref()).and_then(|files| files.as_object()) else {
            return breakpoints;
        };
        for (relative, saved) in files {
            let Some(saved) = saved.as_array() else { continue };
            let path = root.join(relative);
            let mut entries: Vec<BreakpointEntry> = saved
                .iter()
                .filter_map(|value| match value {
                    serde_json::Value::Number(number) => {
                        number.as_u64().filter(|line| *line > 0).map(|line| BreakpointEntry::new((line - 1) as u32))
                    }
                    serde_json::Value::Object(entry) => {
                        let line = entry.get("line")?.as_u64().filter(|line| *line > 0)?;
                        let mut breakpoint = BreakpointEntry::new((line - 1) as u32);
                        breakpoint.enabled = entry.get("enabled").and_then(|value| value.as_bool()).unwrap_or(true);
                        breakpoint.condition = text(entry.get("condition"));
                        breakpoint.log_message = text(entry.get("logMessage"));
                        Some(breakpoint)
                    }
                    _ => None,
                })
                .collect();
            entries.sort_by_key(|entry| entry.line);
            entries.dedup_by_key(|entry| entry.line);
            if !entries.is_empty() {
                breakpoints.by_file.insert(path, entries);
            }
        }
        breakpoints
    }

    pub fn save(&self, config_dir: &Path) {
        let mut all = read_all(config_dir).unwrap_or_default();
        let files = self
            .by_file
            .iter()
            .filter(|(_, entries)| !entries.is_empty())
            .map(|(path, entries)| {
                let relative = path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy().into_owned();
                let entries = entries
                    .iter()
                    .map(|entry| {
                        let mut object = serde_json::Map::new();
                        object.insert("line".into(), serde_json::Value::from(entry.line as u64 + 1));
                        object.insert("enabled".into(), serde_json::Value::from(entry.enabled));
                        if let Some(condition) = entry.condition.as_deref().filter(|c| !c.trim().is_empty()) {
                            object.insert("condition".into(), serde_json::Value::from(condition));
                        }
                        if let Some(message) = entry.log_message.as_deref().filter(|m| !m.trim().is_empty()) {
                            object.insert("logMessage".into(), serde_json::Value::from(message));
                        }
                        serde_json::Value::Object(object)
                    })
                    .collect();
                (relative, serde_json::Value::Array(entries))
            })
            .collect::<serde_json::Map<_, _>>();
        if files.is_empty() {
            all.remove(self.root.to_string_lossy().as_ref());
        } else {
            all.insert(self.root.to_string_lossy().into_owned(), serde_json::Value::Object(files));
        }
        if let Some(parent) = config_dir.join(FILE).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&serde_json::Value::Object(all)) {
            let _ = std::fs::write(config_dir.join(FILE), bytes);
        }
    }

    /// Enabled lines by file, for the editor's line highlights.
    pub fn map(&self) -> BTreeMap<PathBuf, Vec<u32>> {
        self.by_file
            .iter()
            .map(|(path, entries)| {
                (path.clone(), entries.iter().filter(|entry| entry.enabled).map(|entry| entry.line).collect())
            })
            .collect()
    }

    pub fn entries(&self, path: &Path) -> Vec<BreakpointEntry> {
        self.by_file.get(path).cloned().unwrap_or_default()
    }

    pub fn all(&self) -> Vec<(PathBuf, BreakpointEntry)> {
        self.by_file
            .iter()
            .flat_map(|(path, entries)| entries.iter().cloned().map(move |entry| (path.clone(), entry)))
            .collect()
    }

    pub fn find(&self, path: &Path, line: u32) -> Option<&BreakpointEntry> {
        self.by_file.get(path)?.iter().find(|entry| entry.line == line)
    }

    /// Adds or removes a breakpoint; returns `true` when it is now set.
    pub fn toggle(&mut self, path: &Path, line: u32) -> bool {
        let entries = self.by_file.entry(path.to_path_buf()).or_default();
        if let Some(index) = entries.iter().position(|entry| entry.line == line) {
            entries.remove(index);
            if entries.is_empty() {
                self.by_file.remove(path);
            }
            false
        } else {
            let index = entries.partition_point(|entry| entry.line < line);
            entries.insert(index, BreakpointEntry::new(line));
            true
        }
    }

    pub fn remove(&mut self, path: &Path, line: u32) {
        if let Some(entries) = self.by_file.get_mut(path) {
            entries.retain(|entry| entry.line != line);
            if entries.is_empty() {
                self.by_file.remove(path);
            }
        }
    }

    pub fn set_condition(&mut self, path: &Path, line: u32, condition: Option<String>) {
        if let Some(entry) =
            self.by_file.get_mut(path).and_then(|entries| entries.iter_mut().find(|entry| entry.line == line))
        {
            entry.condition = condition.filter(|condition| !condition.trim().is_empty());
        }
    }

    pub fn set_log_message(&mut self, path: &Path, line: u32, message: Option<String>) {
        if let Some(entry) =
            self.by_file.get_mut(path).and_then(|entries| entries.iter_mut().find(|entry| entry.line == line))
        {
            entry.log_message = message.filter(|message| !message.trim().is_empty());
        }
    }

    pub fn set_enabled(&mut self, path: &Path, line: u32, enabled: bool) {
        if let Some(entry) =
            self.by_file.get_mut(path).and_then(|entries| entries.iter_mut().find(|entry| entry.line == line))
        {
            entry.enabled = enabled;
        }
    }

    pub fn count(&self) -> usize {
        self.by_file.values().map(Vec::len).sum()
    }
}

fn text(value: Option<&serde_json::Value>) -> Option<String> {
    value.and_then(|value| value.as_str()).filter(|value| !value.trim().is_empty()).map(str::to_owned)
}

fn read_all(config_dir: &Path) -> Option<serde_json::Map<String, serde_json::Value>> {
    let bytes = std::fs::read(config_dir.join(FILE)).ok()?;
    match serde_json::from_slice::<serde_json::Value>(&bytes).ok()? {
        serde_json::Value::Object(map) => Some(map),
        _ => None,
    }
}
