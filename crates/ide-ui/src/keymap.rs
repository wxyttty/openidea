//! Keymap editor: the workbench commands, their current shortcut, and a dialog to
//! rebind them.
//!
//! Shortcuts live in `settings.keymap_overrides` (`action id → keystrokes`), which
//! the workbench applies on top of the `keymap` preset — the same shape the Tauri
//! shell wrote, so both UIs read one file. The editor lists the commands the
//! native workbench implements (there is no point offering an action that cannot
//! run), shows the keystroke that answers today from GPUI's own keymap, and takes
//! a new chord as a plain string. An empty string unbinds, as IDEA's "remove
//! shortcut" does.
//!
//! Applying a change re-binds GPUI's keymap through `KeyBinding::load`, so the
//! shortcut works immediately rather than after a restart.

use crate::theme::IdeColors as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::BTreeMap;
use std::rc::Rc;

/// One command the editor can rebind: its settings id, the label the menus use,
/// and the action it dispatches.
pub struct KeyCommand {
    pub id: &'static str,
    pub label: &'static str,
    action: Rc<dyn Action>,
}

impl KeyCommand {
    fn new(id: &'static str, label: &'static str, action: impl Action + 'static) -> Self {
        Self { id, label, action: Rc::new(action) }
    }
}

/// Every command with a default binding in `actions::install`, in menu order.
///
/// This is the list the dialog offers: an action bound in the keymap but absent
/// here cannot be reached from the UI, and showing it would be a dead row.
pub fn commands() -> Vec<KeyCommand> {
    use crate::actions::*;
    vec![
        KeyCommand::new("file.openProject", "Open Project…", OpenProject),
        KeyCommand::new("file.save", "Save", SaveFile),
        KeyCommand::new("file.saveAll", "Save All", SaveAll),
        KeyCommand::new("file.close", "Close Tab", CloseTab),
        KeyCommand::new("file.settings", "Settings…", Settings),
        KeyCommand::new("edit.findInFile", "Find in File", FindInEditor),
        KeyCommand::new("edit.rename", "Rename Symbol", Rename),
        KeyCommand::new("edit.intentions", "Show Context Actions", Intentions),
        KeyCommand::new("go.toLine", "Go to Line…", GoToLine),
        KeyCommand::new("go.file", "Go to File…", QuickOpen),
        KeyCommand::new("go.symbol", "Command Palette…", CommandPalette),
        KeyCommand::new("search.findInPath", "Find in Path…", FindInPath),
        KeyCommand::new("view.toggleProject", "Toggle Project Tool Window", ToggleProject),
        KeyCommand::new("view.toggleTerminal", "Toggle Terminal", ToggleTerminal),
        KeyCommand::new("view.toggleProblems", "Toggle Problems", ToggleProblems),
        KeyCommand::new("view.terminal", "Terminal", ShowProject),
        KeyCommand::new("view.run", "Run Tool Window", ShowRun),
        KeyCommand::new("view.database", "Database", ShowDb),
        KeyCommand::new("view.http", "HTTP Client", ShowHttp),
        KeyCommand::new("view.notebook", "Notebook", ShowNotebook),
        KeyCommand::new("view.agents", "Agents", ShowAgents),
        KeyCommand::new("view.ssh", "SSH Connections", ShowSsh),
        KeyCommand::new("view.duplicates", "Locate Duplicates", ShowDuplicates),
        KeyCommand::new("view.structural", "Search Structurally", ShowStructural),
        KeyCommand::new("view.structure", "Structure", ShowStructure),
        KeyCommand::new("run.run", "Run", Run),
        KeyCommand::new("run.stop", "Stop", StopRun),
        KeyCommand::new("debug.debug", "Debug", Debug),
        KeyCommand::new("debug.resume", "Resume", ResumeDebug),
        KeyCommand::new("debug.stepInto", "Step Into", StepInto),
        KeyCommand::new("debug.stepOver", "Step Over", StepOver),
        KeyCommand::new("debug.stepOut", "Step Out", StepOut),
        KeyCommand::new("debug.toggleBreakpoint", "Toggle Breakpoint", ToggleBreakpoint),
        KeyCommand::new("debug.viewBreakpoints", "View Breakpoints", ViewBreakpoints),
        KeyCommand::new("http.runAtCaret", "Run HTTP Request at Caret", RunHttpAtCaret),
        KeyCommand::new("file.quit", "Quit", Quit),
        KeyCommand::new("help.about", "About OpenIDEA", About),
        KeyCommand::new("app.nextTab", "Next Tab", NextTab),
        KeyCommand::new("app.prevTab", "Previous Tab", PrevTab),
        KeyCommand::new("app.refreshProject", "Refresh Project", RefreshProject),
        KeyCommand::new("app.toggleTheme", "Toggle Theme", ToggleTheme),
    ]
}

/// The keystroke that answers `action` today, if any (the last binding wins, as
/// GPUI resolves it).
/// The host OS, for formatting keystrokes the way the user's keyboard shows them.
fn host_os() -> OperatingSystem {
    if cfg!(target_os = "macos") {
        OperatingSystem::Mac
    } else if cfg!(target_os = "windows") {
        OperatingSystem::Windows
    } else if cfg!(target_os = "freebsd") {
        OperatingSystem::FreeBsd
    } else {
        OperatingSystem::Linux
    }
}

fn current_keystroke(cx: &App, action: &dyn Action) -> Option<String> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let os = host_os();
    keymap
        .bindings_for_action(action)
        .last()
        .map(|binding| binding.keystrokes().iter().map(|keystroke| keystroke.unparse(os)).collect::<Vec<_>>().join(" "))
}

/// One row of the editor: a command, its current shortcut and the override (if
/// the user already changed it).
#[derive(Clone)]
pub struct KeyRow {
    pub id: &'static str,
    pub label: &'static str,
    pub current: Option<String>,
    pub overridden: bool,
}

/// The rows for the settings dialog.
pub fn rows(cx: &App, overrides: &BTreeMap<String, Vec<String>>) -> Vec<KeyRow> {
    commands()
        .into_iter()
        .map(|command| {
            let override_keys = overrides.get(command.id);
            KeyRow {
                id: command.id,
                label: command.label,
                current: match override_keys {
                    Some(keys) => keys.first().cloned(),
                    None => current_keystroke(cx, command.action.as_ref()),
                },
                overridden: override_keys.is_some(),
            }
        })
        .collect()
}

/// The keystroke a command answers today, from a plain `&App`.
fn current_keystroke_from(cx: &App, action: &dyn Action) -> Option<String> {
    current_keystroke(cx, action)
}

/// Applies one override: binds `keys` to the command (after removing its previous
/// overrides) and writes it into `settings.keymap_overrides`.
///
/// `keys` is a GPUI keystroke string (`secondary-shift-o`, chords separated by
/// spaces); empty removes every binding for the command.
pub fn apply_override(cx: &mut App, command_id: &str, keys: &str) -> Result<(), String> {
    let command =
        commands().into_iter().find(|command| command.id == command_id).ok_or_else(|| {
            format!("unknown command {command_id}")
        })?;
    let predicate = Some(Rc::new(
        KeyBindingContextPredicate::parse(crate::actions::CONTEXT).map_err(|err| format!("{err:?}"))?,
    ));
    let mut bindings = Vec::new();
    if keys.trim().is_empty() {
        // Unbind: the settings file keeps an empty list, and the live keymap gets
        // `Unbind` bindings for the shortcuts the command answers today.
        if let Some(current) = current_keystroke_from(cx, command.action.as_ref()) {
            for keystroke in current.split_whitespace() {
                if let Ok(binding) =
                    KeyBinding::load(keystroke, Box::new(Unbind(keystroke.into())), predicate.clone(), false, None, cx)
                {
                    bindings.push(binding);
                }
            }
        }
    } else {
        for keystroke in keys.split_whitespace() {
            let binding = KeyBinding::load(keystroke, command.action.boxed_clone(), predicate.clone(), false, None, cx)
                .map_err(|_| format!("invalid keystroke {keystroke}"))?;
            bindings.push(binding);
        }
    }
    cx.bind_keys(bindings);
    Ok(())
}

/// The values to write into `settings.keymap_overrides` for one command.
pub fn override_value(keys: &str) -> Vec<String> {
    keys.split_whitespace().map(str::to_owned).collect()
}

/// A dialog that takes a new shortcut for one command.
pub struct KeymapDialog {
    pub command: Option<&'static str>,
    pub label: SharedString,
    pub input: Entity<InputState>,
    pub error: Option<SharedString>,
    focus_handle: FocusHandle,
    _submit: Subscription,
}

impl KeymapDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("secondary-shift-o (blank unbinds)"));
        let submit = cx.subscribe(&input, |this: &mut Self, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                cx.emit(KeymapAccepted);
            }
        });
        Self {
            command: None,
            label: "".into(),
            input,
            error: None,
            focus_handle: cx.focus_handle(),
            _submit: submit,
        }
    }

    /// Opens the dialog for `command`, prefilled with its current shortcut.
    pub fn open(
        &mut self,
        command: &'static str,
        label: SharedString,
        current: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command = Some(command);
        self.label = label;
        self.error = None;
        self.input.update(cx, |input, cx| {
            input.set_value(current.clone().unwrap_or_default(), window, cx)
        });
        let focus = self.input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    pub fn keys(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_owned()
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.command = None;
        cx.notify();
    }

    pub fn is_open(&self) -> bool {
        self.command.is_some()
    }
}

/// Emitted when the user pressed Enter: the workbench applies the override.
pub struct KeymapAccepted;

impl EventEmitter<KeymapAccepted> for KeymapDialog {}

impl Focusable for KeymapDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for KeymapDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme: Theme = cx.theme().clone();
        div()
            .id("keymap-dialog")
            .w(px(460.))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border_strong())
            .rounded(px(8.))
            .shadow_xl()
            .occlude()
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.foreground)
                    .child(format!("Shortcut for {}", self.label)),
            )
            .child(Input::new(&self.input).id("keymap-input"))
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(theme.fg_muted())
                    .child("Enter to apply · blank removes the shortcut · Esc cancels"),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_size(px(10.5)).text_color(theme.danger).child(error))
            })
    }
}

/// Keys that GPUI accepts, for validating what the user typed before the keymap
/// API sees it. Chords are space-separated, each `modifier-modifier-key`.
pub fn validate_keystroke(keys: &str) -> Result<(), String> {
    if keys.trim().is_empty() {
        return Ok(());
    }
    let known_modifiers = ["secondary", "ctrl", "alt", "shift", "cmd", "super", "fn"];
    for chord in keys.split_whitespace() {
        let parts: Vec<&str> = chord.split('-').collect();
        let Some(key) = parts.last() else { return Err(format!("missing key in {chord}")) };
        if key.is_empty() {
            return Err(format!("missing key in {chord}"));
        }
        for modifier in &parts[..parts.len() - 1] {
            if !known_modifiers.contains(modifier) {
                return Err(format!("unknown modifier {modifier}"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{override_value, validate_keystroke};

    #[test]
    fn valid_chords_pass_and_unknown_modifiers_fail() {
        assert!(validate_keystroke("").is_ok(), "blank is the unbind case");
        assert!(validate_keystroke("secondary-s").is_ok());
        assert!(validate_keystroke("ctrl-alt-up").is_ok());
        assert!(validate_keystroke("secondary-k secondary-p").is_ok());
        assert!(validate_keystroke("hyper-s").is_err(), "unknown modifiers are rejected");
        assert!(validate_keystroke("ctrl-").is_err(), "a dangling modifier has no key");
    }

    #[test]
    fn an_override_is_one_string_per_chord() {
        assert_eq!(override_value("secondary-k secondary-p"), vec!["secondary-k", "secondary-p"]);
        assert!(override_value("   ").is_empty(), "blank clears the override");
    }
}
