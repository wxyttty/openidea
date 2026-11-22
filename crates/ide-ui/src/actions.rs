//! Workbench actions and their key bindings.
//!
//! Actions are dispatched through GPUI's keymap from the `Workspace` key
//! context, which the workbench root sets. The same actions drive the command
//! palette and the menu bar.

use gpui_kit::*;
use std::rc::Rc;

/// Key context of the workbench root; every binding below is scoped to it so
/// editor-local bindings (Ctrl+S in a text field, say) keep precedence.
pub const CONTEXT: &str = "Workspace";

gpui_kit::actions!(
    openidea,
    [
        Quit,
        About,
        OpenProject,
        SaveFile,
        SaveAll,
        CloseTab,
        QuickOpen,
        FindInPath,
        ShowProject,
        ShowSearch,
        ShowVcs,
        NextTab,
        PrevTab,
        ToggleProject,
        ToggleProblems,
        ToggleTerminal,
        CommandPalette,
        RefreshProject,
        ToggleTheme,
        Run,
        StopRun,
        ShowRun,
        ShowDb,
        ShowHttp,
        ShowNotebook,
        ShowAgents,
        ShowNotifications,
        RunHttpAtCaret,
        Debug,
        ShowDebug,
        StopDebug,
        ResumeDebug,
        PauseDebug,
        StepInto,
        StepOver,
        StepOut,
        ToggleBreakpoint,
        ViewBreakpoints,
        Settings,
        GoToLine,
        FindInEditor,
        CloseFind,
        Intentions,
        Rename,
        ShowDuplicates,
        ShowStructural,
        ShowStructure,
        ShowSsh,
        ShowDeploy,
        ShowRemoteHost,
        CheckUpdates,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        Undo,
        Redo,
        Cut,
        Copy,
        Paste,
        SelectAll,
    ]
);

pub fn install(cx: &mut App) {
    // `KeyBinding::load` resolves the platform-neutral `secondary-` modifier
    // (Cmd on macOS, Ctrl elsewhere); `KeyBinding::new` does not.
    let predicate = Some(Rc::new(KeyBindingContextPredicate::parse(CONTEXT).expect("valid context")));
    let mut bindings = Vec::new();
    let mut bind = |keystroke: &str, action: Box<dyn Action>| {
        if let Ok(binding) = KeyBinding::load(keystroke, action, predicate.clone(), false, None, cx) {
            bindings.push(binding);
        } else {
            tracing::warn!("invalid keybinding {keystroke}");
        }
    };
    bind("secondary-q", Box::new(Quit));
    bind("secondary-o", Box::new(OpenProject));
    bind("secondary-s", Box::new(SaveFile));
    bind("secondary-w", Box::new(CloseTab));
    bind("secondary-z", Box::new(Undo));
    bind("secondary-shift-z", Box::new(Redo));
    bind("secondary-x", Box::new(Cut));
    bind("secondary-c", Box::new(Copy));
    bind("secondary-v", Box::new(Paste));
    bind("secondary-a", Box::new(SelectAll));
    bind("secondary-shift-p", Box::new(CommandPalette));
    bind("secondary-p", Box::new(QuickOpen));
    bind("secondary-shift-f", Box::new(FindInPath));
    bind("secondary-shift-s", Box::new(SaveAll));
    bind("ctrl-tab", Box::new(NextTab));
    bind("ctrl-shift-tab", Box::new(PrevTab));
    bind("alt-1", Box::new(ToggleProject));
    bind("alt-2", Box::new(ToggleTerminal));
    bind("alt-6", Box::new(ToggleProblems));
    bind("f5", Box::new(Run));
    bind("shift-f5", Box::new(StopRun));
    bind("alt-4", Box::new(ShowRun));
    bind("alt-3", Box::new(ShowDb));
    bind("alt-7", Box::new(ShowHttp));
    bind("alt-8", Box::new(ShowNotebook));
    bind("alt-9", Box::new(ShowAgents));
    bind("ctrl-alt-r", Box::new(RunHttpAtCaret));
    bind("shift-f9", Box::new(Debug));
    bind("alt-5", Box::new(ShowDebug));
    bind("ctrl-f2", Box::new(StopDebug));
    bind("f9", Box::new(ResumeDebug));
    bind("f7", Box::new(StepInto));
    bind("f8", Box::new(StepOver));
    bind("shift-f8", Box::new(StepOut));
    bind("ctrl-f8", Box::new(ToggleBreakpoint));
    bind("ctrl-shift-f8", Box::new(ViewBreakpoints));
    bind("ctrl-alt-s", Box::new(Settings));
    bind("secondary-g", Box::new(GoToLine));
    bind("secondary-f", Box::new(FindInEditor));
    bind("escape", Box::new(CloseFind));
    bind("alt-enter", Box::new(Intentions));
    bind("alt-shift-f6", Box::new(Rename));
    bind("secondary-equal", Box::new(ZoomIn));
    bind("secondary-minus", Box::new(ZoomOut));
    bind("secondary-0", Box::new(ZoomReset));
    cx.bind_keys(bindings);

    // Editor-local bindings. `gpui-base` implements multi-cursor (Alt+click,
    // AddCursorAbove/Below) but ships no keystrokes for them; IDEA's are
    // Ctrl+Alt+Up/Down. The editor's key context is `Input mode=editor`, and a
    // binding there wins over the workspace's, so plain typing stays intact.
    let editor = Some(Rc::new(
        KeyBindingContextPredicate::parse("Input").expect("valid editor context"),
    ));
    let mut editor_bindings = Vec::new();
    for (keystroke, action) in [
        ("ctrl-alt-up", Box::new(gpui_kit::base::input::AddCursorAbove) as Box<dyn Action>),
        ("ctrl-alt-down", Box::new(gpui_kit::base::input::AddCursorBelow) as Box<dyn Action>),
    ] {
        match KeyBinding::load(keystroke, action, editor.clone(), false, None, cx) {
            Ok(binding) => editor_bindings.push(binding),
            Err(err) => tracing::warn!("invalid editor keybinding {keystroke}: {err}"),
        }
    }
    cx.bind_keys(editor_bindings);
}
