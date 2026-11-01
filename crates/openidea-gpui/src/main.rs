//! OpenIDEA's native desktop entry point on GPUI.
//!
//! This is the replacement for `src-tauri/src/main.rs` + `lib.rs`: instead of
//! booting a Tauri runtime and a webview, it initialises the same backend
//! crates (`ide-host`, `ide-tools`, `ide-sdk`, …) and opens a GPUI window with
//! the workbench from `ide-ui`. No IPC, no webview, no TypeScript bridge.

use gpui_kit::*;
use ide_host::{Host, HostConfig, SinkFactory};
use ide_language::{LanguageRegistry, Syntax};
use ide_ui::{Workbench, WorkbenchConfig, events, theme};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// The application identifier, shared with the Tauri bundle so settings,
/// caches, local history and installed tools stay where they were.
const IDENTIFIER: &str = "ai.l0ng.openidea";

fn main() {
    ide_workspace::process::mark_start();
    logging();

    let config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join(IDENTIFIER);
    let data_dir = dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join(IDENTIFIER);
    if let Err(err) = std::fs::create_dir_all(&config_dir).and_then(|_| std::fs::create_dir_all(&data_dir)) {
        eprintln!("cannot create {config_dir:?} / {data_dir:?}: {err}");
        std::process::exit(1);
    }

    // Tools, JDKs and language servers keep the layout the Tauri build used.
    ide_tools::extend_process_path(&data_dir);
    ide_dap::adapter::set_tools_root(ide_tools::tools_dir(&data_dir));
    ide_sdk::java::add_jdk_root(&ide_tools::jdk::jdks_dir(&data_dir));
    let refresh_shell_path = ide_host::shell_env::inherit_login_shell_path(IDENTIFIER);
    ide_ssh::git_creds::init(data_dir.join("git-credentials.json"));
    let removed = ide_workspace::caches::apply_pending(&data_dir);
    if !removed.is_empty() {
        tracing::info!("invalidated caches: {removed:?}");
    }
    if let Some(refresh) = refresh_shell_path {
        refresh();
    }

    // The backend expects a tokio runtime; GPUI runs on its own executor, so the
    // runtime lives beside it for LSP, DAP, terminals and search.
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("openidea-tokio").build() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("cannot start the tokio runtime: {err}");
            std::process::exit(1);
        }
    };
    let _enter = runtime.enter();
    let runtime_handle = runtime.handle().clone();

    // Grammars are needed by the first editor frame; build them off-thread like
    // the Tauri state did so the window opens immediately.
    let syntax: Arc<OnceLock<Arc<Syntax>>> = Arc::default();
    {
        let syntax = syntax.clone();
        std::thread::Builder::new()
            .name("openidea-syntax-init".into())
            .spawn(move || {
                syntax.get_or_init(|| Arc::new(Syntax::new()));
            })
            .expect("spawn syntax init");
    }

    let languages = match LanguageRegistry::load(Some(&config_dir.join("languages"))) {
        Ok(languages) => Arc::new(languages),
        Err(err) => {
            eprintln!("cannot load language definitions: {err}");
            std::process::exit(1);
        }
    };

    let stream = events::stream();
    let bridge = stream.bridge.clone();
    let sink_for: SinkFactory = Box::new(move |_window| Arc::new(bridge.clone()) as ide_core::SharedSink);
    let host = Arc::new(Host::new(HostConfig {
        config_dir: config_dir.clone(),
        data_dir: data_dir.clone(),
        languages,
        syntax,
        sink_for,
        owner: |window| window,
        runtime: runtime_handle,
        project_environment: false,
    }));

    let (initial_project, initial_file) = match std::env::args_os().nth(1).map(PathBuf::from) {
        Some(path) if path.is_file() => (path.parent().map(|dir| dir.to_path_buf()), Some(path)),
        Some(path) if path.is_dir() => (Some(path), None),
        _ => (None, None),
    };

    let theme = ide_ui::theme::load_choice(&config_dir).unwrap_or(ide_ui::theme::ThemeChoice::IslandsDark);
    let config = WorkbenchConfig { config_dir, data_dir, initial_project, initial_file, theme };
    let receiver = stream.receiver;

    let application = gpui_kit::application().with_assets(gpui_kit::assets::AllAssets);
    application.run(move |cx| {
        gpui_kit::init(cx);
        theme::install_choice(cx, config.theme);
        ide_ui::actions::install(cx);

        let bounds = Bounds::centered(None, size(px(1360.), px(880.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: true,
            show: true,
            ..Default::default()
        };
        let result = gpui_kit::open_window(options, cx, {
            let host = host.clone();
            let config = WorkbenchConfig {
                config_dir: config.config_dir.clone(),
                data_dir: config.data_dir.clone(),
                initial_project: config.initial_project.clone(),
                initial_file: config.initial_file.clone(),
                theme: config.theme,
            };
            move |window, cx| cx.new(|cx| Workbench::new(host, config, receiver, window, cx))
        });
        match result {
            Ok((_window, _view)) => {
                cx.activate(true);
            }
            Err(err) => {
                eprintln!("cannot open the OpenIDEA window: {err}");
                cx.quit();
            }
        }
    });
}

fn logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ide_ui=debug,openidea_gpui=debug,gpui=warn"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
