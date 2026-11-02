//! Notification center: the history of backend `IdeEvent::Notify` messages.
//!
//! The backend reports transient messages (formatter missing, git credential
//! needed, environment problems, …) through `IdeEvent::Notify`. The log keeps
//! the newest 200 entries; the Notifications tool window lists them and the
//! status bar shows the unread count. Whether a notification is listed follows
//! the effective settings (`notifications.groups.system.log`), matching the
//! Tauri shell's NotificationService, which filed backend events under the
//! `system` group.

use crate::theme::IdeColors as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{TestSupportExt as _, *};
use ide_core::event::NotifyLevel;
use std::time::SystemTime;

/// How many entries the history keeps (the Tauri shell's `MAX_HISTORY`).
const MAX_HISTORY: usize = 200;

#[derive(Clone)]
pub struct NotificationItem {
    pub id: u64,
    pub level: NotifyLevel,
    pub title: SharedString,
    pub body: Option<SharedString>,
    pub time: SystemTime,
    pub read: bool,
}

impl NotificationItem {
    /// Relative age ("just now", "5m ago", "3h ago"): no timezone needed, so
    /// no time crate is pulled in for a meta label.
    fn age(&self) -> String {
        let minutes = self.time.elapsed().unwrap_or_default().as_secs() / 60;
        match minutes {
            0 => "just now".into(),
            1..=59 => format!("{minutes}m ago"),
            60..=1439 => format!("{}h ago", minutes / 60),
            _ => format!("{}d ago", minutes / 1440),
        }
    }
}

/// Newest-first notification history, rendered as the Notifications tool window.
pub struct NotificationLog {
    items: Vec<NotificationItem>,
    next_id: u64,
}

impl NotificationLog {
    pub fn new() -> Self {
        Self { items: Vec::new(), next_id: 0 }
    }

    /// Appends a notification. `read` marks it read on arrival (the tool
    /// window is visible). Older entries fall off past `MAX_HISTORY`.
    pub fn push(
        &mut self,
        level: NotifyLevel,
        title: impl Into<SharedString>,
        body: Option<impl Into<SharedString>>,
        read: bool,
    ) {
        self.next_id += 1;
        self.items.insert(
            0,
            NotificationItem {
                id: self.next_id,
                level,
                title: title.into(),
                body: body.map(Into::into),
                time: SystemTime::now(),
                read,
            },
        );
        self.items.truncate(MAX_HISTORY);
    }

    pub fn remove(&mut self, id: u64) {
        self.items.retain(|item| item.id != id);
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// The list was seen: everything is read.
    pub fn mark_all_read(&mut self) {
        for item in &mut self.items {
            item.read = true;
        }
    }

    pub fn unread(&self) -> usize {
        self.items.iter().filter(|item| !item.read).count()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// The history, newest first.
    pub fn items(&self) -> &[NotificationItem] {
        &self.items
    }
}

impl Render for NotificationLog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let count = self.items.len();
        let rows = std::rc::Rc::new(self.items.clone());
        let row_theme = theme.clone();
        let on_close: std::rc::Rc<dyn Fn(u64, &mut Window, &mut App)> =
            std::rc::Rc::new(cx.processor(|this: &mut Self, id: u64, _window, cx| {
                this.remove(id);
                cx.notify();
            }));
        let clear = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
            this.clear();
            cx.notify();
        });
        let clear_hover = theme.list_hover;

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(
                div()
                    .h(px(26.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_3()
                    .bg(theme.muted)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.fg_secondary())
                            .child("Notifications"),
                    )
                    .child(div().text_size(px(11.)).text_color(theme.fg_muted()).child(format!("{count} entries")))
                    .child(div().flex_1())
                    .when(count > 0, |this| {
                        this.child(
                            div()
                                .id("notifications-clear")
                                .test_support()
                                .h(px(18.))
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_1()
                                .rounded(px(3.))
                                .cursor_pointer()
                                .hover(move |this| this.bg(clear_hover))
                                .on_click(clear)
                                .text_size(px(10.))
                                .text_color(theme.fg_secondary())
                                .child(Icon::new(IconName::Trash).xsmall().text_color(theme.fg_muted()))
                                .child("Clear All"),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .when(rows.is_empty(), |this| {
                        this.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(12.))
                                .text_color(theme.fg_muted())
                                .child("No notifications"),
                        )
                    })
                    .when(!rows.is_empty(), |this| {
                        let rows = rows.clone();
                        // Variable-height entries (bodies wrap): a plain scroll
                        // container, not a fixed-row `uniform_list`.
                        this.child(
                            div()
                                .id("notifications-list")
                                .test_support()
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .p_1()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .children(
                                    rows.iter()
                                        .map(|item| render_notification_row(item, &row_theme, on_close.clone()))
                                        .collect::<Vec<AnyElement>>(),
                                ),
                        )
                    }),
            )
    }
}

/// One history entry; built eagerly in `render` (the scroll container takes
/// pre-built children, so no closure borrowing the log is needed).
fn render_notification_row(
    item: &NotificationItem,
    theme: &Theme,
    on_close: std::rc::Rc<dyn Fn(u64, &mut Window, &mut App)>,
) -> AnyElement {
    let (icon, color) = match item.level {
        NotifyLevel::Error => (IconName::CircleAlert, theme.danger),
        NotifyLevel::Warning => (IconName::TriangleAlert, theme.warning),
        NotifyLevel::Info => (IconName::Info, theme.info),
    };
    let id = item.id;
    let hover = theme.list_hover;
    div()
        .id(ElementId::Name(format!("notification-{id}").into()))
        .test_support()
        .flex_shrink_0()
        .w_full()
        .px_2()
        .py_1()
        .flex()
        .items_start()
        .gap_2()
        .rounded(px(3.))
        .hover(move |this| this.bg(hover))
        .child(div().flex().items_center().h(px(18.)).child(Icon::new(icon).xsmall().text_color(color)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(if item.read { theme.fg_secondary() } else { theme.foreground })
                        .when(!item.read, |this| this.font_weight(FontWeight::SEMIBOLD))
                        .truncate()
                        .child(item.title.clone()),
                )
                .when_some(item.body.clone(), |this, body| {
                    this.child(
                        div()
                            .text_size(px(11.))
                            .text_color(theme.fg_muted())
                            .max_w(px(720.))
                            .child(body),
                    )
                }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(10.5))
                .text_color(theme.fg_muted())
                .child(item.age()),
        )
        .child(
            div()
                .id(ElementId::Name(format!("notification-close-{id}").into()))
                .test_support()
                .h(px(18.))
                .w(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .cursor_pointer()
                .hover(move |this| this.bg(hover))
                .on_click(move |_: &ClickEvent, window, cx| on_close(id, window, cx))
                .child(Icon::new(IconName::X).xsmall().text_color(theme.fg_muted())),
        )
        .into_any_element()
}
