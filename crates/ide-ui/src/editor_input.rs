//! A small multi-line text editor entity, for settings JSON and other long text
//! the workbench edits in place.
//!
//! `InputState` is single-line, while `EditorState` is the code editor and needs
//! a highlight language. This wraps `EditorState` with the JSON highlighter and
//! exposes just what the settings page needs: read the text, replace it, focus.

use gpui_kit::component::input::{Editor, EditorState, InputContent};
use gpui_kit::*;

pub struct JsonEditor {
    state: Entity<EditorState>,
}

impl JsonEditor {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| {
            let mut state = EditorState::new(window, cx);
            state.set_highlighter("json", cx);
            state
        });
        Self { state }
    }

    /// Sets the text (used when the settings page opens).
    pub fn set_text(&mut self, text: impl Into<String>, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.into();
        self.state.update(cx, |state, cx| state.set_value(InputContent::new(text), window, cx));
    }

    /// The text as the user sees it.
    pub fn text(&self, cx: &App) -> String {
        self.state.read(cx).value().to_string()
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        let handle = self.state.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
    }

    pub fn state(&self) -> &Entity<EditorState> {
        &self.state
    }
}

impl Focusable for JsonEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.read(cx).focus_handle(cx)
    }
}

impl Render for JsonEditor {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Editor::new(&self.state).size_full()
    }
}
