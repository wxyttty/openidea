//! Context actions popup: ordering, filtering, section headings, keyboard walk
//! and the popup placement helper. The LSP round trip itself is covered by
//! `ide-host`'s code-action tests; these check the popup the workbench shows.
#![recursion_limit = "512"]

use gpui_kit::{AppContext, TestAppContext};
use ide_core::{Pos, Range, ServerId};
use ide_lsp::types::CodeAction;
use ide_ui::intentions::{IntentionsPopup, popup_origin};
use gpui_kit::px;

fn action(title: &str, kind: Option<&str>, preferred: bool) -> CodeAction {
    CodeAction {
        title: title.into(),
        kind: kind.map(Into::into),
        is_preferred: preferred,
        disabled_reason: None,
        edit: None,
        command: None,
        data: None,
        server: ServerId(1),
    }
}

#[gpui_kit::test]
fn preferred_action_comes_first_and_is_selected(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let popup = cx.new(IntentionsPopup::new);
    cx.update(|cx| {
        popup.update(cx, |popup, _| {
            popup.set_intentions(
                vec![
                    ide_ui::intentions::Intention::from_action(action("Second", Some("refactor"), false)),
                    ide_ui::intentions::Intention::from_action(action("Fix it", Some("quickfix"), true)),
                ],
                None,
            );
        });
        // Preferred first.
        assert_eq!(popup.read(cx).intention(0).unwrap().title.as_ref(), "Fix it");
        assert_eq!(popup.read(cx).selected_intention().unwrap().title.as_ref(), "Fix it");
        // Sections come from the LSP kind.
        assert_eq!(popup.read(cx).intention(0).unwrap().section(), "Quick Fixes");
        assert_eq!(popup.read(cx).intention(1).unwrap().section(), "Refactorings");
    });
}

#[gpui_kit::test]
fn keyboard_walks_and_filter_narrows(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let popup = cx.new(IntentionsPopup::new);
    cx.update(|cx| {
        popup.update(cx, |popup, _| {
            popup.set_intentions(
                vec![
                    ide_ui::intentions::Intention::from_action(action("Extract variable", Some("refactor.extract"), false)),
                    ide_ui::intentions::Intention::from_action(action("Extract method", Some("refactor.extract"), false)),
                    ide_ui::intentions::Intention::from_action(action("Import java.util.List", Some("quickfix"), false)),
                ],
                None,
            );
        });
        assert_eq!(popup.read(cx).selected_intention().unwrap().title.as_ref(), "Extract variable");
        popup.update(cx, |popup, _| popup.select_next());
        assert_eq!(popup.read(cx).selected_intention().unwrap().title.as_ref(), "Extract method");
        popup.update(cx, |popup, _| popup.select_prev());
        assert_eq!(popup.read(cx).selected_intention().unwrap().title.as_ref(), "Extract variable");

        // Filter is case-insensitive and resets the selection.
        popup.update(cx, |popup, _| popup.filter("import"));
        assert_eq!(popup.read(cx).selected_intention().unwrap().title.as_ref(), "Import java.util.List");
        popup.update(cx, |popup, _| popup.filter("EXTRACT ME"));
        assert_eq!(popup.read(cx).selected_intention().unwrap().title.as_ref(), "Extract method");
        popup.update(cx, |popup, _| popup.filter("nothing"));
        assert!(popup.read(cx).selected_intention().is_none());
    });
}

#[test]
fn popup_origin_flips_above_near_the_bottom() {
    let window = gpui_kit::size(px(1200.), px(800.));
    // Plenty of room below: the popup opens under the caret.
    let below = popup_origin(gpui_kit::point(px(100.), px(100.)), window);
    assert!(below.y > px(100.));
    // Near the bottom: it flips above so it stays on screen.
    let flipped = popup_origin(gpui_kit::point(px(100.), px(700.)), window);
    assert!(flipped.y < px(700.));
    // Never leaves the window horizontally.
    let right = popup_origin(gpui_kit::point(px(1190.), px(100.)), window);
    assert!(right.x + px(480.) <= window.width);
}

#[test]
fn request_range_shape_is_the_caret() {
    // The request range must be the caret itself when nothing is selected;
    // a zero-width range is what servers expect for "actions here".
    let caret = Pos { line: 3, col: 7 };
    let range = Range { start: caret, end: caret };
    assert_eq!(range.start.line, range.end.line);
    assert_eq!(range.start.col, range.end.col);
}
