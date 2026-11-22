//! Notebook tool window: the guards the kernel execution path relies on, plus
//! the interpreter selection the header shows.
//!
//! A live run needs a Python environment with `ipykernel`, which this machine
//! does not have, and loading a notebook goes through a project whose index and
//! watcher threads make the GPUI test scheduler report nondeterminism. These
//! tests therefore cover what is reachable without either: the panel refuses to
//! run before any kernel request is sent, and the interpreter choice survives
//! blank input. The backend's admission, streaming, interrupt and drain logic is
//! covered by `ide-host`'s own tests.
#![recursion_limit = "512"]

use gpui_kit::{AppContext, Entity, TestAppContext};
use ide_core::{EventSink, SharedSink};
use ide_host::{Host, HostConfig, SinkFactory};
use ide_language::{LanguageRegistry, Syntax};
use ide_ui::events;
use ide_ui::notebook::NotebookPanel;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// A host and panel with no project open: the state the tool window starts in.
fn open_panel(cx: &mut TestAppContext) -> (Entity<NotebookPanel>, PathBuf) {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.into_path();
    let config_dir = root.join("config");
    let data_dir = root.join("data");
    let local_data_dir = root.join("local-data");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&local_data_dir).unwrap();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("nb-test-tokio")
        .build()
        .expect("tokio runtime");
    let stream = events::stream();
    let bridge = stream.bridge.clone();
    let sink_for: SinkFactory = Box::new(move |_window| Arc::new(bridge.clone()) as SharedSink);
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
    let panel = cx.update(|cx| cx.new(|cx| NotebookPanel::new(host, cx)));
    (panel, root)
}

#[gpui_kit::test]
fn run_all_without_an_open_notebook_is_refused(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (panel, _root) = open_panel(cx);
    cx.update(|cx| {
        let panel = panel.clone();
        panel.update(cx, |panel, cx| panel.run_all(cx));
        assert_eq!(
            panel.read(cx).notice().map(|notice| notice.to_string()),
            Some("Open a notebook first".to_owned()),
            "the panel refuses before contacting the kernel"
        );
        assert!(panel.read(cx).executing().is_empty(), "no batch was admitted");
        assert!(!panel.read(cx).is_open());
    });
}

#[gpui_kit::test]
fn executing_a_cell_before_opening_a_notebook_is_refused(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (panel, _root) = open_panel(cx);
    cx.update(|cx| {
        let panel = panel.clone();
        panel.update(cx, |panel, cx| panel.execute_cells(vec!["first".to_owned()], cx));
        assert_eq!(
            panel.read(cx).notice().map(|notice| notice.to_string()),
            Some("Open a notebook first".to_owned())
        );
        assert!(panel.read(cx).executing().is_empty());
    });
}

#[gpui_kit::test]
fn opening_a_path_without_a_project_reports_it(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (panel, root) = open_panel(cx);
    cx.update(|cx| {
        let panel = panel.clone();
        let path = root.join("session.ipynb");
        panel.update(cx, |panel, cx| panel.open_path(path, cx));
        assert_eq!(
            panel.read(cx).notice().map(|notice| notice.to_string()),
            Some("Open a project first".to_owned()),
            "no kernel work starts without a project"
        );
    });
}

#[gpui_kit::test]
fn naming_an_interpreter_survives_and_empties_back_to_detection(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (panel, _root) = open_panel(cx);
    cx.update(|cx| {
        let panel = panel.clone();
        panel.update(cx, |panel, cx| panel.set_python(Some("/usr/bin/python3".to_owned()), cx));
        assert_eq!(panel.read(cx).python().map(|path| path.to_string()), Some("/usr/bin/python3".to_owned()));
        panel.update(cx, |panel, cx| panel.set_python(Some("   ".to_owned()), cx));
        assert!(panel.read(cx).python().is_none(), "blank input goes back to SDK detection");
        panel.update(cx, |panel, cx| panel.set_python(None, cx));
        assert!(panel.read(cx).python().is_none());
    });
}

#[gpui_kit::test]
fn interrupting_without_a_running_batch_is_a_no_op(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (panel, _root) = open_panel(cx);
    cx.update(|cx| {
        let panel = panel.clone();
        // No request is active, so this must not panic or claim to interrupt.
        panel.update(cx, |panel, cx| panel.interrupt(cx));
        assert!(panel.read(cx).notice().is_none());
    });
}
