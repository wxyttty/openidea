//! Bottom tool dock: the workbench puts every tool window into a `DockArea`
//! (tabs, drag between groups, persisted layout) while `bottom` keeps selecting
//! which one is on screen.
#![recursion_limit = "512"]

use gpui_kit::base::dock::DockPlacement;
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{
    AnyWindowHandle, AppContext, Entity, IntoElement, ParentElement, Render, Styled, TestAppContext,
    Window, WindowOptions, div,
};
use ide_ui::actions;
use ide_ui::theme::{self, ThemeChoice};
use ide_ui::workbench::{Workbench, WorkbenchConfig};
use std::sync::{Arc, OnceLock};

struct Fixture {
    handle: AnyWindowHandle,
    workbench: Entity<Workbench>,
    _runtime: tokio::runtime::Runtime,
}

fn open(cx: &mut TestAppContext) -> Fixture {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.into_path();
    let config_dir = root.join("config");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
    let stream = ide_ui::events::stream();
    let sink_bridge = stream.bridge.clone();
    let sink_for: ide_host::SinkFactory = Box::new(move |_w| {
        Arc::new(sink_bridge.clone()) as ide_core::SharedSink
    });
    let host = {
        let _guard = runtime.enter();
        let languages =
            Arc::new(ide_language::LanguageRegistry::load(Some(&config_dir.join("languages"))).unwrap());
        Arc::new(ide_host::Host::new(ide_host::HostConfig {
            config_dir: config_dir.clone(),
            data_dir: data_dir.clone(),
            languages,
            syntax: Arc::new(OnceLock::new()),
            sink_for,
            owner: |w| w,
            runtime: runtime.handle().clone(),
            project_environment: false,
        }))
    };
    let receiver = stream.receiver;
    cx.update(|cx| {
        gpui_kit::init(cx);
        theme::install_choice(cx, ThemeChoice::IslandsDark);
        actions::install(cx);
    });
    let (handle, workbench) = cx.update(|cx| {
        let host = host.clone();
        let (c, d) = (config_dir.clone(), data_dir.clone());
        gpui_kit::open_window(
            WindowOptions { show: false, ..Default::default() },
            cx,
            |window, cx| {
                cx.new(|cx| {
                    Workbench::new(
                        host,
                        WorkbenchConfig {
                            config_dir: c,
                            data_dir: d,
                            initial_project: None,
                            initial_file: None,
                            theme: ThemeChoice::IslandsDark,
                        },
                        receiver,
                        window,
                        cx,
                    )
                })
            },
        )
        .expect("open")
    });
    Fixture { handle: handle.into(), workbench, _runtime: runtime }
}

#[gpui_kit::test]
fn bottom_dock_holds_every_tool_window(cx: &mut TestAppContext) {
    let fixture = open(cx);
    cx.update(|cx| {
        let dock = fixture
            .workbench
            .read(cx)
            .dock()
            .expect("the workbench owns a bottom dock")
            .read(cx);
        assert!(dock.has_dock(DockPlacement::Bottom));
        let tree = dock.layout(DockPlacement::Bottom).expect("bottom layout");
        // One tab group with all ten tool windows, as built in `Workbench::new`.
        let tabs = tree.panels().count();
        assert_eq!(
            tabs, 13,
            "problems, terminal, diff, run, debug, db, http, notebook, agents, duplicates, structural, structure, notifications"
        );
    });

    // Opening a tool window selects its panel in the dock.
    cx.update_window(fixture.handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("alt-6", cx); // Toggle Problems
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let workbench = fixture.workbench.read(cx);
        assert!(workbench.bottom_is_problems());
        let dock = workbench.dock().unwrap().read(cx);
        assert!(dock.has_dock(DockPlacement::Bottom));
    });
}
