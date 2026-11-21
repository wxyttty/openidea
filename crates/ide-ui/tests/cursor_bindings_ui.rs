//! Editor multi-cursor keybindings: `Ctrl+Alt+Up/Down` (IDEA's) add a caret
//! above/below. `gpui-base` implements the actions but ships no keystrokes for
//! them, so the workbench installs them for the editor's key context.
#![recursion_limit = "512"]

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, TestAppContext,
    Window, WindowOptions, div,
};
use ide_ui::actions;

struct Host;

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}

#[gpui_kit::test]
fn editor_keymap_has_multicursor_keystrokes(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        actions::install(cx);
    });
    cx.update(|cx| {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        let keystrokes: Vec<String> = keymap
            .bindings()
            .flat_map(|binding| binding.keystrokes().iter().map(|k| k.key().to_owned()))
            .collect();
        let alt_up = keymap.bindings().any(|binding| {
            binding.keystrokes().iter().any(|k| {
                k.key() == "up" && k.modifiers().control && k.modifiers().alt
            })
        });
        let alt_down = keymap.bindings().any(|binding| {
            binding.keystrokes().iter().any(|k| {
                k.key() == "down" && k.modifiers().control && k.modifiers().alt
            })
        });
        assert!(alt_up, "Ctrl+Alt+Up is bound: {keystrokes:?}");
        assert!(alt_down, "Ctrl+Alt+Down is bound: {keystrokes:?}");

    });
}

#[gpui_kit::test]
fn multicursor_actions_dispatch_without_panicking(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        actions::install(cx);
    });
    let handle = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions { show: false, ..Default::default() },
            cx,
            |_, cx| cx.new(|_| Host),
        )
        .expect("open window")
        .0
        .into()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(gpui_kit::base::input::AddCursorBelow), cx);
        window.dispatch_action(Box::new(gpui_kit::base::input::AddCursorAbove), cx);
        window.render_frame(cx);
    })
    .unwrap();
}
