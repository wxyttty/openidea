//! UI integration tests for the Notifications tool window
//! (`crates/ide-ui/src/notifications.rs`) and its workbench wiring
//! (`IdeEvent::Notify → Workbench::on_event → settings filter → read state`).
//!
//! Imports are explicit (no `use gpui_kit::*`): the glob would also import
//! GPUI's `test` macro and shadow Rust's ordinary `#[test]`.

use gpui_kit::test::TestWindowExt;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyWindowHandle, AppContext, Context, Entity, IntoElement, ParentElement, Point, Render,
    ScrollDelta, Styled, TestAppContext, Window, WindowOptions, div, point, px,
};
use ide_core::event::NotifyLevel;
use ide_core::{EventSink, IdeEvent};
use ide_host::{Host, HostConfig, SinkFactory};
use ide_language::{LanguageRegistry, Syntax};
use ide_ui::events::{self, EventBridge};
use ide_ui::notifications::{BalloonDisplay, MAX_BALLOONS, NotificationLog, NotificationPrefs};
use ide_ui::workbench::{Workbench, WorkbenchConfig};
use ide_ui::theme::{self, ThemeChoice};
use std::path::PathBuf;
use std::time::Duration;
use std::sync::{Arc, OnceLock};

/// Window root that renders the log, as the workbench does in its bottom tab.
struct NotificationHost {
    log: Entity<NotificationLog>,
}

impl Render for NotificationHost {
    fn render(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let log = self.log.clone();
        div().size_full().child(log)
    }
}

fn open_log(
    cx: &mut TestAppContext,
    setup: impl FnOnce(&mut NotificationLog),
) -> (AnyWindowHandle, Entity<NotificationLog>) {
    cx.update(|cx| {
        let log = cx.new(|cx| {
            let mut log = NotificationLog::new();
            setup(&mut log);
            log
        });
        let log_for_view = log.clone();
        let (handle, _view) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds {
                    origin: gpui_kit::Point::default(),
                    size: gpui_kit::size(px(480.), px(320.)),
                })),
                show: false,
                ..Default::default()
            },
            cx,
            |_, cx| cx.new(|_| NotificationHost { log: log_for_view }),
        )
        .expect("open test window");
        (handle.into(), log)
    })
}

#[gpui_kit::test]
fn renders_entries_close_and_clear_all(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (handle, log) = open_log(cx, |log| {
        log.push(NotifyLevel::Error, "Formatter missing", Some("rustfmt not found"));
        log.push(NotifyLevel::Info, "Welcome", None::<String>);
    });

    cx.update(|cx| {
        assert_eq!(log.read(cx).len(), 2);
        assert_eq!(log.read(cx).unread(), 2);
    });

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        // Both entries painted; the newest is on top.
        assert!(window.find("notification-2").visible());
        assert!(window.find("notification-1").visible());

        // Close the first entry through its close button.
        window.click("notification-close-1", cx);
        assert!(window.try_find("notification-1").is_none());
        assert!(window.find("notification-2").visible());

        // Clear All empties the list.
        window.click("notifications-clear", cx);
        assert!(window.try_find("notification-2").is_none());
    })
    .unwrap();

    cx.update(|cx| {
        assert_eq!(log.read(cx).len(), 0);
        assert_eq!(log.read(cx).unread(), 0);
    });
}

#[gpui_kit::test]
fn long_lists_scroll_inside_the_panel(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (handle, _log) = open_log(cx, |log| {
        for index in 1..=30 {
            log.push(
                NotifyLevel::Warning,
                format!("entry {index}"),
                Some(format!("line one of {index}\nline two of {index}\nline three of {index}")),
            );
        }
    });

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        // The scroll container stays inside the window (no overflow past the panel).
        let list = window.find("notifications-list");
        let list_bounds = list.bounds();
        let window_size = window.bounds().size;
        assert!(list_bounds.bottom() <= window_size.height + px(1.));
        assert!(list_bounds.right() <= window_size.width + px(1.));

        // Rows do not overlap and grow with their bodies.
        let first = window.find("notification-30").bounds();
        let second = window.find("notification-29").bounds();
        assert!(second.top() >= first.bottom() - px(0.5));
        assert!(first.size.height > px(30.));

        // The oldest entry is below the fold; scrolling brings it in and it
        // can be closed.
        assert!(!window.find("notification-1").visible());
        for _ in 0..25 {
            window.scroll(
                "notifications-list",
                ScrollDelta::Pixels(point(px(0.), px(-150.))),
                cx,
            );
        }
        assert!(window.find("notification-1").visible());
        window.click("notification-close-1", cx);
        assert!(window.try_find("notification-1").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn caps_history_at_200_and_marks_read(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let log = cx.update(|cx| {
        cx.new(|cx| {
            let mut log = NotificationLog::new();
            for index in 0..205 {
                log.push(NotifyLevel::Info, format!("note {index}"), None::<String>);
            }
            log
        })
    });

    cx.update(|cx| {
        assert_eq!(log.read(cx).len(), 200);
        assert_eq!(log.read(cx).unread(), 200);
        // Newest first: the newest is 204, the oldest kept is 5.
        assert_eq!(log.read(cx).items()[0].title.as_ref(), "note 204");
        assert_eq!(log.read(cx).items()[199].title.as_ref(), "note 5");
        log.update(cx, |log, cx| {
            log.mark_all_read();
            cx.notify();
        });
        assert_eq!(log.read(cx).unread(), 0);
    });
}

// --- Workbench entry tests: IdeEvent::Notify → on_event → log state ---

struct WorkbenchFixture {
    handle: AnyWindowHandle,
    workbench: Entity<Workbench>,
    bridge: EventBridge,
    _runtime: tokio::runtime::Runtime,
}

fn open_workbench(cx: &mut TestAppContext, settings_toml: Option<&str>) -> WorkbenchFixture {
    let temp = tempfile::tempdir().expect("temp dir");
    // `into_path` hands over the directory without deleting it on drop; the
    // workbench reads settings lazily, so the dirs must outlive the fixture.
    let root = temp.into_path();
    let config_dir = root.join("config");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();
    if let Some(settings) = settings_toml {
        std::fs::write(config_dir.join("settings.toml"), settings).unwrap();
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("wb-test-tokio")
        .build()
        .expect("tokio runtime");

    let stream = events::stream();
    let bridge = stream.bridge.clone();
    let sink_bridge = stream.bridge.clone();
    let sink_for: SinkFactory =
        Box::new(move |_window| Arc::new(sink_bridge.clone()) as ide_core::SharedSink);
    let host = {
        let _guard = runtime.enter();
        let languages =
            Arc::new(LanguageRegistry::load(Some(&config_dir.join("languages"))).expect("languages"));
        Arc::new(Host::new(HostConfig {
            config_dir: config_dir.clone(),
            data_dir: data_dir.clone(),
            languages,
            syntax: Arc::new(OnceLock::new()),
            sink_for,
            owner: |window| window,
            runtime: runtime.handle().clone(),
            project_environment: false,
        }))
    };

    let receiver = stream.receiver;

    cx.update(|cx| {
        gpui_kit::init(cx);
        theme::install_choice(cx, ThemeChoice::IslandsDark);
    });
    let (handle, workbench) = cx.update(|cx| {
        let host = host.clone();
        let (handle, view) = gpui_kit::open_window(
            WindowOptions { show: false, ..Default::default() },
            cx,
            |window, cx| {
                cx.new(|cx| {
                    Workbench::new(
                        host.clone(),
                        WorkbenchConfig {
                            config_dir: config_dir.clone(),
                            data_dir: data_dir.clone(),
                            initial_project: None,
                            initial_file: None,
                            theme: ThemeChoice::IslandsDark,
                            launches: None,
                        },
                        receiver,
                        window,
                        cx,
                    )
                })
            },
        )
        .expect("open workbench window");
        (handle.into(), view)
    });
    WorkbenchFixture { handle, workbench, bridge, _runtime: runtime }
}

fn notify(fixture: &WorkbenchFixture, cx: &mut TestAppContext, level: NotifyLevel, title: &str) {
    fixture.bridge.emit(IdeEvent::Notify { level, title: title.into(), body: None });
    cx.run_until_parked();
}

#[gpui_kit::test]
fn notify_events_flow_through_the_workbench(cx: &mut TestAppContext) {
    let fixture = open_workbench(cx, None);

    // Hidden panel: the notification lands in the history as unread and does
    // not open the tool window.
    notify(&fixture, cx, NotifyLevel::Warning, "formatter missing");
    cx.update(|cx| {
        let log = fixture.workbench.read(cx).notifications();
        assert_eq!(log.read(cx).len(), 1);
        assert_eq!(log.read(cx).unread(), 1);
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("bottom-tab-Notifications").is_none());
    })
    .unwrap();

    // The status bar shows the unread badge; clicking it opens the panel and
    // marks the history read. Without a project the panel docks under the
    // welcome screen (no bottom tab bar), so the entry itself is the check.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("status-notifications", cx);
        assert!(window.find("notification-1").visible());
    })
    .unwrap();
    cx.update(|cx| {
        let log = fixture.workbench.read(cx).notifications();
        assert_eq!(log.read(cx).unread(), 0);
    });

    // While the panel is visible, arriving notifications are read on arrival.
    notify(&fixture, cx, NotifyLevel::Error, "second event");
    cx.update(|cx| {
        let log = fixture.workbench.read(cx).notifications();
        assert_eq!(log.read(cx).len(), 2);
        assert_eq!(log.read(cx).unread(), 0);
    });
}

#[gpui_kit::test]
fn settings_log_false_keeps_notifications_out_of_history(cx: &mut TestAppContext) {
    // The global layer (`<config>/settings.toml`); project overrides come from
    // the same merged `Settings` value (see ide-workspace's settings tests).
    let fixture = open_workbench(
        cx,
        Some("[notifications.groups.system]\nlog = false\n"),
    );
    notify(&fixture, cx, NotifyLevel::Warning, "suppressed by settings");
    cx.update(|cx| {
        let log = fixture.workbench.read(cx).notifications();
        // Not logged: nothing in the history list. With balloons still on the
        // entry exists only as a balloon.
        assert_eq!(log.read(cx).items().iter().filter(|item| item.logged).count(), 0);
        assert_eq!(log.read(cx).prefs().log, false);
    });
}

// --- Balloon semantics: fade-out clock, hover pause, window activation ---

fn timed_log(cx: &mut TestAppContext, prefs: NotificationPrefs) -> Entity<NotificationLog> {
    cx.update(|cx| {
        cx.new(|cx| {
            let mut log = NotificationLog::new();
            log.set_prefs(prefs);
            log
        })
    })
}

#[gpui_kit::test]
fn info_balloons_fade_after_ten_seconds(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let log = timed_log(cx, NotificationPrefs::default());
    cx.update(|cx| {
        log.update(cx, |log, cx| {
            log.push(NotifyLevel::Info, "plain info", None::<String>);
            cx.notify();
        });
        assert_eq!(log.read(cx).balloons().len(), 1);
        assert_eq!(log.read(cx).len(), 1);
    });

    // 9 s: still up.
    cx.update(|cx| {
        log.update(cx, |log, cx| {
            log.tick(Duration::from_secs(9), true);
            cx.notify();
        })
    });
    cx.update(|cx| assert_eq!(log.read(cx).balloons().len(), 1));

    // 2 s more: faded, but kept in the history (it is logged).
    cx.update(|cx| {
        log.update(cx, |log, cx| {
            log.tick(Duration::from_secs(2), true);
            cx.notify();
        })
    });
    cx.update(|cx| {
        assert_eq!(log.read(cx).balloons().len(), 0);
        assert_eq!(log.read(cx).len(), 1);
    });
}

#[gpui_kit::test]
fn error_balloons_keep_a_longer_clock_and_stay_in_history(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let log = timed_log(cx, NotificationPrefs::default());
    cx.update(|cx| {
        log.update(cx, |log, cx| {
            log.push(NotifyLevel::Error, "error", None::<String>);
            cx.notify();
        });
        // Sticky: an info balloon would be gone after 10 s; an error keeps its
        // longer (5 min) clock.
        log.update(cx, |log, cx| {
            log.tick(Duration::from_secs(120), true);
            cx.notify();
        });
        assert_eq!(log.read(cx).balloons().len(), 1);
        // Dismissed by hand; the entry stays in the history.
        log.update(cx, |log, cx| {
            log.dismiss_balloon(log.items()[0].id);
            cx.notify();
        });
        assert_eq!(log.read(cx).balloons().len(), 0);
        assert_eq!(log.read(cx).len(), 1);
        // Past its sticky timeout the balloon fades; the history keeps it.
        log.update(cx, |log, cx| {
            log.push(NotifyLevel::Error, "second error", None::<String>);
            log.tick(Duration::from_secs(3600), true);
            cx.notify();
        });
        assert_eq!(log.read(cx).balloons().len(), 0);
        assert_eq!(log.read(cx).len(), 2);
    });
}

#[gpui_kit::test]
fn hover_and_inactive_window_pause_the_clock(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let log = timed_log(cx, NotificationPrefs::default());
    let id = cx.update(|cx| {
        log.update(cx, |log, cx| {
            let id = log.push(NotifyLevel::Warning, "warning", None::<String>);
            cx.notify();
            id
        })
    });

    // Hovered: 20 s of clock change nothing.
    cx.update(|cx| {
        log.update(cx, |log, cx| {
            log.set_hovered(id, true);
            log.tick(Duration::from_secs(20), true);
            cx.notify();
        });
        assert_eq!(log.read(cx).balloons().len(), 1);
    });

    // Unhovered but the window is inactive: still paused.
    cx.update(|cx| {
        log.update(cx, |log, cx| {
            log.set_hovered(id, false);
            log.tick(Duration::from_secs(20), false);
            cx.notify();
        });
        assert_eq!(log.read(cx).balloons().len(), 1);
    });

    // Active and not hovered: fades.
    cx.update(|cx| {
        log.update(cx, |log, cx| {
            log.tick(Duration::from_secs(11), true);
            cx.notify();
        })
    });
    cx.update(|cx| assert_eq!(log.read(cx).balloons().len(), 0));
}

#[gpui_kit::test]
fn only_the_newest_three_balloons_stack(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let log = timed_log(cx, NotificationPrefs::default());
    cx.update(|cx| {
        log.update(cx, |log, cx| {
            for index in 0..5 {
                log.push(NotifyLevel::Info, format!("n{index}"), None::<String>);
            }
            cx.notify();
        });
        assert_eq!(log.read(cx).balloons().len(), MAX_BALLOONS);
        // All five stay in the history.
        assert_eq!(log.read(cx).len(), 5);
    });
}

#[gpui_kit::test]
fn display_none_keeps_balloons_away_but_logs(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let log = timed_log(cx, NotificationPrefs { balloons: true, display: BalloonDisplay::None, log: true });
    cx.update(|cx| {
        log.update(cx, |log, cx| {
            log.push(NotifyLevel::Info, "no popup", None::<String>);
            cx.notify();
        });
        assert_eq!(log.read(cx).balloons().len(), 0);
        assert_eq!(log.read(cx).len(), 1);
    });

    // `log: false` + `display: none` drops the event entirely.
    let log = timed_log(cx, NotificationPrefs { balloons: true, display: BalloonDisplay::None, log: false });
    cx.update(|cx| {
        log.update(cx, |log, cx| {
            log.push(NotifyLevel::Info, "dropped", None::<String>);
            cx.notify();
        });
        assert_eq!(log.read(cx).len(), 0);
    });
}

/// Window root that renders the balloon stack, as the workbench overlay does.
struct BalloonHost {
    log: Entity<NotificationLog>,
}

impl Render for BalloonHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let balloons = self.log.update(cx, |log, cx| log.render_balloons(&theme, cx));
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_end()
            .items_end()
            .gap_2()
            .children(balloons)
    }
}

#[gpui_kit::test]
fn balloons_render_hover_and_close(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (handle, log) = cx.update(|cx| {
        let log = cx.new(|cx| {
            let mut log = NotificationLog::new();
            log.push(NotifyLevel::Warning, "first balloon", Some("with a body"));
            log.push(NotifyLevel::Info, "second balloon", None::<String>);
            log
        });
        let host = log.clone();
        let (handle, _view) = gpui_kit::open_window(
            WindowOptions { show: false, ..Default::default() },
            cx,
            |_, cx| cx.new(|_| BalloonHost { log: host }),
        )
        .expect("open balloon window");
        (handle.into(), log)
    });

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        // Both balloons painted, stacked (newest at the bottom).
        let first = window.find("balloon-1");
        let second = window.find("balloon-2");
        assert!(first.visible() && second.visible());
        assert!(second.bounds().top() >= first.bounds().bottom() - px(0.5));

        // Hovering pauses that balloon's clock (through the element's
        // `on_hover`, not a direct state poke).
        window.hover("balloon-1", cx);
    })
    .unwrap();

    cx.update(|cx| {
        let hovered: Vec<(u64, bool)> =
            log.read(cx).items().iter().map(|item| (item.id, item.hovered)).collect();
        assert_eq!(hovered, vec![(2, false), (1, true)], "hover reached the log");
    });

    cx.update(|cx| {
        log.update(cx, |log, cx| {
            log.tick(Duration::from_secs(30), true);
            cx.notify();
        });
        // The hovered one stays; the other faded.
        let up: Vec<u64> = log.read(cx).balloons().iter().map(|item| item.id).collect();
        assert_eq!(up, vec![1]);
    });

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("balloon-2").is_none());
        // Closing a balloon hides it; the history keeps both entries.
        window.click("balloon-close-1", cx);
        assert!(window.try_find("balloon-1").is_none());
    })
    .unwrap();

    cx.update(|cx| {
        assert_eq!(log.read(cx).balloons().len(), 0);
        assert_eq!(log.read(cx).len(), 2);
    });
}
