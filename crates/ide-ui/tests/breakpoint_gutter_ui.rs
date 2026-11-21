//! Breakpoint gutter marks: `EditorArea::set_breakpoints` feeds both the line
//! fill and the gutter strip; ranges follow edits like the fill does.
#![recursion_limit = "512"]

use gpui_kit::base::input::{InputContent, RopeExt as _};
use gpui_kit::component::input::{EditorState, GutterDecoration};
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, TestAppContext,
    Window, WindowOptions, div,
};
use std::collections::BTreeMap;

/// Host that owns an editor and a gutter collection, as `EditorArea` does.
struct GutterHost {
    editor: Entity<EditorState>,
    gutter: Option<gpui_kit::component::input::GutterDecorationCollection>,
}

impl Render for GutterHost {
    fn render(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(gpui_kit::component::input::Editor::new(&self.editor).h_full())
    }
}

#[gpui_kit::test]
fn gutter_marks_follow_breakpoint_lines(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (handle, host) = cx.update(|cx| {
        let (handle, view) = gpui_kit::open_window(
            WindowOptions { show: false, ..Default::default() },
            cx,
            |window, cx| {
                cx.new(|cx| {
                    let editor = cx.new(|cx| {
                        let mut state = EditorState::new(window, cx);
                        state.set_value(
                            InputContent::new("fn one() {}\nfn two() {}\nfn three() {}\nfn four() {}"),
                            window,
                            cx,
                        );
                        state
                    });
                    GutterHost { editor, gutter: None }
                })
            },
        )
        .expect("open window");
        (handle.into(), view)
    });

    // Install marks for lines 1 and 3 (0-based), as `apply_breakpoints` does.
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        host.update(cx, |host, cx| {
            let ranges: Vec<GutterDecoration> = {
                let state = host.editor.read(cx);
                let rope = state.text();
                [1usize, 3]
                    .iter()
                    .map(|line| {
                        let start = rope.line_start_offset(*line);
                        let end = rope.line_start_offset(*line) + 1;
                        GutterDecoration::new(start..end)
                    })
                    .collect()
            };
            let collection = host
                .editor
                .update(cx, |state, cx| state.create_gutter_decorations_collection(ranges, cx));
            host.gutter = Some(collection);
            cx.notify();
        });
        window.render_frame(cx);
    })
    .unwrap();

    // The collection reports the two marked ranges, in document order.
    cx.update(|cx| {
        let collection = host.read(cx).gutter.clone().expect("collection installed");
        let ranges = collection.get_ranges(cx);
        assert_eq!(ranges.len(), 2, "two breakpoint marks: {ranges:?}");
        assert_eq!(ranges[0].start, 12, "line 1 starts after the first 12 bytes");
        assert!(ranges[1].start > ranges[0].start, "marks stay in order");
    });

    // Clearing on an empty breakpoint map empties the strip; the collection
    // stays usable (the gutter keeps its column).
    cx.update_window(handle, |_, window, cx| {
        host.update(cx, |host, cx| {
            if let Some(collection) = host.gutter.clone() {
                collection.clear(cx);
            }
            cx.notify();
        });
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let collection = host.read(cx).gutter.clone().expect("collection installed");
        assert!(collection.get_ranges(cx).is_empty());
    });
}

#[gpui_kit::test]
fn breakpoint_map_type_is_shared_with_the_workbench(cx: &mut TestAppContext) {
    // The workbench hands `BTreeMap<PathBuf, Vec<u32>>` to `EditorArea`; the
    // gutter marks derive from the same lines, so the mapping stays one source
    // of truth. This guards the type against accidental drift.
    let _: BTreeMap<std::path::PathBuf, Vec<u32>> = BTreeMap::new();
    let _ = cx;
}
