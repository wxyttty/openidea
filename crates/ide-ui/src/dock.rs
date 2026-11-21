//! Dockable wrapper for the workbench's tool windows.
//!
//! `gpui-base`'s dock (`DockArea`) knows where a panel sits, how it drags, and
//! how the layout persists; a panel only provides a name, a title and its
//! content. The workbench's tool windows are ordinary entities
//! (`ProblemsPanel`, `TerminalPanel`, …) that predate the dock, so
//! [`ToolPanel`] adapts one without touching its own code: it carries the title,
//! the tab label and the focus handle the dock asks for, and renders the wrapped
//! entity.

use gpui_kit::assets::IconName;
use gpui_kit::base::dock::PanelEvent;
use gpui_kit::component::dock::{Panel, PanelControl, PanelStyle};
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::*;

/// One tool window in the dock: a title, a stable panel name, and the entity
/// that draws it.
pub struct ToolPanel<T: Render + 'static> {
    /// Stable across sessions; persisted layouts refer to it.
    name: &'static str,
    title: SharedString,
    icon: IconName,
    content: Entity<T>,
    focus_handle: FocusHandle,
    closable: bool,
    style: PanelStyle,
}

impl<T: Render + 'static> ToolPanel<T> {
    pub fn new(
        name: &'static str,
        title: impl Into<SharedString>,
        icon: IconName,
        content: Entity<T>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            name,
            title: title.into(),
            icon,
            content,
            focus_handle: cx.focus_handle(),
            closable: true,
            style: PanelStyle::Auto,
        }
    }

    /// Panels that carry their own chrome (a terminal grid, the problems list)
    /// keep the tab bar off.
    pub fn with_style(mut self, style: PanelStyle) -> Self {
        self.style = style;
        self
    }

    pub fn not_closable(mut self) -> Self {
        self.closable = false;
        self
    }

    pub fn content(&self) -> &Entity<T> {
        &self.content
    }
}

impl<T: Render + 'static> EventEmitter<PanelEvent> for ToolPanel<T> {}

impl<T: Render + 'static> Focusable for ToolPanel<T> {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl<T: Render + 'static> gpui_kit::base::dock::Panel for ToolPanel<T> {
    fn panel_name(&self) -> &'static str {
        self.name
    }

    fn closable(&self, _: &App) -> bool {
        self.closable
    }
}

impl<T: Render + 'static> Panel for ToolPanel<T> {
    fn tab_name(&self, _cx: &App) -> Option<SharedString> {
        Some(self.title.clone())
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let icon = self.icon;
        div().flex().items_center().gap_1().child(Icon::new(icon).xsmall()).child(self.title.clone())
    }

    fn zoom_control(&self, _cx: &App) -> Option<PanelControl> {
        Some(PanelControl::Menu)
    }

    fn inner_padding(&self, _cx: &App) -> bool {
        false
    }

    fn title_bar(&self, _cx: &App) -> bool {
        matches!(self.style, PanelStyle::TabBar)
    }
}

impl<T: Render + 'static> Render for ToolPanel<T> {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().min_h_0().child(self.content.clone())
    }
}
