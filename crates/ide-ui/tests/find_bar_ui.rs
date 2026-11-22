//! Find in File bar: `Ctrl+F` opens it and the `Aa` / `.*` chips toggle the
//! matcher options (regex and case sensitivity).
#![recursion_limit = "512"]

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled,
    TestAppContext, Window, WindowOptions, div,
};
use ide_ui::actions;
use ide_ui::editor::FindOptions;
use ide_ui::theme::{self, ThemeChoice};
use ide_ui::workbench::{Workbench, WorkbenchConfig};
use std::sync::{Arc, OnceLock};

fn open_workbench(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Workbench>) {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.into_path();
    let config_dir = root.join("config");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
    let stream = ide_ui::events::stream();
    let bridge = stream.bridge.clone();
    let sink_bridge = stream.bridge.clone();
    let sink_for: ide_host::SinkFactory =
        Box::new(move |_window| Arc::new(sink_bridge.clone()) as ide_core::SharedSink);
    let host = {
        let _guard = runtime.enter();
        let languages =
            Arc::new(ide_language::LanguageRegistry::load(Some(&config_dir.join("languages"))).expect("langs"));
        Arc::new(ide_host::Host::new(ide_host::HostConfig {
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
        actions::install(cx);
    });
    let result = cx.update(|cx| {
        let host = host.clone();
        let config_dir = config_dir.clone();
        let data_dir = data_dir.clone();
        gpui_kit::open_window(
            WindowOptions { show: false, ..Default::default() },
            cx,
            |window, cx| {
                cx.new(|cx| {
                    Workbench::new(
                        host.clone(),
                        WorkbenchConfig {
                            config_dir,
                            data_dir,
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
        .expect("open workbench window")
    });
    let _ = &runtime;
    (result.0.into(), result.1)
}

#[gpui_kit::test]
fn ctrl_f_opens_the_bar_and_chips_toggle_options(cx: &mut TestAppContext) {
    let (handle, workbench) = open_workbench(cx);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("secondary-f", cx);
        window.render_frame(cx);
        assert!(window.find("find-case").visible(), "Aa chip is shown");
        assert!(window.find("find-regex").visible(), "regex chip is shown");

        // Defaults: case-insensitive plain text.
        assert_eq!(workbench.read(cx).find_options(), FindOptions { case_sensitive: false, regex: false });

        window.click("find-case", cx);
        assert!(workbench.read(cx).find_options().case_sensitive);
        window.click("find-regex", cx);
        assert!(workbench.read(cx).find_options().regex);
        // Toggling back returns to the defaults.
        window.click("find-case", cx);
        window.click("find-regex", cx);
        assert_eq!(workbench.read(cx).find_options(), FindOptions::default());
        assert!(workbench.read(cx).find_is_open());
    })
    .unwrap();
}
