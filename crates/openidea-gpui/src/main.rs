//! OpenIDEA's native desktop entry point on GPUI.
//!
//! This is the replacement for `src-tauri/src/main.rs` + `lib.rs`: instead of
//! booting a Tauri runtime and a webview, it initialises the same backend
//! crates (`ide-host`, `ide-tools`, `ide-sdk`, …) and opens a GPUI window with
//! the workbench from `ide-ui`. No IPC, no webview, no TypeScript bridge.

mod single_instance;

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

    let (Ok(config_dir), Ok(data_dir), Ok(local_data_dir)) = (
        ide_host::data_dirs::config_dir(IDENTIFIER),
        ide_host::data_dirs::data_dir(IDENTIFIER),
        ide_host::data_dirs::startup_local_data_dir(IDENTIFIER),
    ) else {
        eprintln!("cannot resolve the OpenIDEA directories");
        std::process::exit(1);
    };
    if let Err(err) =
        std::fs::create_dir_all(&config_dir).and_then(|_| std::fs::create_dir_all(&data_dir)).and_then(|_| {
            std::fs::create_dir_all(&local_data_dir)
        })
    {
        eprintln!("cannot create {config_dir:?} / {data_dir:?} / {local_data_dir:?}: {err}");
        std::process::exit(1);
    }
    // One instance per user: a second launch hands its arguments (a project or
    // file) to the running window and exits, like the Tauri single-instance
    // plugin did.
    let mut instance_lock = match single_instance::acquire(&data_dir.join("openidea.sock")) {
        Ok(lock) => lock,
        Err(single_instance::Claim::Handed(arguments)) => {
            tracing::info!("another instance received {arguments:?}");
            return;
        }
        Err(single_instance::Claim::Owned(lock)) => lock,
    };

    // Tools, JDKs and language servers live in the local (machine) data directory.
    ide_tools::extend_process_path(&local_data_dir);
    ide_dap::adapter::set_tools_root(ide_tools::tools_dir(&local_data_dir));
    ide_sdk::java::add_jdk_root(&ide_tools::jdk::jdks_dir(&local_data_dir));
    let refresh_shell_path = ide_host::shell_env::inherit_login_shell_path(&IDENTIFIER);
    ide_ssh::git_creds::init(data_dir.join("git-credentials.json"));
    let removed = ide_workspace::caches::apply_pending(&data_dir, &local_data_dir);
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
    let sink_bridge = bridge.clone();
    let sink_for: SinkFactory = Box::new(move |_window| Arc::new(sink_bridge.clone()) as ide_core::SharedSink);
    let host = Arc::new(Host::new(HostConfig {
        config_dir: config_dir.clone(),
        data_dir: data_dir.clone(),
        local_data_dir: local_data_dir.clone(),
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
    let launches = std::sync::Arc::new(std::sync::Mutex::new(instance_lock.take_incoming()));
    let bridge_for_windows = bridge.clone();
    let config = WorkbenchConfig {
        config_dir,
        data_dir,
        initial_project,
        initial_file,
        theme,
        launches: launches.lock().ok().and_then(|mut slot| slot.take()),
        bridge: bridge_for_windows,
    };
    let receiver = stream.receiver;

    // The lock must outlive the window: dropping it removes the socket.
    let _instance_lock = instance_lock;
    let application = gpui_kit::application().with_assets(gpui_kit::assets::AllAssets);
    application.run(move |cx| {
        gpui_kit::init(cx);
        // Highlights queries for the grammars gpui-component ships without one.
        ide_ui::highlights::install();
        theme::install_choice(cx, config.theme);
        ide_ui::actions::install(cx);

        // Restore the window's last geometry (`ui.json` › window), as the Tauri
        // shell did; a fresh install centres at the default size.
        let state = ide_ui::layout::WindowState::load(&config.config_dir);
        let bounds = match (state.x.is_finite(), state.y.is_finite()) {
            (true, true) => Bounds {
                origin: point(px(state.x), px(state.y)),
                size: size(px(state.width), px(state.height)),
            },
            _ => Bounds::centered(None, size(px(state.width), px(state.height)), cx),
        };
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: true,
            show: true,
            ..Default::default()
        };

        let result = gpui_kit::open_window(options, cx, {
            let host = host.clone();
            let bridge_for_windows = bridge.clone();
    let config = WorkbenchConfig {
                config_dir: config.config_dir.clone(),
                data_dir: config.data_dir.clone(),
                initial_project: config.initial_project.clone(),
                initial_file: config.initial_file.clone(),
                theme: config.theme,
                // The forwarded-argument receiver is moved into the workbench
                // (a `Receiver` cannot be cloned).
                launches: launches.lock().ok().and_then(|mut slot| slot.take()),
                bridge: config.bridge.clone(),
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
