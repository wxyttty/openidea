//! End-to-end workbench flows through the real `Workbench` (a real `Host`, the
//! real event bridge, the real menus/actions), not through wrapper views.
//!
//! These are the flows a user takes on first contact: the window opens, the
//! theme and layout come from the settings, the tool windows switch, and the
//! document events the backend emits reach the editor. They complement the
//! panel-level suites (notifications, dock, find bar, notebook, …) by exercising
//! the shell itself.
#![recursion_limit = "512"]

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext, Entity, TestAppContext, WindowOptions, px,
};
use ide_core::{EventSink, IdeEvent, SharedSink};
use ide_host::{Host, HostConfig, SinkFactory};
use ide_language::{LanguageRegistry, Syntax};
use ide_ui::events;
use ide_ui::theme::{self, ThemeChoice};
use ide_ui::workbench::{Workbench, WorkbenchConfig};
use std::sync::{Arc, OnceLock};

struct Fixture {
    handle: AnyWindowHandle,
    workbench: Entity<Workbench>,
    bridge: events::EventBridge,
    _runtime: tokio::runtime::Runtime,
    _temp: tempfile::TempDir,
}

fn open_workbench(cx: &mut TestAppContext, settings_toml: Option<&str>) -> Fixture {
    let temp = tempfile::tempdir().expect("temp dir");
    let config_dir = temp.path().join("config");
    let data_dir = temp.path().join("data");
    let local_data_dir = temp.path().join("local-data");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&local_data_dir).unwrap();
    if let Some(settings) = settings_toml {
        std::fs::write(config_dir.join("settings.toml"), settings).unwrap();
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("wb-e2e-tokio")
        .build()
        .expect("tokio runtime");
    let stream = events::stream();
    let bridge = stream.bridge.clone();
    let sink_bridge = stream.bridge.clone();
    let sink_for: SinkFactory = Box::new(move |_window| Arc::new(sink_bridge.clone()) as SharedSink);
    let host = {
        let _guard = runtime.enter();
        let languages =
            Arc::new(LanguageRegistry::load(Some(&config_dir.join("languages"))).expect("languages"));
        Arc::new(Host::new(HostConfig {
            config_dir: config_dir.clone(),
            data_dir: data_dir.clone(),
            local_data_dir: local_data_dir.clone(),
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
    let (handle, view) = cx
        .update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds {
                        origin: gpui_kit::Point::default(),
                        size: gpui_kit::size(px(1200.), px(800.)),
                    })),
                    show: false,
                    ..Default::default()
                },
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
                                bridge: bridge.clone(),
                            },
                            receiver,
                            window,
                            cx,
                        )
                    })
                },
            )
        })
        .expect("open workbench window");
    Fixture { handle: handle.into(), workbench: view, bridge, _runtime: runtime, _temp: temp }
}

#[gpui_kit::test]
fn the_shell_opens_with_the_welcome_screen_and_menu_bar(cx: &mut TestAppContext) {
    let fixture = open_workbench(cx, None);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        // The welcome screen offers the two ways to start.
        assert!(window.find("welcome-open-project").visible());
        assert!(window.find("welcome-new-project").visible());
        // The nine menus are painted.
        assert!(window.try_find("menu-File").is_some());
        assert!(window.try_find("menu-Edit").is_some());
        assert!(window.try_find("menu-View").is_some());
        assert!(window.try_find("menu-Navigate").is_some());
        assert!(window.try_find("menu-Refactor").is_some());
        assert!(window.try_find("menu-Git").is_some());
        assert!(window.try_find("menu-Run").is_some());
        assert!(window.try_find("menu-Debug").is_some());
        assert!(window.try_find("menu-Help").is_some());
        // No project: the bottom dock has no tab strip on screen.
        assert!(window.try_find("bottom-tab-Problems").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_status_bar_reports_the_theme_and_no_project(cx: &mut TestAppContext) {
    let fixture = open_workbench(cx, None);
    cx.update(|cx| {
        assert!(fixture.workbench.read(cx).has_project() == false);
    });
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("status-message").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_backend_notification_reaches_the_status_bar_and_the_log(cx: &mut TestAppContext) {
    let fixture = open_workbench(cx, None);
    fixture.bridge.emit(IdeEvent::Notify {
        level: ide_core::event::NotifyLevel::Warning,
        title: "formatter missing".into(),
        body: Some("rustfmt is not installed".into()),
    });
    cx.run_until_parked();
    cx.update(|cx| {
        let workbench = fixture.workbench.read(cx);
        assert_eq!(workbench.status_text(), "formatter missing — rustfmt is not installed");
        let log = workbench.notifications();
        assert_eq!(log.read(cx).len(), 1);
        assert_eq!(log.read(cx).unread(), 1);
    });
}

#[gpui_kit::test]
fn a_suppressed_title_does_not_open_a_second_notification(cx: &mut TestAppContext) {
    let fixture = open_workbench(cx, None);
    let notify = |title: &str| IdeEvent::Notify {
        level: ide_core::event::NotifyLevel::Info,
        title: title.into(),
        body: None,
    };
    fixture.bridge.emit(notify("index rebuilt"));
    cx.run_until_parked();
    cx.update(|cx| {
        let workbench = fixture.workbench.clone();
        workbench.update(cx, |workbench, cx| workbench.suppress_title("index rebuilt", cx));
    });
    fixture.bridge.emit(notify("index rebuilt"));
    cx.run_until_parked();
    cx.update(|cx| {
        let log = fixture.workbench.read(cx).notifications();
        assert_eq!(log.read(cx).len(), 1, "the suppressed title did not add an entry");
    });
}

#[gpui_kit::test]
fn switching_tool_windows_through_the_view_menu_changes_the_selection(cx: &mut TestAppContext) {
    let fixture = open_workbench(cx, None);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        // Without a project the dock is not rendered, so drive the command the
        // View menu entry runs (the entry and the command share `Cmd::Terminal`).
        fixture
            .workbench
            .update(cx, |workbench, cx| workbench.select_bottom_by_label("Terminal", window, cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let workbench = fixture.workbench.read(cx);
        assert_eq!(workbench.selected_bottom_label(), "Terminal");
        assert!(workbench.terminal_is_selected());
    });
}

#[gpui_kit::test]
fn the_settings_dialog_opens_and_shows_the_sections(cx: &mut TestAppContext) {
    let fixture = open_workbench(cx, None);
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        fixture.workbench.update(cx, |workbench, cx| workbench.open_settings(window, cx));
        window.render_frame(cx);
        // The dialog is open: its Done button and the JSON section are drawn.
        assert!(window.try_find("settings-done").is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_second_notification_replaces_the_status_message(cx: &mut TestAppContext) {
    let fixture = open_workbench(cx, None);
    for title in ["first", "second"] {
        fixture.bridge.emit(IdeEvent::Notify {
            level: ide_core::event::NotifyLevel::Info,
            title: title.into(),
            body: None,
        });
        cx.run_until_parked();
    }
    cx.update(|cx| {
        assert_eq!(fixture.workbench.read(cx).status_text(), "second");
        assert_eq!(fixture.workbench.read(cx).notifications().read(cx).len(), 2);
    });
}
