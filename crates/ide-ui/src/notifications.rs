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
use std::time::{Duration, SystemTime};

/// How many entries the history keeps (the Tauri shell's `MAX_HISTORY`).
const MAX_HISTORY: usize = 200;

/// How many balloons stack up at once (the Tauri shell's `MAX_BALLOONS`).
pub const MAX_BALLOONS: usize = 3;

/// Info and warning balloons fade after this long; errors stay until dismissed
/// (the Tauri shell's `BALLOON_TIMEOUT`).
const BALLOON_TIMEOUT: Duration = Duration::from_secs(10);

/// Sticky balloons (and logged errors) fade after five minutes, as in IDEA.
const STICKY_BALLOON_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Balloon clock resolution: frequent enough to look continuous, cheap enough
/// to run while balloons are up.
pub const TICK: Duration = Duration::from_millis(250);

/// Settings › Notifications › group display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BalloonDisplay {
    Balloon,
    Sticky,
    None,
}

/// The effective notification preferences of the `system` group, which is where
/// backend `Notify` events land (the Tauri shell's `notificationPrefs`).
#[derive(Clone, Copy, Debug)]
pub struct NotificationPrefs {
    /// Show balloons at all (`notifications.balloons`).
    pub balloons: bool,
    pub display: BalloonDisplay,
    /// Also list it in the Notifications tool window (`…groups.system.log`).
    pub log: bool,
}

impl Default for NotificationPrefs {
    fn default() -> Self {
        Self { balloons: true, display: BalloonDisplay::Balloon, log: true }
    }
}

#[derive(Clone)]
pub struct NotificationItem {
    pub id: u64,
    pub level: NotifyLevel,
    pub title: SharedString,
    pub body: Option<SharedString>,
    pub time: SystemTime,
    pub read: bool,
    /// Listed in the Notifications tool window.
    pub logged: bool,
    /// Currently shown as a balloon.
    pub balloon: bool,
    /// Whether the balloon stays until dismissed (errors, sticky groups).
    pub sticky: bool,
    /// Time left before the balloon fades; `None` keeps it until dismissed.
    pub remaining: Option<Duration>,
    /// The pointer is over the balloon, so its clock is paused.
    pub hovered: bool,
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
    prefs: NotificationPrefs,
    /// Started the balloon clock (one foreground task per log).
    ticking: bool,
}

impl NotificationLog {
    pub fn new() -> Self {
        Self { items: Vec::new(), next_id: 0, prefs: NotificationPrefs::default(), ticking: false }
    }

    /// Applies the effective `system`-group preferences; called before pushing
    /// an event, so a settings change applies to the next notification.
    pub fn set_prefs(&mut self, prefs: NotificationPrefs) {
        self.prefs = prefs;
    }

    pub fn prefs(&self) -> NotificationPrefs {
        self.prefs
    }

    /// Appends a notification. `read` marks it read on arrival (the tool
    /// window is visible). Older entries fall off past `MAX_HISTORY`.
    ///
    /// With balloons enabled and the group's display not `None`, the entry is
    /// also shown as a balloon (at most `MAX_BALLOONS` at once, oldest first).
    pub fn push(
        &mut self,
        level: NotifyLevel,
        title: impl Into<SharedString>,
        body: Option<impl Into<SharedString>>,
    ) -> u64 {
        let prefs = self.prefs;
        let balloon = prefs.balloons && prefs.display != BalloonDisplay::None;
        // Errors stay up; `sticky` groups keep the balloon until dismissed.
        let sticky = prefs.display == BalloonDisplay::Sticky || matches!(level, NotifyLevel::Error);
        // Neither shown nor listed: dropped, as IDEA's "No popup" + no log.
        if !balloon && !prefs.log {
            self.next_id += 1;
            return self.next_id;
        }
        let timeout = if sticky {
            prefs.log.then_some(STICKY_BALLOON_TIMEOUT)
        } else {
            Some(BALLOON_TIMEOUT)
        };
        self.next_id += 1;
        let id = self.next_id;
        self.items.insert(
            0,
            NotificationItem {
                id,
                level,
                title: title.into(),
                body: body.map(Into::into),
                time: SystemTime::now(),
                read: false,
                logged: prefs.log,
                balloon,
                sticky,
                remaining: balloon.then_some(timeout).flatten(),
                hovered: false,
            },
        );
        self.items.truncate(MAX_HISTORY);
        self.trim_balloons();
        id
    }

    /// Only the newest `MAX_BALLOONS` balloons stay up.
    fn trim_balloons(&mut self) {
        let mut shown = 0;
        for item in &mut self.items {
            if item.balloon {
                shown += 1;
                if shown > MAX_BALLOONS {
                    item.balloon = false;
                }
            }
        }
    }

    /// Advances every visible balloon's clock: paused while the pointer is on
    /// it or the window is inactive (IDEA's smart fade-out).
    pub fn tick(&mut self, elapsed: Duration, window_active: bool) {
        let mut changed = false;
        for item in &mut self.items {
            if !item.balloon || item.hovered || !window_active {
                continue;
            }
            let Some(remaining) = item.remaining else { continue };
            if remaining <= elapsed {
                item.balloon = false;
                item.remaining = None;
                changed = true;
            } else {
                item.remaining = Some(remaining - elapsed);
            }
        }
        if changed || !window_active {
            self.ticking = self.should_tick();
        }
    }

    pub fn set_hovered(&mut self, id: u64, hovered: bool) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            if item.balloon {
                item.hovered = hovered;
            }
        }
    }

    /// Hides a balloon; a logged entry stays in the history.
    pub fn dismiss_balloon(&mut self, id: u64) {
        self.items.retain_mut(|item| {
            if item.id != id || !item.balloon {
                return true;
            }
            item.balloon = false;
            item.remaining = None;
            item.logged
        });
    }

    /// The balloons still on screen, oldest first (the stack renders newest at
    /// the bottom).
    pub fn balloons(&self) -> Vec<&NotificationItem> {
        let mut balloons: Vec<&NotificationItem> = self.items.iter().filter(|item| item.balloon).collect();
        balloons.sort_by_key(|item| item.id);
        balloons
    }

    /// Whether a balloon clock still has work to do (visible balloons that can
    /// fade). The workbench starts a tick task when this first becomes true and
    /// stops it when it goes false.
    pub fn should_tick(&self) -> bool {
        self.items.iter().any(|item| item.balloon && item.remaining.is_some())
    }

    pub fn mark_ticking(&mut self, ticking: bool) {
        self.ticking = ticking;
    }

    /// Whether any balloon's clock is currently running (a reason to keep the
    /// workbench's clock task alive).
    pub fn ticking(&self) -> bool {
        self.ticking
    }

    pub fn remove(&mut self, id: u64) {
        self.items.retain(|item| item.id != id);
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.ticking = false;
    }

    /// The list was seen: everything is read and no longer a balloon.
    pub fn mark_all_read(&mut self) {
        for item in &mut self.items {
            item.read = true;
            item.balloon = false;
            item.remaining = None;
        }
        self.ticking = false;
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

    /// The balloon stack (oldest first, i.e. newest at the bottom) as floating
    /// cards for the window's bottom-right corner. Empty when balloons are off
    /// or none are up.
    pub fn render_balloons(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if !self.prefs.balloons || self.prefs.display == BalloonDisplay::None {
            return Vec::new();
        }
        let hovering = theme.list_hover;
        self.balloons()
            .into_iter()
            .map(|item| {
                let (icon, color) = level_icon(item.level, theme);
                let id = item.id;
                let level_accent = color;
                let dismiss = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                    this.dismiss_balloon(id);
                    cx.notify();
                });
                let hover = cx.listener(move |this: &mut Self, hovered: &bool, _window, cx| {
                    this.set_hovered(id, *hovered);
                    cx.notify();
                });
                div()
                    .id(ElementId::Name(format!("balloon-{id}").into()))
                    .test_support()
                    .w(px(340.))
                    .max_h(px(180.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .bg(theme.background)
                    .border_1()
                    .border_color(level_accent.opacity(0.5))
                    .rounded(px(6.))
                    .shadow_lg()
                    .occlude()
                    .on_hover(hover)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Icon::new(icon).small().text_color(level_accent))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child(item.title.clone()),
                            )
                            .child(
                                div()
                                    .id(ElementId::Name(format!("balloon-close-{id}").into()))
                                    .test_support()
                                    .h(px(16.))
                                    .w(px(16.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(3.))
                                    .cursor_pointer()
                                    .hover(move |this| this.bg(hovering))
                                    .on_click(dismiss)
                                    .child(Icon::new(IconName::X).xsmall().text_color(theme.fg_muted())),
                            ),
                    )
                    .when_some(item.body.clone(), |this, body| {
                        this.child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.fg_secondary())
                                .child(body),
                        )
                    })
                    .into_any_element()
            })
            .collect()
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
    let (icon, color) = level_icon(item.level, theme);
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

/// Icon and colour for a notification level (history rows and balloons).
fn level_icon(level: NotifyLevel, theme: &Theme) -> (IconName, Hsla) {
    match level {
        NotifyLevel::Error => (IconName::CircleAlert, theme.danger),
        NotifyLevel::Warning => (IconName::TriangleAlert, theme.warning),
        NotifyLevel::Info => (IconName::Info, theme.info),
    }
}
