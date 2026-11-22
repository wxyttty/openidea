//! The workbench: the window root that replaces the Tauri window and the React
//! `Workbench` component tree.
//!
//! Layout mirrors IntelliJ's Islands UI: a menu bar, a left tool-window stripe,
//! the project tree / editor split, a bottom tool window and a status bar. The
//! menu bar, command palette, tool-window stripes and shortcuts are native GPUI
//! elements; opening a project goes straight to `ide-host::Host`, with no IPC.

use crate::actions::{self, *};
use crate::agents::AgentPanel;
use crate::breakpoints::Breakpoints;
use crate::db::DbPanel;
use crate::debug::{DebugAction, DebugPanel};
use crate::deploy::DeployPanel;
use crate::diff::DiffPanel;
use crate::duplicates::DuplicatesPanel;
use crate::dock::ToolPanel;
use crate::editor::{EditorArea, FindOptions, TabInfo};
use crate::http::HttpPanel;
use crate::intentions::IntentionSection;
use crate::rename::{RenameDialog, RenameDone, dialog_origin};
use crate::layout::Layout;
use crate::notebook::NotebookPanel;
use crate::notifications::{BalloonDisplay, NotificationLog, NotificationPrefs};
use crate::problems::{ProblemsPanel, SharedDiagnostics, shared_diagnostics};
use crate::project_tree::ProjectTree;
use crate::run::RunPanel;
use crate::search::SearchPanel;
use crate::structural::StructuralPanel;
use crate::ssh::SshPanel;
use crate::structure::StructurePanel;
use crate::terminal::TerminalPanel;
use crate::theme;
use crate::theme::IdeColors as _;
use crate::vcs::VcsPanel;
use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dock::{DockArea, DockLayout, DockPlacement, DockSkin};
use gpui_kit::component::input::{Input, InputContent, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_core::IdeEvent;
use ide_host::Host;
use ide_plugin::{Limits, PluginHost, PluginInfo, PluginStatus};
use ide_workspace::{RecentProjects, SettingsScope};
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

/// Everything the workbench needs before the first frame.
pub struct WorkbenchConfig {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub initial_project: Option<PathBuf>,
    /// A file given on the command line; opened after its project.
    pub initial_file: Option<PathBuf>,
    pub theme: theme::ThemeChoice,
    /// Paths a later launch forwarded (single instance); the workbench opens
    /// them in this window as they arrive.
    pub launches: Option<std::sync::mpsc::Receiver<Vec<String>>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuKind {
    File,
    Edit,
    View,
    Navigate,
    Refactor,
    Git,
    Run,
    Debug,
    Help,
}

const MENU_KINDS: [MenuKind; 9] = [
    MenuKind::File,
    MenuKind::Edit,
    MenuKind::View,
    MenuKind::Navigate,
    MenuKind::Refactor,
    MenuKind::Git,
    MenuKind::Run,
    MenuKind::Debug,
    MenuKind::Help,
];

impl MenuKind {
    fn label(self) -> &'static str {
        match self {
            MenuKind::File => "File",
            MenuKind::Edit => "Edit",
            MenuKind::View => "View",
            MenuKind::Navigate => "Navigate",
            MenuKind::Refactor => "Refactor",
            MenuKind::Git => "Git",
            MenuKind::Run => "Run",
            MenuKind::Debug => "Debug",
            MenuKind::Help => "Help",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cmd {
    OpenProject,
    RecentProjects,
    GoToLine,
    FindInEditor,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    FloatBottom,
    DockBottom,
    Plugins,
    Save,
    SaveAll,
    CloseTab,
    NextTab,
    PrevTab,
    QuickOpen,
    FindInPath,
    ShowProject,
    ShowSearch,
    ShowVcs,
    ToggleProject,
    ToggleProblems,
    ToggleTerminal,
    CommandPalette,
    RefreshProject,
    RefreshVcs,
    ToggleTheme,
    ToggleBreakpoint,
    ViewBreakpoints,
    Settings,
    Run,
    StopRun,
    ShowRun,
    EditRunConfigurations,
    Debug,
    ShowDebug,
    ShowDb,
    ShowHttp,
    ShowNotebook,
    RunHttpAtCaret,
    StopDebug,
    ResumeDebug,
    PauseDebug,
    StepInto,
    StepOver,
    StepOut,
    Appearance,
    TrustProject,
    About,
    ShowAgents,
    ShowNotifications,
    Intentions,
    Rename,
    ShowDuplicates,
    ShowStructural,
    ShowStructure,
    ShowSsh,
    ShowDeploy,
    Quit,
}

struct MenuEntry {
    label: &'static str,
    shortcut: Option<&'static str>,
    cmd: Cmd,
}

const FILE_MENU: &[MenuEntry] = &[
    MenuEntry { label: "Open Project…", shortcut: Some("Ctrl+O"), cmd: Cmd::OpenProject },
    MenuEntry { label: "Open Recent Project…", shortcut: None, cmd: Cmd::RecentProjects },
    MenuEntry { label: "Settings…", shortcut: Some("Ctrl+Alt+S"), cmd: Cmd::Settings },
    MenuEntry { label: "Refresh Project", shortcut: None, cmd: Cmd::RefreshProject },
    MenuEntry { label: "Save", shortcut: Some("Ctrl+S"), cmd: Cmd::Save },
    MenuEntry { label: "Save All", shortcut: Some("Ctrl+Shift+S"), cmd: Cmd::SaveAll },
    MenuEntry { label: "Trust Project and Enable Language Servers", shortcut: None, cmd: Cmd::TrustProject },
    MenuEntry { label: "Close Tab", shortcut: Some("Ctrl+W"), cmd: Cmd::CloseTab },
    MenuEntry { label: "Quit", shortcut: Some("Ctrl+Q"), cmd: Cmd::Quit },
];

const VIEW_MENU: &[MenuEntry] = &[
    MenuEntry { label: "Project", shortcut: Some("Alt+1"), cmd: Cmd::ShowProject },
    MenuEntry { label: "Find in Path", shortcut: Some("Ctrl+Shift+F"), cmd: Cmd::FindInPath },
    MenuEntry { label: "Git", shortcut: None, cmd: Cmd::ShowVcs },
    MenuEntry { label: "Problems", shortcut: Some("Alt+6"), cmd: Cmd::ToggleProblems },
    MenuEntry { label: "Terminal", shortcut: Some("Alt+2"), cmd: Cmd::ToggleTerminal },
    MenuEntry { label: "Appearance…", shortcut: None, cmd: Cmd::Appearance },
    MenuEntry { label: "Toggle Theme", shortcut: None, cmd: Cmd::ToggleTheme },
    MenuEntry { label: "Database", shortcut: Some("Alt+3"), cmd: Cmd::ShowDb },
    MenuEntry { label: "HTTP Client", shortcut: Some("Alt+7"), cmd: Cmd::ShowHttp },
    MenuEntry { label: "Notebook", shortcut: Some("Alt+8"), cmd: Cmd::ShowNotebook },
    MenuEntry { label: "Agents", shortcut: Some("Alt+9"), cmd: Cmd::ShowAgents },
    MenuEntry { label: "Notifications", shortcut: None, cmd: Cmd::ShowNotifications },
    MenuEntry { label: "Plugins…", shortcut: None, cmd: Cmd::Plugins },
    MenuEntry { label: "Float Tool Window", shortcut: None, cmd: Cmd::FloatBottom },
    MenuEntry { label: "Dock Tool Window", shortcut: None, cmd: Cmd::DockBottom },
    MenuEntry { label: "Zoom In", shortcut: None, cmd: Cmd::ZoomIn },
    MenuEntry { label: "Zoom Out", shortcut: None, cmd: Cmd::ZoomOut },
    MenuEntry { label: "Reset Zoom", shortcut: None, cmd: Cmd::ZoomReset },
];

const NAVIGATE_MENU: &[MenuEntry] = &[
    MenuEntry { label: "Go to File…", shortcut: Some("Ctrl+P"), cmd: Cmd::QuickOpen },
    MenuEntry { label: "Go to Line…", shortcut: Some("Ctrl+G"), cmd: Cmd::GoToLine },
    MenuEntry { label: "Find in File…", shortcut: Some("Ctrl+F"), cmd: Cmd::FindInEditor },
    MenuEntry { label: "Plugins…", shortcut: None, cmd: Cmd::Plugins },
    MenuEntry { label: "Float Tool Window", shortcut: None, cmd: Cmd::FloatBottom },
    MenuEntry { label: "Dock Tool Window", shortcut: None, cmd: Cmd::DockBottom },
    MenuEntry { label: "Find in Path…", shortcut: Some("Ctrl+Shift+F"), cmd: Cmd::FindInPath },
    MenuEntry { label: "Command Palette…", shortcut: Some("Ctrl+Shift+P"), cmd: Cmd::CommandPalette },
    MenuEntry { label: "Next Tab", shortcut: Some("Ctrl+Tab"), cmd: Cmd::NextTab },
    MenuEntry { label: "Previous Tab", shortcut: Some("Ctrl+Shift+Tab"), cmd: Cmd::PrevTab },
];

const EDIT_MENU: &[MenuEntry] = &[
    MenuEntry { label: "Undo", shortcut: Some("Ctrl+Z"), cmd: Cmd::Undo },
    MenuEntry { label: "Redo", shortcut: Some("Ctrl+Shift+Z"), cmd: Cmd::Redo },
    MenuEntry { label: "Cut", shortcut: Some("Ctrl+X"), cmd: Cmd::Cut },
    MenuEntry { label: "Copy", shortcut: Some("Ctrl+C"), cmd: Cmd::Copy },
    MenuEntry { label: "Paste", shortcut: Some("Ctrl+V"), cmd: Cmd::Paste },
    MenuEntry { label: "Select All", shortcut: Some("Ctrl+A"), cmd: Cmd::SelectAll },
    MenuEntry { label: "Find in File", shortcut: Some("Ctrl+F"), cmd: Cmd::FindInEditor },
    MenuEntry { label: "Find in Path", shortcut: Some("Ctrl+Shift+F"), cmd: Cmd::FindInPath },
];

const REFACTOR_MENU: &[MenuEntry] = &[
    MenuEntry { label: "Rename Symbol", shortcut: Some("Shift+F6"), cmd: Cmd::Rename },
    MenuEntry { label: "Show Context Actions", shortcut: Some("Alt+Enter"), cmd: Cmd::Intentions },
    MenuEntry { label: "Locate Duplicates…", shortcut: None, cmd: Cmd::ShowDuplicates },
    MenuEntry { label: "Search Structurally…", shortcut: None, cmd: Cmd::ShowStructural },
    MenuEntry { label: "Structure…", shortcut: None, cmd: Cmd::ShowStructure },
];

const GIT_MENU: &[MenuEntry] = &[
    MenuEntry { label: "Show Git Tool Window", shortcut: None, cmd: Cmd::ShowVcs },
    MenuEntry { label: "Refresh Status", shortcut: None, cmd: Cmd::RefreshVcs },
];

const RUN_MENU: &[MenuEntry] = &[
    MenuEntry { label: "Run", shortcut: Some("F5"), cmd: Cmd::Run },
    MenuEntry { label: "Stop", shortcut: Some("Shift+F5"), cmd: Cmd::StopRun },
    MenuEntry { label: "Show Run Tool Window", shortcut: Some("Alt+4"), cmd: Cmd::ShowRun },
    MenuEntry { label: "Edit Configurations…", shortcut: None, cmd: Cmd::EditRunConfigurations },
    MenuEntry { label: "Show Database Tool Window", shortcut: Some("Alt+3"), cmd: Cmd::ShowDb },
    MenuEntry { label: "Show HTTP Client", shortcut: Some("Alt+7"), cmd: Cmd::ShowHttp },
    MenuEntry { label: "Run HTTP Request at Caret", shortcut: Some("Ctrl+Alt+R"), cmd: Cmd::RunHttpAtCaret },
];

const DEBUG_MENU: &[MenuEntry] = &[
    MenuEntry { label: "Start Debugging", shortcut: Some("Shift+F9"), cmd: Cmd::Debug },
    MenuEntry { label: "Resume", shortcut: Some("F9"), cmd: Cmd::ResumeDebug },
    MenuEntry { label: "Pause", shortcut: None, cmd: Cmd::PauseDebug },
    MenuEntry { label: "Step Over", shortcut: Some("F8"), cmd: Cmd::StepOver },
    MenuEntry { label: "Step Into", shortcut: Some("F7"), cmd: Cmd::StepInto },
    MenuEntry { label: "Step Out", shortcut: Some("Shift+F8"), cmd: Cmd::StepOut },
    MenuEntry { label: "Stop", shortcut: Some("Ctrl+F2"), cmd: Cmd::StopDebug },
    MenuEntry { label: "Toggle Breakpoint", shortcut: Some("Ctrl+F8"), cmd: Cmd::ToggleBreakpoint },
    MenuEntry { label: "View Breakpoints…", shortcut: Some("Ctrl+Shift+F8"), cmd: Cmd::ViewBreakpoints },
    MenuEntry { label: "Show Debug Tool Window", shortcut: Some("Alt+5"), cmd: Cmd::ShowDebug },
    MenuEntry { label: "Show Database Tool Window", shortcut: Some("Alt+3"), cmd: Cmd::ShowDb },
    MenuEntry { label: "Show HTTP Client", shortcut: Some("Alt+7"), cmd: Cmd::ShowHttp },
    MenuEntry { label: "Run HTTP Request at Caret", shortcut: Some("Ctrl+Alt+R"), cmd: Cmd::RunHttpAtCaret },
];

const HELP_MENU: &[MenuEntry] = &[MenuEntry { label: "About OpenIDEA", shortcut: None, cmd: Cmd::About }];

fn parse_goto(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split(':');
    let line: u32 = parts.next()?.trim().parse().ok()?;
    if line == 0 {
        return None;
    }
    let col = match parts.next() {
        Some(col) => col.trim().parse::<u32>().ok()?.max(1),
        None => 1,
    };
    Some((line - 1, col - 1))
}

/// Default floating tool-window geometry inside a window of `width` × `height`.
fn float_bounds(width: f32, height: f32) -> FloatPanel {
    let w = 720_f32.min((width - 40.).max(200.));
    let h = 480_f32.min((height - 80.).max(160.));
    FloatPanel { x: ((width - w) / 2.).max(10.), y: 80., w, h, edge: None }
}

/// How close to a window edge a dragged card must be to snap there.
const SNAP_DISTANCE: f32 = 28.;

/// The edge a dragged card at (`x`, `y`) snaps to, if any. Bottom wins over the
/// sides near the bottom corners, so the wide dock is reachable there.
fn snap_edge(x: f32, y: f32, width: f32, height: f32) -> Option<DockEdge> {
    if y >= height - SNAP_DISTANCE {
        Some(DockEdge::Bottom)
    } else if x <= SNAP_DISTANCE {
        Some(DockEdge::Left)
    } else if x + SNAP_DISTANCE >= width {
        Some(DockEdge::Right)
    } else {
        None
    }
}

/// Geometry of a card docked to `edge` in a window of `width` × `height`.
/// A docked card spans its edge at about a third of the window, leaving the
/// editor usable.
fn docked_bounds(edge: DockEdge, width: f32, height: f32) -> FloatPanel {
    const MIN: f32 = 240.;
    const MAX: f32 = 560.;
    match edge {
        DockEdge::Left => {
            let w = (width * 0.32).clamp(MIN, MAX).min(width - 80.);
            FloatPanel { x: 0., y: 0., w, h: height, edge: Some(edge) }
        }
        DockEdge::Right => {
            let w = (width * 0.32).clamp(MIN, MAX).min(width - 80.);
            FloatPanel { x: width - w, y: 0., w, h: height, edge: Some(edge) }
        }
        DockEdge::Bottom => {
            // A short window keeps a usable strip instead of clamping to an
            // empty range (min > max panics in `f32::clamp`).
            let max = (height - 120.).max(80.);
            let h = (height * 0.34).clamp(80_f32.min(max), max);
            FloatPanel { x: 0., y: height - h, w: width, h, edge: Some(edge) }
        }
    }
}

fn menu_entries(kind: MenuKind) -> &'static [MenuEntry] {
    match kind {
        MenuKind::File => FILE_MENU,
        MenuKind::View => VIEW_MENU,
        MenuKind::Navigate => NAVIGATE_MENU,
        MenuKind::Edit => EDIT_MENU,
        MenuKind::Refactor => REFACTOR_MENU,
        MenuKind::Git => GIT_MENU,
        MenuKind::Run => RUN_MENU,
        MenuKind::Debug => DEBUG_MENU,
        MenuKind::Help => HELP_MENU,
    }
}

/// Everything the command palette can run, in the order it lists them.
const PALETTE: &[MenuEntry] = &[
    MenuEntry { label: "Go to File…", shortcut: Some("Ctrl+P"), cmd: Cmd::QuickOpen },
    MenuEntry { label: "Open Project…", shortcut: Some("Ctrl+O"), cmd: Cmd::OpenProject },
    MenuEntry { label: "Open Recent Project…", shortcut: None, cmd: Cmd::RecentProjects },
    MenuEntry { label: "Settings…", shortcut: Some("Ctrl+Alt+S"), cmd: Cmd::Settings },
    MenuEntry { label: "Go to Line…", shortcut: Some("Ctrl+G"), cmd: Cmd::GoToLine },
    MenuEntry { label: "Find in File…", shortcut: Some("Ctrl+F"), cmd: Cmd::FindInEditor },
    MenuEntry { label: "Plugins…", shortcut: None, cmd: Cmd::Plugins },
    MenuEntry { label: "Float Tool Window", shortcut: None, cmd: Cmd::FloatBottom },
    MenuEntry { label: "Dock Tool Window", shortcut: None, cmd: Cmd::DockBottom },
    MenuEntry { label: "Find in Path…", shortcut: Some("Ctrl+Shift+F"), cmd: Cmd::FindInPath },
    MenuEntry { label: "Command Palette…", shortcut: Some("Ctrl+Shift+P"), cmd: Cmd::CommandPalette },
    MenuEntry { label: "Save File", shortcut: Some("Ctrl+S"), cmd: Cmd::Save },
    MenuEntry { label: "Save All", shortcut: Some("Ctrl+Shift+S"), cmd: Cmd::SaveAll },
    MenuEntry { label: "Trust Project and Enable Language Servers", shortcut: None, cmd: Cmd::TrustProject },
    MenuEntry { label: "Close Tab", shortcut: Some("Ctrl+W"), cmd: Cmd::CloseTab },
    MenuEntry { label: "Next Tab", shortcut: Some("Ctrl+Tab"), cmd: Cmd::NextTab },
    MenuEntry { label: "Previous Tab", shortcut: Some("Ctrl+Shift+Tab"), cmd: Cmd::PrevTab },
    MenuEntry { label: "Show Project", shortcut: Some("Alt+1"), cmd: Cmd::ShowProject },
    MenuEntry { label: "Show Git Tool Window", shortcut: None, cmd: Cmd::ShowVcs },
    MenuEntry { label: "Show Notebook", shortcut: Some("Alt+8"), cmd: Cmd::ShowNotebook },
    MenuEntry { label: "Show Agents", shortcut: Some("Alt+9"), cmd: Cmd::ShowAgents },
    MenuEntry { label: "Show Notifications", shortcut: None, cmd: Cmd::ShowNotifications },
    MenuEntry { label: "Show Context Actions", shortcut: Some("Alt+Enter"), cmd: Cmd::Intentions },
    MenuEntry { label: "Rename Symbol", shortcut: Some("Shift+F6"), cmd: Cmd::Rename },
    MenuEntry { label: "Locate Duplicates…", shortcut: None, cmd: Cmd::ShowDuplicates },
    MenuEntry { label: "Search Structurally…", shortcut: None, cmd: Cmd::ShowStructural },
    MenuEntry { label: "Structure…", shortcut: None, cmd: Cmd::ShowStructure },
    MenuEntry { label: "SSH Connections…", shortcut: None, cmd: Cmd::ShowSsh },
    MenuEntry { label: "Deployment…", shortcut: None, cmd: Cmd::ShowDeploy },
    MenuEntry { label: "Toggle Problems", shortcut: Some("Alt+6"), cmd: Cmd::ToggleProblems },
    MenuEntry { label: "Toggle Terminal", shortcut: Some("Alt+2"), cmd: Cmd::ToggleTerminal },
    MenuEntry { label: "Refresh Project Tree", shortcut: None, cmd: Cmd::RefreshProject },
    MenuEntry { label: "Trust Project", shortcut: None, cmd: Cmd::TrustProject },
    MenuEntry { label: "Appearance…", shortcut: None, cmd: Cmd::Appearance },
    MenuEntry { label: "Toggle Light / Dark Theme", shortcut: None, cmd: Cmd::ToggleTheme },
    MenuEntry { label: "About OpenIDEA", shortcut: None, cmd: Cmd::About },
    MenuEntry { label: "Quit OpenIDEA", shortcut: Some("Ctrl+Q"), cmd: Cmd::Quit },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BottomTab {
    Problems,
    Terminal,
    Diff,
    Run,
    Debug,
    Db,
    Http,
    Notebook,
    Agents,
    Duplicates,
    Structural,
    Structure,
    Ssh,
    Deploy,
    Notifications,
}

impl BottomTab {
    fn label(self) -> &'static str {
        match self {
            BottomTab::Problems => "Problems",
            BottomTab::Terminal => "Terminal",
            BottomTab::Diff => "Diff",
            BottomTab::Run => "Run",
            BottomTab::Debug => "Debug",
            BottomTab::Db => "Database",
            BottomTab::Http => "HTTP Client",
            BottomTab::Notebook => "Notebook",
            BottomTab::Agents => "Agents",
            BottomTab::Duplicates => "Duplicates",
            BottomTab::Structural => "Structural Search",
            BottomTab::Structure => "Structure",
            BottomTab::Ssh => "SSH",
            BottomTab::Deploy => "Deployment",
            BottomTab::Notifications => "Notifications",
        }
    }

    fn index(self) -> usize {
        match self {
            BottomTab::Problems => 0,
            BottomTab::Terminal => 1,
            BottomTab::Diff => 2,
            BottomTab::Run => 3,
            BottomTab::Debug => 4,
            BottomTab::Db => 5,
            BottomTab::Http => 6,
            BottomTab::Notebook => 7,
            BottomTab::Agents => 8,
            BottomTab::Duplicates => 9,
            BottomTab::Structural => 10,
            BottomTab::Structure => 11,
            BottomTab::Ssh => 12,
            BottomTab::Deploy => 13,
            BottomTab::Notifications => 14,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResizeTarget {
    Left,
    Bottom,
}

/// A small on/off chip in the Find bar (`Aa` for case, `.*` for regex).
fn option_toggle(
    id: &'static str,
    label: &'static str,
    title: &'static str,
    active: bool,
    theme: &gpui_kit::component::Theme,
    hover: Hsla,
    click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(ElementId::Name(id.into()))
        .test_support()
        .h(px(20.))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .cursor_pointer()
        .when(active, |this| this.bg(theme.accent.opacity(0.22)))
        .hover(move |this| this.bg(hover))
        .on_click(click)
        .text_size(px(10.))
        .text_color(if active { theme.foreground } else { theme.fg_muted() })
        .aria_label(title)
        .child(label)
        .into_any_element()
}

/// Where a floating tool window is parked when it reaches a window edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DockEdge {
    Left,
    Right,
    Bottom,
}

impl DockEdge {
    fn label(self) -> &'static str {
        match self {
            DockEdge::Left => "left",
            DockEdge::Right => "right",
            DockEdge::Bottom => "bottom",
        }
    }
}

#[derive(Clone, Copy)]
struct FloatPanel {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    /// Set once the card has snapped to a window edge; dragging it away floats
    /// it again.
    edge: Option<DockEdge>,
}

/// The drag of a floating card: where the pointer went down and where the card
/// was, plus the edge the release would snap to.
#[derive(Clone, Copy)]
struct FloatDrag {
    start_x: f32,
    start_y: f32,
    panel_x: f32,
    panel_y: f32,
    /// Preview only: the card keeps floating until the pointer is released.
    preview: Option<DockEdge>,
}

#[derive(Clone, Copy)]
struct ResizeDrag {
    target: ResizeTarget,
    start: Point<Pixels>,
    size: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LeftPanel {
    Project,
    Search,
    Vcs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsSection {
    Editor,
    Terminal,
    Keymap,
    Appearance,
    General,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PaletteMode {
    Commands,
    Files,
}

pub struct Workbench {
    host: Arc<Host>,
    config_dir: PathBuf,
    data_dir: PathBuf,
    project: Option<Arc<ide_host::Project>>,
    editor: Entity<EditorArea>,
    tree: Entity<ProjectTree>,
    problems: Entity<ProblemsPanel>,
    terminal: Entity<TerminalPanel>,
    run: Entity<RunPanel>,
    debug: Entity<DebugPanel>,
    db: Entity<DbPanel>,
    http: Entity<HttpPanel>,
    notebook: Entity<NotebookPanel>,
    agents: Entity<AgentPanel>,
    notifications: Entity<NotificationLog>,
    duplicates: Entity<DuplicatesPanel>,
    structural: Entity<StructuralPanel>,
    structure: Entity<StructurePanel>,
    ssh: Entity<SshPanel>,
    deploy: Entity<DeployPanel>,
    /// The bottom tool area: a dock, so panels move between groups and the
    /// layout persists.
    intentions: Entity<IntentionSection>,
    rename: Entity<RenameDialog>,
    rename_open: bool,
    dock: Option<Entity<DockArea>>,
    dock_skin: Option<std::rc::Rc<DockSkin>>,
    /// Panel id of each `BottomTab` in the dock, in `BottomTab::index` order.
    dock_panels: Vec<(BottomTab, gpui_kit::base::dock::PanelId)>,
    diff: Entity<DiffPanel>,
    search: Entity<SearchPanel>,
    vcs: Entity<VcsPanel>,
    diagnostics: SharedDiagnostics,
    recent: RecentProjects,
    focus_handle: FocusHandle,
    palette_focus: FocusHandle,
    status: SharedString,
    breakpoints: Breakpoints,
    breakpoints_open: bool,
    left: LeftPanel,
    show_project: bool,
    show_bottom: bool,
    bottom: BottomTab,
    palette_open: bool,
    palette_mode: PaletteMode,
    palette_return: Option<FocusHandle>,
    palette_query: String,
    palette_index: usize,
    quick: Vec<(PathBuf, SharedString)>,
    open_menu: Option<MenuKind>,
    menu_pos: Point<Pixels>,
    about_open: bool,
    indexing: Option<SharedString>,
    lsp: Option<SharedString>,
    theme: theme::ThemeChoice,
    appearance_open: bool,
    recent_open: bool,
    goto_open: bool,
    goto_submit: bool,
    find_open: bool,
    find_entered: bool,
    find_highlight_dirty: bool,
    find_index: usize,
    find_searched: bool,
    find_options: FindOptions,
    settings_open: bool,
    /// The last window geometry written to `ui.json`, so a frame that did not
    /// move or resize the window does not touch the disk.
    window_state: crate::layout::WindowState,
    /// The shortcut dialog (Settings › Keymap).
    keymap_dialog: Entity<crate::keymap::KeymapDialog>,
    keymap_accepted: Option<Subscription>,
    settings_section: SettingsSection,
    editor_font_size: f32,
    terminal_font_size: f32,
    layout: Layout,
    terminal_viewport: Rc<Cell<(f32, f32)>>,
    resize_drag: Option<ResizeDrag>,
    float_panel: Option<FloatPanel>,
    float_drag: Option<FloatDrag>,
    plugins: Option<Arc<PluginHost>>,
    plugins_open: bool,
    plugins_list: Vec<PluginInfo>,
    plugins_notice: Option<SharedString>,
    plugin_task: Option<Task<()>>,
    /// The task draining the plugin event stream.
    plugin_event_task: Option<Task<()>>,
    /// The plugin event stream (notifications and editor text insertions).
    plugin_events: futures::channel::mpsc::UnboundedReceiver<ide_plugin::PluginEvent>,
    balloon_clock: Option<Task<()>>,
    condition_input: Entity<InputState>,
    log_input: Entity<InputState>,
    goto_input: Entity<InputState>,
    find_input: Entity<InputState>,
    selected_breakpoint: Option<(PathBuf, u32)>,
    _condition_subscription: Subscription,
    _log_subscription: Subscription,
    _goto_subscription: Subscription,
    _find_subscription: Subscription,
}

impl Workbench {
    pub fn new(
        host: Arc<Host>,
        config: WorkbenchConfig,
        mut events: UnboundedReceiver<IdeEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial_settings = host.global_settings.effective();
        let editor_font_size = initial_settings.editor.font_size;
        let terminal_font_size = initial_settings.terminal.font_size;
        let editor = cx.new(|cx| EditorArea::new(host.clone(), cx));
        let tree = cx.new(|cx| ProjectTree::new(editor.clone(), cx));
        let diagnostics = shared_diagnostics();
        let problems = cx.new(|cx| ProblemsPanel::new(diagnostics.clone(), editor.clone(), cx));
        let search = cx.new(|cx| SearchPanel::new(host.clone(), editor.clone(), window, cx));
        let vcs = cx.new(|cx| VcsPanel::new(editor.clone(), window, cx));
        let condition_input = cx.new(|cx| InputState::new(window, cx).placeholder("Condition, e.g. x > 0"));
        let condition_subscription =
            cx.subscribe(&condition_input, |this: &mut Self, _input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.apply_condition(cx);
                }
            });
        let goto_input = cx.new(|cx| InputState::new(window, cx).placeholder("Line[:column], e.g. 42 or 42:8"));
        let goto_subscription = cx.subscribe(&goto_input, |this: &mut Self, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.goto_submit = true;
                cx.notify();
            }
        });
        let find_input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in file…"));
        let find_subscription = cx.subscribe(&find_input, |this: &mut Self, _input, event: &InputEvent, cx| {
            match event {
                InputEvent::Change => {
                    this.find_searched = false;
                    this.find_index = 0;
                    this.find_highlight_dirty = true;
                }
                InputEvent::PressEnter { .. } => {
                    this.find_entered = true;
                }
                _ => {}
            }
            cx.notify();
        });
        let log_input = cx.new(|cx| InputState::new(window, cx).placeholder("Log message, e.g. x={x}"));
        let log_subscription = cx.subscribe(&log_input, |this: &mut Self, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.apply_log_message(cx);
            }
        });
        let layout = Layout::load(&config.config_dir);
        let terminal_viewport: Rc<Cell<(f32, f32)>> = Rc::new(Cell::new((0., 0.)));
        let terminal = cx.new(|cx| TerminalPanel::new(host.clone(), terminal_viewport.clone(), cx));
        let run = cx.new(|cx| RunPanel::new(host.clone(), cx));
        let debug = cx.new(|cx| DebugPanel::new(host.clone(), window, cx));
        let db = cx.new(|cx| DbPanel::new(host.clone(), window, cx));
        let http = cx.new(|cx| HttpPanel::new(host.clone(), window, cx));
        let notebook = cx.new(|cx| NotebookPanel::new(host.clone(), cx));
        let agents = cx.new(|cx| AgentPanel::new(host.clone(), window, cx));
        let notifications = cx.new(|_| NotificationLog::new());
        let duplicates = cx.new(|cx| DuplicatesPanel::new(host.clone(), editor.clone(), cx));
        let structural = cx.new(|cx| StructuralPanel::new(host.clone(), editor.clone(), window, cx));
        let structure = cx.new(|cx| StructurePanel::new(editor.clone(), host.syntax(), cx));
        let ssh = cx.new(|cx| SshPanel::new(host.clone(), cx));
        let deploy = cx.new(|cx| DeployPanel::new(host.clone(), editor.clone(), cx));
        let rename = cx.new(|cx| RenameDialog::new(window, cx));
        let rename_done = cx.subscribe(&rename, |this: &mut Self, _dialog, _: &RenameDone, cx| {
            this.rename_open = false;
            this.set_status("Rename applied", cx);
        });
        let _rename_done = rename_done;
        let intentions = {
            let editor = editor.clone();
            let host = host.clone();
            cx.new(|cx| IntentionSection::new(window, cx, editor, host))
        };
        let diff = cx.new(|cx| DiffPanel::new(cx));

        // The bottom tool area as a dock: panels become tabs of a bottom dock,
        // drag between groups, and the layout persists through `DockArea`.
        let (dock_entity, dock_skin) = DockSkin::dock_area("workbench-bottom", Some(1), window, cx);
        let problems_panel = cx.new(|cx| ToolPanel::new("problems", "Problems", IconName::CircleAlert, problems.clone(), cx));
        let terminal_panel = cx.new(|cx| ToolPanel::new("terminal", "Terminal", IconName::SquareTerminal, terminal.clone(), cx));
        let diff_panel = cx.new(|cx| ToolPanel::new("diff", "Diff", IconName::GitCompare, diff.clone(), cx));
        let run_panel = cx.new(|cx| ToolPanel::new("run", "Run", IconName::Play, run.clone(), cx));
        let debug_panel = cx.new(|cx| ToolPanel::new("debug", "Debug", IconName::Bug, debug.clone(), cx));
        let db_panel = cx.new(|cx| ToolPanel::new("database", "Database", IconName::Database, db.clone(), cx));
        let http_panel = cx.new(|cx| ToolPanel::new("http", "HTTP Client", IconName::Cloud, http.clone(), cx));
        let notebook_panel = cx.new(|cx| ToolPanel::new("notebook", "Notebook", IconName::BookOpen, notebook.clone(), cx));
        let agents_panel = cx.new(|cx| ToolPanel::new("agents", "Agents", IconName::Bot, agents.clone(), cx));
        let notifications_panel =
            cx.new(|cx| ToolPanel::new("notifications", "Notifications", IconName::Info, notifications.clone(), cx));
        let duplicates_panel =
            cx.new(|cx| ToolPanel::new("duplicates", "Duplicates", IconName::Copy, duplicates.clone(), cx));
        let structural_panel = cx.new(|cx| {
            ToolPanel::new("structural", "Structural Search", IconName::Search, structural.clone(), cx)
        });
        let structure_panel =
            cx.new(|cx| ToolPanel::new("structure", "Structure", crate::structure::ICON, structure.clone(), cx));
        let ssh_panel = cx.new(|cx| ToolPanel::new("ssh", "SSH", IconName::Cloud, ssh.clone(), cx));
        let deploy_panel = cx.new(|cx| ToolPanel::new("deploy", "Deployment", crate::deploy::ICON, deploy.clone(), cx));
        let dock_panels = vec![
            (BottomTab::Problems, problems_panel.entity_id()),
            (BottomTab::Terminal, terminal_panel.entity_id()),
            (BottomTab::Diff, diff_panel.entity_id()),
            (BottomTab::Run, run_panel.entity_id()),
            (BottomTab::Debug, debug_panel.entity_id()),
            (BottomTab::Db, db_panel.entity_id()),
            (BottomTab::Http, http_panel.entity_id()),
            (BottomTab::Notebook, notebook_panel.entity_id()),
            (BottomTab::Agents, agents_panel.entity_id()),
            (BottomTab::Duplicates, duplicates_panel.entity_id()),
            (BottomTab::Structural, structural_panel.entity_id()),
            (BottomTab::Structure, structure_panel.entity_id()),
            (BottomTab::Ssh, ssh_panel.entity_id()),
            (BottomTab::Deploy, deploy_panel.entity_id()),
            (BottomTab::Notifications, notifications_panel.entity_id()),
        ]
        .into_iter()
        .map(|(tab, id)| (tab, gpui_kit::base::dock::PanelId::from(id)))
        .collect::<Vec<_>>();
        let bottom_layout = DockLayout::tabs()
            .panel(problems_panel)
            .panel(terminal_panel)
            .panel(diff_panel)
            .panel(run_panel)
            .panel(debug_panel)
            .panel(db_panel)
            .panel(http_panel)
            .panel(notebook_panel)
            .panel(agents_panel)
            .panel(duplicates_panel)
            .panel(structural_panel)
            .panel(structure_panel)
            .panel(ssh_panel)
            .panel(deploy_panel)
            .panel(notifications_panel);
        dock_entity.update(cx, |dock, cx| {
            dock.set_dock(DockPlacement::Bottom, bottom_layout, window, cx);
        });
        editor.update(cx, |editor, cx| editor.set_font_size(editor_font_size, cx));
        let initial_soft_wrap = initial_settings.editor.soft_wrap;
        editor.update(cx, |editor, cx| editor.set_soft_wrap_default(initial_soft_wrap, window, cx));
        terminal.update(cx, |terminal, cx| terminal.set_font_size(terminal_font_size, cx));
        cx.subscribe(&vcs, |this: &mut Self, _vcs, event: &crate::vcs::OpenDiffRequest, cx| {
            this.show_bottom = true;
            this.bottom = BottomTab::Diff;
            this.diff.update(cx, |diff, cx| diff.show(event.0.clone(), cx));
            cx.notify();
        })
        .detach();
        let recent = RecentProjects::load(&config.config_dir);
        // Plugin events (notifications, `insertText`, `replaceSelection`) are
        // queued here and drained by a task spawned once the workbench exists.
        let (plugin_tx, plugin_rx) = futures::channel::mpsc::unbounded::<ide_plugin::PluginEvent>();
        let plugin_sink: Arc<dyn ide_plugin::PluginEventSink> =
            Arc::new(move |_: Option<&str>, event: ide_plugin::PluginEvent| {
                let _ = plugin_tx.unbounded_send(event);
            });
        let plugins =
            PluginHost::new(config.data_dir.join("plugins"), plugin_sink, Limits::default()).ok().map(Arc::new);
        if let Some(plugin_host) = &plugins {
            let trust = host.clone();
            plugin_host.set_project_policy(Arc::new(move |root| trust.trust.trusted(root).unwrap_or(false)));
            let loader = plugin_host.clone();
            let _ = std::thread::Builder::new().name("plugin-load".into()).spawn(move || loader.load_all());
        }
        let window_state = crate::layout::WindowState::load(&config.config_dir);
        let mut this = Self {
            host,
            config_dir: config.config_dir,
            data_dir: config.data_dir,
            project: None,
            editor,
            tree,
            problems,
            terminal,
            run,
            debug,
            db,
            http,
            notebook,
            agents,
            notifications,
            duplicates,
            structural,
            structure,
            ssh,
            deploy,
            intentions,
            rename,
            rename_open: false,
            dock: Some(dock_entity),
            dock_skin: Some(dock_skin),
            dock_panels,
            diff,
            search,
            vcs,
            diagnostics,
            recent,
            focus_handle: cx.focus_handle(),
            palette_focus: cx.focus_handle(),
            status: "Ready".into(),
            breakpoints: Breakpoints::default(),
            breakpoints_open: false,
            left: LeftPanel::Project,
            show_project: true,
            show_bottom: false,
            bottom: BottomTab::Problems,
            palette_open: false,
            palette_mode: PaletteMode::Commands,
            palette_return: None,
            palette_query: String::new(),
            palette_index: 0,
            quick: Vec::new(),
            open_menu: None,
            menu_pos: point(px(8.), px(30.)),
            about_open: false,
            indexing: None,
            lsp: None,
            theme: config.theme,
            appearance_open: false,
            recent_open: false,
            goto_open: false,
            goto_submit: false,
            find_open: false,
            find_entered: false,
            find_highlight_dirty: false,
            find_index: 0,
            find_searched: false,
            find_options: FindOptions::default(),
            settings_open: false,
            window_state,
            keymap_dialog: cx.new(|cx| crate::keymap::KeymapDialog::new(window, cx)),
            keymap_accepted: None,
            settings_section: SettingsSection::Editor,
            editor_font_size,
            terminal_font_size,
            layout,
            terminal_viewport,
            resize_drag: None,
            float_panel: None,
            float_drag: None,
            plugins,
            plugins_open: false,
            plugins_list: Vec::new(),
            plugins_notice: None,
            plugin_task: None,
            plugin_event_task: None,
            plugin_events: plugin_rx,
            balloon_clock: None,
            condition_input,
            log_input,
            goto_input,
            find_input,
            selected_breakpoint: None,
            _condition_subscription: condition_subscription,
            _log_subscription: log_subscription,
            _goto_subscription: goto_subscription,
            _find_subscription: find_subscription,
        };

        // Backend events arrive on any thread; apply them on the GPUI thread,
        // with the window available for editor patches.
        cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events.next().await {
                if this.update_in(cx, |this, window, cx| this.on_event(event, window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        if let Some(path) = config.initial_project.clone() {
            this.open_project(path, window, cx);
        }
        if let Some(file) = config.initial_file.clone() {
            this.editor.update(cx, |editor, cx| editor.open_file(file, window, cx));
        }
        // A second launch forwards its arguments here; open them in this window.
        if let Some(launches) = config.launches {
            let (tx, mut rx) = futures::channel::mpsc::unbounded::<Vec<String>>();
            std::thread::Builder::new()
                .name("openidea-launch-bridge".into())
                .spawn(move || {
                    while let Ok(paths) = launches.recv() {
                        if tx.unbounded_send(paths).is_err() {
                            break;
                        }
                    }
                })
                .expect("spawn launch bridge");
            cx.spawn_in(window, async move |this, cx| {
                while let Some(arguments) = rx.next().await {
                    if this
                        .update_in(cx, |this, window, cx| {
                            for argument in arguments {
                                this.open_launch(PathBuf::from(argument), window, cx);
                            }
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
        }
        let handle = this.focus_handle.clone();
        window.focus(&handle, cx);
        this
    }

    /// The `system` notification group's effective preferences (project
    /// overrides the global settings); backend and plugin events share it.
    fn notification_prefs(&self) -> NotificationPrefs {
        let settings = self
            .project
            .as_ref()
            .map(|project| project.settings.effective())
            .unwrap_or_else(|| self.host.global_settings.effective());
        let group = settings.notifications.groups.get("system");
        NotificationPrefs {
            balloons: settings.notifications.balloons,
            display: match group.map(|group| group.display.as_str()) {
                Some("none") => BalloonDisplay::None,
                Some("sticky") => BalloonDisplay::Sticky,
                _ => BalloonDisplay::Balloon,
            },
            log: group.map(|group| group.log).unwrap_or(true),
        }
    }

    fn on_event(&mut self, event: IdeEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            IdeEvent::Notify { level, title, body } => {
                self.status = match body.as_deref() {
                    Some(body) if !body.is_empty() => format!("{title} — {body}").into(),
                    _ => title.clone().into(),
                };
                // The Notifications tool window lists backend events under the
                // `system` group, as the Tauri shell's NotificationService did;
                // `log: false` keeps them out of the history. Project settings
                // override the global ones when a project is open.
                let prefs = self.notification_prefs();
                // Balloons are hidden while the Notifications tool window is on
                // screen (IDEA shows either the stack or the list).
                let read = self.notifications_visible();
                self.notifications.update(cx, |log, cx| {
                    log.set_prefs(prefs);
                    log.push(level, title, body);
                    if read {
                        log.mark_all_read();
                    }
                    cx.notify();
                });
                self.ensure_balloon_clock(window, cx);
                cx.notify();
            }
            IdeEvent::ProjectTrustChanged { .. } => {
                self.status = "Project trust changed".into();
                cx.notify();
            }
            IdeEvent::DocPatched { doc, base_version, version, changes } => {
                self.editor
                    .update(cx, |editor, cx| editor.apply_patch(doc, base_version, version, changes, window, cx));
            }
            IdeEvent::DocDirty { doc, dirty } => {
                self.editor.update(cx, |editor, cx| editor.set_dirty(doc, dirty, cx));
            }
            IdeEvent::DocLanguage { doc, language, .. } => {
                self.editor.update(cx, |editor, cx| editor.set_language(doc, &language.0, cx));
            }
            IdeEvent::Diagnostics { path, source, items } => {
                tracing::debug!(path = ?path.0, source, count = items.len(), "diagnostics");
                self.diagnostics.borrow_mut().apply(&path.0, &source, items);
                let all = self.diagnostics.borrow().for_path(&path.0);
                self.editor.update(cx, |editor, cx| editor.set_diagnostics(&path.0, &all, cx));
                self.problems.update(cx, |problems, cx| problems.refresh(cx));
                cx.notify();
            }
            IdeEvent::Debug { session, event } => {
                if matches!(
                    event,
                    ide_core::event::DebugEvent::Started { parent: None, .. }
                        | ide_core::event::DebugEvent::Stopped { .. }
                ) {
                    self.show_bottom = true;
                    self.bottom = BottomTab::Debug;
                }
                self.debug.update(cx, |debug, cx| debug.on_event(session, event, cx));
                cx.notify();
            }
            IdeEvent::RunExited { run, code } => {
                self.run.update(cx, |panel, cx| panel.mark_exited(run, code, cx));
                cx.notify();
            }
            IdeEvent::TerminalExited { terminal, code } => {
                self.terminal.update(cx, |panel, cx| panel.mark_exited(terminal, code, cx));
                cx.notify();
            }
            IdeEvent::LanguageServer { name, status, message, .. } => {
                use ide_core::event::ServerStatus;
                self.lsp = match status {
                    ServerStatus::Starting => Some(format!("{name}: starting…").into()),
                    ServerStatus::Ready => Some(format!("{name}: ready").into()),
                    ServerStatus::Busy => Some(format!("{name}: working…").into()),
                    ServerStatus::Crashed => Some(message.unwrap_or_else(|| format!("{name}: crashed")).into()),
                    ServerStatus::Stopped => Some(format!("{name}: stopped").into()),
                    ServerStatus::Missing => Some(format!("{name}: not installed").into()),
                    ServerStatus::Misconfigured => {
                        Some(message.unwrap_or_else(|| format!("{name}: misconfigured")).into())
                    }
                };
                cx.notify();
            }
            IdeEvent::VcsChanged { .. } => {
                self.vcs.update(cx, |vcs, cx| vcs.refresh(cx));
                self.diff.update(cx, |diff, cx| diff.refresh(cx));
                cx.notify();
            }
            IdeEvent::Task { title, message, done, .. } => {
                if done {
                    self.indexing = None;
                } else {
                    self.indexing = Some(match message {
                        Some(message) => format!("{title}: {message}").into(),
                        None => title.into(),
                    });
                }
                cx.notify();
            }
            IdeEvent::IndexState { indexing, files, symbols } => {
                if indexing {
                    self.indexing = Some(format!("Indexing… {files} files, {symbols} symbols").into());
                } else if files > 0 {
                    self.indexing = Some(format!("Indexed {files} files, {symbols} symbols").into());
                }
                cx.notify();
            }
            other => tracing::debug!(event = ?other, "unhandled backend event"),
        }
    }

    fn set_status(&mut self, status: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.status = status.into();
        cx.notify();
    }

    /// Whether the Notifications tool window is on screen: docked in the bottom
    /// area (with or without a project) or detached into the floating panel.
    fn notifications_visible(&self) -> bool {
        self.bottom == BottomTab::Notifications && (self.float_panel.is_some() || self.show_bottom)
    }

    /// Points the dock at the panel for `tab`, so `self.bottom` stays the one
    /// source of truth for "which tool window is showing".
    fn sync_dock_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dock) = self.dock.clone() else { return };
        let Some((_, panel)) = self.dock_panels.iter().find(|(tab, _)| *tab == self.bottom) else { return };
        let panel = *panel;
        dock.update(cx, |dock, cx| dock.select_panel(panel, window, cx));
    }

    /// `Alt+Enter`: asks the language servers for code actions at the caret.
    fn show_intentions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((doc, line, col, _range)) = self.editor.read(cx).active_position(cx) else {
            self.set_status("Open a file first", cx);
            return;
        };
        let range = ide_core::Range {
            start: ide_core::Pos { line, col },
            end: ide_core::Pos { line, col },
        };
        let focus = self.intentions.read(cx).popup_focus(cx);
        window.focus(&focus, cx);
        let host = self.host.clone();
        let pos = ide_core::Pos { line, col };
        self.intentions.update(cx, |section, cx| section.request(host, doc, pos, range, cx));
        cx.notify();
    }

    /// `Shift+F6`: rename the symbol under the caret.
    fn start_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((doc, line, col, range)) = self.editor.read(cx).active_position(cx) else {
            self.set_status("Open a file first", cx);
            return;
        };
        let text = self.editor.read(cx).active_text().unwrap_or_default();
        let Some(name) = symbol_at(&text, range) else {
            self.set_status("Put the caret on a symbol to rename", cx);
            return;
        };
        self.rename_open = true;
        let host = self.host.clone();
        let pos = ide_core::Pos { line, col };
        self.rename.update(cx, |dialog, cx| dialog.begin(host, doc, pos, name, window, cx));
        cx.notify();
    }

    /// The notification history (the Notifications tool window's state).
    pub fn notifications(&self) -> &Entity<NotificationLog> {
        &self.notifications
    }

    /// Opens what a launch argument names: a directory opens as the project, a
    /// file opens (its project first). A second instance forwards its arguments
    /// here, and the first launch uses the same rules.
    pub fn open_launch(&mut self, argument: std::path::PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if argument.is_dir() {
            self.open_project(argument, window, cx);
            return;
        }
        if let Some(parent) = argument.parent() {
            let needs_project =
                self.project.as_ref().is_none_or(|project| project.root != parent);
            if needs_project {
                self.open_project(parent.to_path_buf(), window, cx);
            }
        }
        self.editor.update(cx, |editor, cx| editor.open_file(argument, window, cx));
        cx.notify();
    }

    /// The bottom tool dock (all tool windows as dock panels).
    pub fn dock(&self) -> Option<&Entity<DockArea>> {
        self.dock.as_ref()
    }

    /// The dock's skin: its settings (panel style, button visibility) change
    /// through this handle.
    pub fn dock_skin(&self) -> Option<&std::rc::Rc<DockSkin>> {
        self.dock_skin.as_ref()
    }

    /// Whether the Problems tool window is the one selected.
    pub fn bottom_is_problems(&self) -> bool {
        self.bottom == BottomTab::Problems && self.show_bottom
    }

    /// The Find in File options the bar's `Aa` / `.*` chips toggle.
    pub fn find_options(&self) -> FindOptions {
        self.find_options
    }

    /// Whether the Find in File bar is open.
    pub fn find_is_open(&self) -> bool {
        self.find_open
    }

    /// Starts the balloon clock when there is something to fade. One foreground
    /// task per window; it stops itself once no balloon can fade.
    fn ensure_balloon_clock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.balloon_clock.is_some() {
            return;
        }
        let should_tick = self.notifications.read(cx).should_tick();
        if !should_tick {
            return;
        }
        self.notifications.update(cx, |log, _| log.mark_ticking(true));
        let task = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(crate::notifications::TICK).await;
                let keep = this.update_in(cx, |this, window, cx| {
                    let active = window.is_window_active();
                    let mut more = false;
                    this.notifications.update(cx, |log, cx| {
                        log.tick(crate::notifications::TICK, active);
                        // The list is visible: balloons stay hidden.
                        if this.notifications_visible() {
                            log.mark_all_read();
                        }
                        more = log.should_tick();
                        if !more {
                            log.mark_ticking(false);
                        }
                        cx.notify();
                    });
                    more
                });
                if !matches!(keep, Ok(true)) {
                    break;
                }
            }
            this.update(cx, |this, _| {
                this.balloon_clock = None;
            })
            .ok();
        });
        self.balloon_clock = Some(task);
    }

    /// Shows the Notifications tool window and marks the history read, as the
    /// Tauri shell did when its list became visible.
    fn show_notifications(&mut self, cx: &mut Context<Self>) {
        self.show_bottom = true;
        self.bottom = BottomTab::Notifications;
        self.notifications.update(cx, |log, cx| {
            log.mark_all_read();
            cx.notify();
        });
        cx.notify();
    }

    fn pick_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = rfd::FileDialog::new().set_title("Open Project").pick_folder();
        if let Some(path) = picked {
            self.open_project(path, window, cx);
        }
    }

    fn open_project(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        match self.host.open_project("main", &path) {
            Ok(project) => {
                let name = project
                    .root
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| project.root.display().to_string());
                window.set_window_title(&format!("OpenIDEA — {name}"));
                let _ = self.recent.touch(&project.root, None);
                self.tree.update(cx, |tree, cx| tree.set_project(&project, cx));
                self.editor.update(cx, |editor, cx| editor.set_project(&project, cx));
                self.search.update(cx, |search, cx| search.set_project(&project, cx));
                self.vcs.update(cx, |vcs, cx| vcs.set_project(&project, cx));
                self.terminal.update(cx, |terminal, cx| terminal.set_project(&project, cx));
                self.diff.update(cx, |diff, cx| diff.set_project(&project, cx));
                self.run.update(cx, |run, cx| run.set_project(&project, cx));
                self.debug.update(cx, |debug, cx| debug.set_project(&project, cx));
                self.db.update(cx, |db, cx| db.set_project(&project, cx));
                self.http.update(cx, |http, cx| http.set_project(&project, cx));
                self.notebook.update(cx, |notebook, cx| notebook.set_project(&project, cx));
                self.agents.update(cx, |agents, cx| agents.set_project(&project, cx));
                self.breakpoints = Breakpoints::load(&self.config_dir, &project.root);
                let map = self.breakpoints.map();
                self.editor.update(cx, |editor, cx| editor.set_breakpoints(map, cx));
                self.sync_all_breakpoints();
                let settings = project.settings.effective();
                self.apply_editor_font_size(settings.editor.font_size, cx);
                self.apply_terminal_font_size(settings.terminal.font_size, cx);
                self.diagnostics.borrow_mut().clear();
                self.problems.update(cx, |problems, cx| problems.refresh(cx));
                self.project = Some(project);
                self.show_project = true;
                self.left = LeftPanel::Project;
                self.set_status(format!("Opened {name}"), cx);
            }
            Err(err) => self.set_status(format!("Cannot open {}: {err}", path.display()), cx),
        }
    }

    /// Drains plugin events for the workbench's lifetime: notifications go into
    /// the Notifications log, `insertText` / `replaceSelection` reach the editor
    /// at the caret.
    fn ensure_plugin_events(&mut self, cx: &mut Context<Self>) {
        if self.plugin_event_task.is_some() {
            return;
        }
        let mut rx = std::mem::replace(
            &mut self.plugin_events,
            futures::channel::mpsc::unbounded().1,
        );
        self.plugin_event_task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    match event {
                        ide_plugin::PluginEvent::InsertText { text, .. } => {
                            this.insert_at_caret(&text, false, cx);
                        }
                        ide_plugin::PluginEvent::ReplaceSelection { text, .. } => {
                            this.insert_at_caret(&text, true, cx);
                        }
                        ide_plugin::PluginEvent::Notification { level, title, body, .. } => {
                            let level = match level {
                                ide_plugin::NotificationLevel::Error => ide_core::event::NotifyLevel::Error,
                                ide_plugin::NotificationLevel::Warning => ide_core::event::NotifyLevel::Warning,
                                _ => ide_core::event::NotifyLevel::Info,
                            };
                            let prefs = this.notification_prefs();
                            let visible = this.notifications_visible();
                            this.notifications.update(cx, |log, cx| {
                                log.set_prefs(prefs);
                                log.push(level, format!("Plugin: {title}"), (!body.is_empty()).then_some(body));
                                if visible {
                                    log.mark_all_read();
                                }
                                cx.notify();
                            });
                        }
                        ide_plugin::PluginEvent::CommandsChanged => this.refresh_plugins(cx),
                        ide_plugin::PluginEvent::ThemeChanged { .. } => {}
                    }
                    cx.notify();
                });
            }
        }));
    }

    /// Inserts `text` at the caret, or replaces the selection when
    /// `replace_selection` is set (a plugin's `insertText` / `replaceSelection`).
    fn insert_at_caret(&mut self, text: &str, replace_selection: bool, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| editor.insert_text_at_caret(text, replace_selection, cx));
        cx.notify();
    }

    fn run(&mut self, cmd: Cmd, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_plugin_events(cx);
        let tab_before = self.bottom;
        match cmd {
            Cmd::OpenProject => self.pick_project(window, cx),
            Cmd::RecentProjects => {
                self.recent_open = true;
                cx.notify();
            }
            Cmd::GoToLine => {
                self.goto_open = true;
                self.goto_submit = false;
                let handle = self.goto_input.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
                cx.notify();
            }
            Cmd::FindInEditor => {
                self.find_open = true;
                self.find_searched = false;
                self.find_index = 0;
                self.find_highlight_dirty = true;
                cx.notify();
                let entity = cx.entity();
                window.on_next_frame(move |window, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        if this.find_open {
                            let handle = this.find_input.read(cx).focus_handle(cx);
                            window.focus(&handle, cx);
                            cx.notify();
                        }
                    });
                });
            }
            Cmd::Plugins => {
                self.plugins_open = true;
                self.refresh_plugins(cx);
            }
            Cmd::FloatBottom => {
                if !self.show_bottom {
                    self.show_bottom = true;
                }
                self.float_bottom(window, cx);
            }
            Cmd::DockBottom => self.dock_bottom(window, cx),
            Cmd::ZoomIn => self.change_editor_font_size(1., cx),
            Cmd::ZoomOut => self.change_editor_font_size(-1., cx),
            Cmd::ZoomReset => {
                self.apply_editor_font_size(13., cx);
                self.persist_setting("editor.fontSize", serde_json::json!(13.));
            }
            Cmd::Save => self.editor.update(cx, |editor, cx| editor.save_active(window, cx)),
            Cmd::SaveAll => self.editor.update(cx, |editor, cx| editor.save_all(window, cx)),
            Cmd::CloseTab => self.editor.update(cx, |editor, cx| editor.close_active(window, cx)),
            Cmd::NextTab => self.editor.update(cx, |editor, cx| editor.cycle(1, cx)),
            Cmd::PrevTab => self.editor.update(cx, |editor, cx| editor.cycle(-1, cx)),
            Cmd::QuickOpen => self.open_quick_open(window, cx),
            Cmd::FindInPath => {
                tracing::debug!("show search panel");
                self.show_project = true;
                self.left = LeftPanel::Search;
                self.search.update(cx, |search, cx| search.focus(window, cx));
                cx.notify();
            }
            Cmd::ShowProject => {
                self.show_project = true;
                self.left = LeftPanel::Project;
                cx.notify();
            }
            Cmd::ShowSearch => {
                self.show_project = true;
                self.left = LeftPanel::Search;
                self.search.update(cx, |search, cx| search.focus(window, cx));
                cx.notify();
            }
            Cmd::ShowVcs => {
                tracing::debug!("show vcs panel");
                self.show_project = true;
                self.left = LeftPanel::Vcs;
                self.vcs.update(cx, |vcs, cx| vcs.refresh(cx));
                cx.notify();
            }
            Cmd::ToggleProject => {
                if self.show_project && self.left == LeftPanel::Project {
                    self.show_project = false;
                } else {
                    self.show_project = true;
                    self.left = LeftPanel::Project;
                }
                cx.notify();
            }
            Cmd::ToggleProblems => self.toggle_bottom(BottomTab::Problems, cx),
            Cmd::ToggleTerminal => {
                self.toggle_bottom(BottomTab::Terminal, cx);
                if self.show_bottom {
                    self.terminal.update(cx, |terminal, cx| terminal.ensure(window, cx));
                }
            }
            Cmd::CommandPalette => self.open_palette(window, cx),
            Cmd::RefreshProject => {
                self.tree.update(cx, |tree, cx| tree.refresh(cx));
                self.set_status("Project tree refreshed", cx);
            }
            Cmd::RefreshVcs => {
                self.vcs.update(cx, |vcs, cx| vcs.refresh(cx));
                self.set_status("Git status refreshed", cx);
            }
            Cmd::ToggleTheme => self.set_theme(self.theme.counterpart(), cx),
            Cmd::Run => {
                self.show_bottom = true;
                self.bottom = BottomTab::Run;
                self.run.update(cx, |run, cx| run.start_selected(cx));
            }
            Cmd::StopRun => {
                self.run.update(cx, |run, cx| run.stop(cx));
            }
            Cmd::EditRunConfigurations => {
                self.show_bottom = true;
                self.bottom = BottomTab::Run;
                self.run.update(cx, |run, cx| run.open_editor(true, window, cx));
                cx.notify();
            }
            Cmd::ShowRun => {
                self.show_bottom = true;
                self.bottom = BottomTab::Run;
            }
            Cmd::Debug => {
                self.show_bottom = true;
                self.bottom = BottomTab::Debug;
                let Some(config) = self.run.update(cx, |run, _| run.selected_config()) else {
                    self.debug.update(cx, |debug, cx| debug.show_notice("No run configuration selected", cx));
                    return;
                };
                let Some(project) = self.project.clone() else {
                    self.debug.update(cx, |debug, cx| debug.show_notice("Open a project first", cx));
                    return;
                };
                match crate::debug::launch_config_for(&config, &project.root) {
                    Some(launch) => self.debug.update(cx, |debug, cx| debug.start(launch, cx)),
                    None => self.debug.update(cx, |debug, cx| {
                        debug.show_notice(
                            format!("`{}` has no debug adapter configured (debug-adapter in run.toml)", config.name),
                            cx,
                        )
                    }),
                }
            }
            Cmd::ShowDebug => {
                self.show_bottom = true;
                self.bottom = BottomTab::Debug;
            }
            Cmd::StopDebug => self.debug.update(cx, |debug, cx| debug.act(DebugAction::Stop, cx)),
            Cmd::ResumeDebug => self.debug.update(cx, |debug, cx| debug.act(DebugAction::Resume, cx)),
            Cmd::PauseDebug => self.debug.update(cx, |debug, cx| debug.act(DebugAction::Pause, cx)),
            Cmd::StepInto => self.debug.update(cx, |debug, cx| debug.act(DebugAction::StepInto, cx)),
            Cmd::StepOver => self.debug.update(cx, |debug, cx| debug.act(DebugAction::StepOver, cx)),
            Cmd::StepOut => self.debug.update(cx, |debug, cx| debug.act(DebugAction::StepOut, cx)),
            Cmd::ShowDb => {
                self.show_bottom = true;
                self.bottom = BottomTab::Db;
                cx.notify();
            }
            Cmd::ShowHttp => {
                self.show_bottom = true;
                self.bottom = BottomTab::Http;
                if let (Some((path, text, index)), Some(project)) = (self.active_http_file(cx), self.project.clone()) {
                    self.http.update(cx, |http, cx| http.use_file(path, text, &project.root, index, Some(window), cx));
                }
                cx.notify();
            }
            Cmd::ShowNotebook => {
                self.show_bottom = true;
                self.bottom = BottomTab::Notebook;
                if let Some(info) = self.editor.read(cx).active_info()
                    && info.path.extension().and_then(|ext| ext.to_str()) == Some("ipynb")
                {
                    let path = info.path.clone();
                    let changed = self.notebook.read(cx).path() != Some(&path);
                    if changed {
                        self.notebook.update(cx, |notebook, cx| notebook.open_path(path, cx));
                    }
                }
                cx.notify();
            }
            Cmd::RunHttpAtCaret => {
                self.show_bottom = true;
                self.bottom = BottomTab::Http;
                let Some((path, text, index)) = self.active_http_file(cx) else {
                    self.status = "Open a .http file in the editor first".into();
                    cx.notify();
                    return;
                };
                let Some(project) = self.project.clone() else { return };
                self.http.update(cx, |http, cx| {
                    http.use_file(path, text, &project.root, index, Some(window), cx);
                    http.run_current(cx);
                });
            }
            Cmd::Undo => self.dispatch_editor(&gpui_kit::base::input::Undo, window, cx),
            Cmd::Redo => self.dispatch_editor(&gpui_kit::base::input::Redo, window, cx),
            Cmd::Cut => self.dispatch_editor(&gpui_kit::base::input::Cut, window, cx),
            Cmd::Copy => self.dispatch_editor(&gpui_kit::base::input::Copy, window, cx),
            Cmd::Paste => self.dispatch_editor(&gpui_kit::base::input::Paste, window, cx),
            Cmd::SelectAll => self.dispatch_editor(&gpui_kit::base::input::SelectAll, window, cx),
            Cmd::ToggleBreakpoint => self.toggle_breakpoint(cx),
            Cmd::ViewBreakpoints => {
                self.breakpoints_open = true;
                self.selected_breakpoint = None;
                cx.notify();
            }
            Cmd::Settings => {
                self.settings_open = true;
                cx.notify();
            }
            Cmd::Appearance => {
                self.appearance_open = true;
                cx.notify();
            }
            Cmd::TrustProject => self.trust_project(cx),
            Cmd::About => {
                self.about_open = true;
                cx.notify();
            }
            Cmd::ShowAgents => {
                self.show_bottom = true;
                self.bottom = BottomTab::Agents;
                self.agents.update(cx, |agents, cx| agents.ensure(window, cx));
                cx.notify();
            }
            Cmd::ShowNotifications => self.show_notifications(cx),
            Cmd::Intentions => self.show_intentions(window, cx),
            Cmd::Rename => self.start_rename(window, cx),
            Cmd::ShowDuplicates => {
                self.show_bottom = true;
                self.bottom = BottomTab::Duplicates;
                self.duplicates.update(cx, |panel, cx| panel.run(cx));
                cx.notify();
            }
            Cmd::ShowStructural => {
                self.show_bottom = true;
                self.bottom = BottomTab::Structural;
                let focus = self.structural.read(cx).input_focus(cx);
                window.focus(&focus, cx);
                cx.notify();
            }
            Cmd::ShowStructure => {
                self.show_bottom = true;
                self.bottom = BottomTab::Structure;
                self.structure.update(cx, |panel, cx| panel.refresh(cx));
                cx.notify();
            }
            Cmd::ShowSsh => {
                self.show_bottom = true;
                self.bottom = BottomTab::Ssh;
                self.ssh.update(cx, |panel, cx| panel.reload(cx));
                cx.notify();
            }
            Cmd::ShowDeploy => {
                self.show_bottom = true;
                self.bottom = BottomTab::Deploy;
                self.deploy.update(cx, |panel, cx| panel.reload(cx));
                cx.notify();
            }
            Cmd::Quit => cx.quit(),
        }
        // Most commands pick a tool window by assigning `bottom`; point the dock
        // at it so the two stay in step.
        if self.bottom != tab_before {
            self.sync_dock_selection(window, cx);
        }
    }

    fn apply_editor_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        self.editor_font_size = size.clamp(8., 28.);
        self.editor.update(cx, |editor, cx| editor.set_font_size(self.editor_font_size, cx));
        cx.notify();
    }

    fn apply_terminal_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        self.terminal_font_size = size.clamp(8., 28.);
        self.terminal.update(cx, |terminal, cx| terminal.set_font_size(self.terminal_font_size, cx));
        cx.notify();
    }

    fn change_editor_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        let size = (self.editor_font_size + delta).clamp(8., 28.);
        self.apply_editor_font_size(size, cx);
        self.persist_setting("editor.fontSize", serde_json::json!(size));
    }

    fn change_terminal_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        let size = (self.terminal_font_size + delta).clamp(8., 28.);
        self.apply_terminal_font_size(size, cx);
        self.persist_setting("terminal.fontSize", serde_json::json!(size));
    }

    fn find_step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.find_input.read(cx).value().trim().to_owned();
        if query.is_empty() {
            return;
        }
        let matches = self.editor.read(cx).find_matches(&query, self.find_options);
        if matches.is_empty() {
            self.status = format!("No matches for \"{query}\"").into();
            self.find_searched = false;
            cx.notify();
            return;
        }
        let len = matches.len();
        let index = if !self.find_searched {
            if delta < 0 { len - 1 } else { 0 }
        } else if delta < 0 {
            (self.find_index + len - 1) % len
        } else {
            (self.find_index + 1) % len
        };
        self.find_index = index;
        self.find_searched = true;
        let (line, col) = matches[index];
        self.status = format!("{}/{} · line {}", index + 1, len, line + 1).into();
        self.editor.update(cx, |editor, cx| editor.goto_line(line, col, window, cx));
        cx.notify();
    }

    fn current_settings(&self) -> ide_core::settings::Settings {
        match &self.project {
            Some(project) => project.settings.effective(),
            None => self.host.global_settings.effective(),
        }
    }

    fn refresh_plugins(&mut self, cx: &mut Context<Self>) {
        let Some(plugins) = self.plugins.clone() else {
            self.plugins_notice = Some("Plugin host unavailable".into());
            cx.notify();
            return;
        };
        let root = self.project.as_ref().map(|project| project.root.clone());
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            plugins.wait_initial_load(std::time::Duration::from_secs(3));
            if let Some(root) = &root {
                plugins.sync_project(root);
            }
            let snapshot = plugins.list(root.as_deref());
            let _ = tx.unbounded_send(snapshot);
        });
        self.plugin_task = Some(cx.spawn(async move |this, cx| {
            while let Some(snapshot) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    this.plugins_list = snapshot.plugins;
                    this.plugins_notice = None;
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    /// Runs a plugin command (`git.commit`, a manifest's `commands[].id`) and
    /// shows its return value, if any, in the status bar. Effects the plugin
    /// asked for (insertions, notifications) arrive through the event stream.
    pub fn run_plugin_command(
        &mut self,
        plugin_id: String,
        command_id: String,
        arg: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(plugins) = self.plugins.clone() else {
            self.status = "Plugins are unavailable".into();
            cx.notify();
            return;
        };
        // The plugin sees the unsaved buffer, so a command that formats or
        // analyses the file works on what is on screen. The closure is taken
        // once here (the editor is not shared across threads).
        let active_path = self.editor.read(cx).active_info().map(|info| info.path);
        let active_text = self.editor.read(cx).active_text();
        let buffer_text: Option<ide_plugin::BufferText> = active_path.zip(active_text).map(|(path, text)| {
            std::sync::Arc::new(move |asked: &std::path::Path| (asked == path).then(|| text.clone()))
                as ide_plugin::BufferText
        });
        let workspace_root = self.project.as_ref().map(|project| project.root.clone());
        let invocation =
            ide_plugin::Invocation { client: Some("main".to_owned()), workspace_root, buffer_text };
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<Result<Option<String>, String>>();
        let status_command = command_id.clone();
        std::thread::Builder::new()
            .name("plugin-command".into())
            .spawn(move || {
                let result = plugins
                    .run_command(&plugin_id, &command_id, arg.as_deref(), invocation)
                    .map_err(|err| err.to_string());
                let _ = tx.unbounded_send(result);
            })
            .expect("plugin command thread");
        self.plugin_task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok(Some(value)) if !value.is_empty() => this.status = value.into(),
                        Ok(_) => this.status = format!("Ran {status_command}").into(),
                        Err(err) => this.status = format!("{status_command} failed: {err}").into(),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    fn plugin_action(&mut self, action: impl FnOnce() -> Result<(), String> + Send + 'static, cx: &mut Context<Self>) {
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            let _ = tx.unbounded_send(action());
        });
        self.plugin_task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| match result {
                    Ok(()) => this.refresh_plugins(cx),
                    Err(err) => {
                        this.plugins_notice = Some(err.into());
                        cx.notify();
                    }
                });
            }
        }));
        cx.notify();
    }

    fn float_bottom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let bounds = window.bounds();
        self.float_panel = Some(float_bounds(bounds.size.width.as_f32(), bounds.size.height.as_f32()));
        self.float_drag = None;
        self.status = format!("{} floated", self.bottom.label()).into();
        cx.notify();
    }

    /// Undocks the tool window: a card parked on an edge floats free again;
    /// a floating card returns to the bottom tool area.
    fn dock_bottom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let bounds = window.bounds();
        match self.float_panel {
            Some(panel) if panel.edge.is_some() => {
                self.float_panel = Some(float_bounds(bounds.size.width.as_f32(), bounds.size.height.as_f32()));
                self.float_drag = None;
                self.status = format!("{} floated", self.bottom.label()).into();
                cx.notify();
            }
            Some(_) => {
                self.float_panel = None;
                self.float_drag = None;
                self.status = "Tool window docked".into();
                cx.notify();
            }
            None => {}
        }
    }

    fn change_tab_size(&mut self, delta: i32, cx: &mut Context<Self>) {
        let settings = self.current_settings();
        let next = (settings.editor.tab_size as i32 + delta).clamp(1, 8) as u32;
        self.persist_setting("editor.tabSize", serde_json::json!(next));
        cx.notify();
    }

    /// Opens the shortcut dialog for one command (Settings › Keymap).
    fn open_keymap_dialog(
        &mut self,
        id: &'static str,
        label: SharedString,
        current: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.keymap_dialog.update(cx, |dialog, cx| dialog.open(id, label, current, window, cx));
        if self.keymap_accepted.is_none() {
            let subscription = cx.subscribe(
                &self.keymap_dialog,
                |this: &mut Self, _dialog, _: &crate::keymap::KeymapAccepted, cx| {
                    this.apply_keymap(cx);
                },
            );
            self.keymap_accepted = Some(subscription);
        }
        cx.notify();
    }

    /// Writes the dialog's shortcut to `settings.keymap_overrides` and rebinds it
    /// in the live keymap.
    fn apply_keymap(&mut self, cx: &mut Context<Self>) {
        let Some(command) = self.keymap_dialog.read(cx).command else { return };
        let keys = self.keymap_dialog.read(cx).keys(cx);
        if let Err(error) = crate::keymap::validate_keystroke(&keys) {
            self.keymap_dialog.update(cx, |dialog, cx| {
                dialog.error = Some(error.into());
                cx.notify();
            });
            return;
        }
        match crate::keymap::apply_override(cx, command, &keys) {
            Ok(()) => {
                let value = crate::keymap::override_value(&keys);
                let json = if value.is_empty() {
                    serde_json::json!([])
                } else {
                    serde_json::json!(value)
                };
                self.persist_setting(&format!("keymapOverrides.{command}"), json);
                self.keymap_dialog.update(cx, |dialog, cx| dialog.close(cx));
            }
            Err(error) => {
                self.keymap_dialog.update(cx, |dialog, cx| {
                    dialog.error = Some(error.into());
                    cx.notify();
                });
            }
        }
        cx.notify();
    }

    fn persist_setting(&mut self, key: &str, value: serde_json::Value) {
        let store = match &self.project {
            Some(project) => project.settings.as_ref(),
            None => &self.host.global_settings,
        };
        match store.set(SettingsScope::Global, key, value) {
            Ok(_) => {
                self.status = format!("Saved {key}").into();
            }
            Err(err) => {
                self.status = format!("Cannot save {key}: {err}").into();
            }
        }
    }

    /// Sends an editor action (Undo, Copy, …) to the focused editor.
    ///
    /// The Edit menu dispatches through here: the actions are the editor's own,
    /// so a menu click and the keystroke do exactly the same thing. Without an
    /// editor focus (or with no tab open) the action simply does nothing.
    fn dispatch_editor(&mut self, action: &dyn gpui_kit::Action, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.editor.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        focus.dispatch_action(action, window, cx);
        cx.notify();
    }

    /// Runs the `.http` request the caret is in (what the gutter run mark and
    /// the `Ctrl+Alt+R` action both do).
    pub fn run_http_at_caret(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run(Cmd::RunHttpAtCaret, window, cx);
    }

    fn toggle_breakpoint(&mut self, cx: &mut Context<Self>) {
        let Some((path, line)) = self.editor.read(cx).active_location(cx) else {
            self.status = "Open a file to set a breakpoint".into();
            cx.notify();
            return;
        };
        let enabled = self.breakpoints.toggle(&path, line);
        self.breakpoints.save(&self.config_dir);
        let map = self.breakpoints.map();
        self.editor.update(cx, |editor, cx| editor.set_breakpoints(map, cx));
        self.sync_breakpoints(path.clone());
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        self.status = format!(
            "Breakpoint {} at {name}:{} ({} total)",
            if enabled { "set" } else { "removed" },
            line + 1,
            self.breakpoints.count()
        )
        .into();
        cx.notify();
    }

    fn sync_breakpoints(&self, path: PathBuf) {
        let entries = self.breakpoints.entries(&path);
        let host = self.host.clone();
        let runtime = host.runtime.clone();
        runtime.spawn(async move {
            let list = entries
                .into_iter()
                .map(|entry| ide_dap::Breakpoint {
                    line: entry.line,
                    condition: entry.condition,
                    hit_condition: None,
                    log_message: entry.log_message,
                    enabled: entry.enabled,
                })
                .collect();
            if let Err(err) = host.debugger.set_breakpoints(path, list).await {
                tracing::debug!(error = %err, "breakpoint sync failed");
            }
        });
    }

    fn active_http_file(&self, cx: &App) -> Option<(PathBuf, String, usize)> {
        let (path, line) = self.editor.read(cx).active_location(cx)?;
        let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();
        if extension != "http" && extension != "rest" {
            return None;
        }
        let text = self.project.as_ref()?.documents.text_of(&path)?;
        let parsed = ide_http::parse::parse(&text);
        if parsed.requests.is_empty() {
            return None;
        }
        let index = parsed.requests.iter().rposition(|request| request.block.start_line <= line).unwrap_or(0);
        Some((path, text, index))
    }

    fn apply_log_message(&mut self, cx: &mut Context<Self>) {
        let Some((path, line)) = self.selected_breakpoint.clone() else {
            self.status = "Select a breakpoint first".into();
            cx.notify();
            return;
        };
        let value = self.log_input.read(cx).value().trim().to_owned();
        self.breakpoints.set_log_message(&path, line, (!value.is_empty()).then_some(value.clone()));
        self.breakpoints.save(&self.config_dir);
        self.sync_breakpoints(path.clone());
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        self.status = if value.is_empty() {
            format!("Log message cleared for {name}:{}", line + 1).into()
        } else {
            format!("Log message set for {name}:{}: {value}", line + 1).into()
        };
        cx.notify();
    }

    fn apply_condition(&mut self, cx: &mut Context<Self>) {
        let Some((path, line)) = self.selected_breakpoint.clone() else {
            self.status = "Select a breakpoint first".into();
            cx.notify();
            return;
        };
        let value = self.condition_input.read(cx).value().trim().to_owned();
        self.breakpoints.set_condition(&path, line, (!value.is_empty()).then_some(value.clone()));
        self.breakpoints.save(&self.config_dir);
        self.sync_breakpoints(path.clone());
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        self.status = if value.is_empty() {
            format!("Condition cleared for {name}:{}", line + 1).into()
        } else {
            format!("Condition set for {name}:{}: {value}", line + 1).into()
        };
        cx.notify();
    }

    fn sync_all_breakpoints(&self) {
        for path in self.breakpoints.map().keys() {
            self.sync_breakpoints(path.clone());
        }
    }

    fn set_theme(&mut self, choice: theme::ThemeChoice, cx: &mut Context<Self>) {
        self.theme = choice;
        theme::install_choice(cx, choice);
        theme::save_choice(&self.config_dir, choice);
        cx.notify();
    }

    /// Ask `ide-host` to record the decision and start the language servers,
    /// Git hooks and run configurations for this project.
    fn trust_project(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        let host = self.host.clone();
        let root = project.root.clone();
        tracing::info!(root = ?root, "trusting project");
        self.set_status("Trusting project…", cx);
        cx.spawn(async move |this, cx| {
            let result = host.set_project_trust(&root, true, true).await;
            tracing::info!(?result, "trust request finished");
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => this.set_status("Project trusted — language servers and Git hooks enabled", cx),
                Err(err) => this.set_status(format!("Cannot trust project: {err}"), cx),
            });
        })
        .detach();
    }

    fn toggle_bottom(&mut self, tab: BottomTab, cx: &mut Context<Self>) {
        if self.show_bottom && self.bottom == tab {
            self.show_bottom = false;
        } else {
            self.bottom = tab;
            self.show_bottom = true;
        }
        cx.notify();
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette_mode(PaletteMode::Commands, window, cx);
    }

    fn open_quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette_mode(PaletteMode::Files, window, cx);
    }

    fn open_palette_mode(&mut self, mode: PaletteMode, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_open = true;
        self.palette_mode = mode;
        self.palette_return = window.focused(cx);
        self.palette_query.clear();
        self.palette_index = 0;
        if mode == PaletteMode::Files {
            self.refresh_quick();
        }
        let handle = self.palette_focus.clone();
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Files for the Go to File picker: fuzzy index search, recent files first.
    fn refresh_quick(&mut self) {
        self.quick.clear();
        let Some(project) = self.project.clone() else { return };
        let query = self.palette_query.trim().to_owned();
        if query.is_empty() {
            for path in project.index.recent_files() {
                let rel = path.strip_prefix(&project.root).unwrap_or(&path).display().to_string();
                self.quick.push((path, rel.into()));
            }
            if self.quick.is_empty() {
                for file in project.index.find_files("", 50) {
                    self.quick.push((file.path.0, file.rel_path.into()));
                }
            }
        } else {
            for file in project.index.find_files(&query, 50) {
                self.quick.push((file.path.0, file.rel_path.into()));
            }
        }
        self.quick.truncate(50);
    }

    /// Close the palette and give keyboard focus back to whatever had it.
    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_open = false;
        self.palette_query.clear();
        match self.palette_return.take() {
            Some(handle) if handle.is_focused(window) => {}
            Some(handle) => window.focus(&handle, cx),
            None => self.editor.update(cx, |editor, cx| editor.focus_current(window, cx)),
        }
        cx.notify();
    }

    fn filtered_palette(&self) -> Vec<&'static MenuEntry> {
        let query = self.palette_query.to_lowercase();
        PALETTE.iter().filter(|entry| query.is_empty() || entry.label.to_lowercase().contains(&query)).collect()
    }

    fn palette_len(&self) -> usize {
        match self.palette_mode {
            PaletteMode::Commands => {
                let commands = self.filtered_palette().len();
                let plugins = self.plugin_command_count();
                commands + plugins
            }
            PaletteMode::Files => self.quick.len(),
        }
    }

    /// The plugin commands the palette lists under the current query.
    fn plugin_command_list(&self) -> Vec<(String, String, String)> {
        let query = self.palette_query.to_lowercase();
        self.plugins_list
            .iter()
            .filter(|plugin| plugin.enabled && plugin.status == PluginStatus::Active)
            .flat_map(|plugin| {
                let query = query.clone();
                plugin.commands.iter().filter_map(move |command| {
                    let title = command
                        .category
                        .as_ref()
                        .map(|category| format!("{category}: {}", command.title))
                        .unwrap_or_else(|| command.title.clone());
                    (query.is_empty() || title.to_lowercase().contains(query.as_str()))
                        .then(|| (plugin.id.clone(), command.id.clone(), title))
                })
            })
            .collect()
    }

    fn plugin_command_count(&self) -> usize {
        self.plugin_command_list().len()
    }

    /// The `(plugin id, command id)` of the palette's `index`-th plugin row.
    fn plugin_command_at(&self, index: usize) -> Option<(String, String)> {
        self.plugin_command_list().get(index).map(|(plugin, command, _)| (plugin.clone(), command.clone()))
    }

    /// Keys that the intentions popup owns while it is open: Up/Down walk the
    /// list, Enter applies the selected action, Esc closes it.
    fn intentions_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.keymap_dialog.read(cx).is_open() {
            if event.keystroke.key == "escape" {
                self.keymap_dialog.update(cx, |dialog, cx| dialog.close(cx));
                cx.stop_propagation();
            }
            return true;
        }
        if self.rename_open {
            if event.keystroke.key == "escape" {
                self.rename_open = false;
                self.rename.update(cx, |dialog, cx| dialog.close(cx));
                self.editor.update(cx, |editor, cx| editor.focus_current(window, cx));
                cx.stop_propagation();
            }
            return true;
        }
        if !self.intentions.read(cx).open {
            return false;
        }
        let key = event.keystroke.key.as_str();
        let consumed = self.intentions.update(cx, |section, cx| section.on_key(key, cx));
        if consumed {
            cx.stop_propagation();
            if !self.intentions.read(cx).open {
                self.editor.update(cx, |editor, cx| editor.focus_current(window, cx));
            }
        }
        consumed
    }

    fn palette_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "escape" => self.close_palette(window, cx),
            "enter" => {
                if self.palette_mode == PaletteMode::Files {
                    if let Some((path, _)) = self.quick.get(self.palette_index).cloned() {
                        self.close_palette(window, cx);
                        self.editor.update(cx, |editor, cx| editor.open_file_at(path, 0, 0, window, cx));
                    }
                } else {
                    let entries = self.filtered_palette();
                    if let Some(entry) = entries.get(self.palette_index) {
                        let cmd = entry.cmd;
                        self.close_palette(window, cx);
                        self.run(cmd, window, cx);
                    } else if let Some((plugin_id, command_id)) =
                        self.plugin_command_at(self.palette_index.saturating_sub(entries.len()))
                    {
                        self.close_palette(window, cx);
                        self.run_plugin_command(plugin_id, command_id, None, cx);
                    }
                }
            }
            "up" => {
                self.palette_index = self.palette_index.saturating_sub(1);
                cx.notify();
            }
            "down" => {
                let len = self.palette_len();
                if len > 0 {
                    self.palette_index = (self.palette_index + 1).min(len - 1);
                }
                cx.notify();
            }
            "backspace" => {
                self.palette_query.pop();
                self.palette_index = 0;
                if self.palette_mode == PaletteMode::Files {
                    self.refresh_quick();
                }
                cx.notify();
            }
            _ => {
                if let Some(text) = &keystroke.key_char
                    && !keystroke.modifiers.control
                    && !keystroke.modifiers.platform
                {
                    self.palette_query.push_str(text);
                    self.palette_index = 0;
                    if self.palette_mode == PaletteMode::Files {
                        self.refresh_quick();
                    }
                    cx.notify();
                }
            }
        }
    }

    fn render_menu_bar(&mut self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut bar = div()
            .h(px(30.))
            .w_full()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.border);
        for kind in MENU_KINDS {
            let open = self.open_menu == Some(kind);
            let click = cx.listener(move |this: &mut Self, event: &ClickEvent, _window, cx| {
                this.open_menu = if this.open_menu == Some(kind) { None } else { Some(kind) };
                this.menu_pos = event.position();
                cx.notify();
            });
            bar = bar.child(
                div()
                    .id(ElementId::Name(format!("menu-{}", kind.label()).into()))
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(theme.foreground)
                    .when(open, |this| this.bg(theme.secondary))
                    .hover(move |this| this.bg(theme.secondary_hover))
                    .on_click(click)
                    .child(kind.label()),
            );
        }
        bar.child(div().flex_1())
            .child(
                div()
                    .px_2()
                    .max_w(px(520.))
                    .truncate()
                    .text_size(px(11.))
                    .text_color(theme.fg_muted())
                    .child(self.project.as_ref().map(|p| p.root.display().to_string()).unwrap_or_default()),
            )
            .into_any_element()
    }

    fn render_menu_dropdown(
        &mut self,
        kind: MenuKind,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut card = div()
            .absolute()
            .top(px(30.))
            .left(self.menu_pos.x)
            .w(px(260.))
            .py_1()
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border_strong())
            .rounded(px(6.))
            .shadow_lg();
        for entry in menu_entries(kind) {
            let cmd = entry.cmd;
            let click = cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                this.open_menu = None;
                this.run(cmd, window, cx);
            });
            card = card.child(
                div()
                    .id(ElementId::Name(format!("menu-item-{}-{}", kind.label(), entry.label).into()))
                    .h(px(26.))
                    .px_3()
                    .mx_1()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_6()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(move |this| this.bg(theme.list_hover))
                    .on_click(click)
                    .child(div().text_size(px(12.)).text_color(theme.popover_foreground).child(entry.label))
                    .child(
                        div().text_size(px(11.)).text_color(theme.fg_muted()).child(entry.shortcut.unwrap_or_default()),
                    ),
            );
        }
        card.into_any_element()
    }

    fn render_stripe(&mut self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut stripe = div()
            .w(px(40.))
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .py_2()
            .gap_1()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border);
        stripe = stripe
            .child(self.tool_button(
                "tool-project",
                IconName::FolderOpen,
                self.show_project && self.left == LeftPanel::Project,
                theme,
                cx,
                Cmd::ShowProject,
            ))
            .child(self.tool_button(
                "tool-search",
                IconName::Search,
                self.show_project && self.left == LeftPanel::Search,
                theme,
                cx,
                Cmd::ShowSearch,
            ))
            .child(self.tool_button(
                "tool-vcs",
                IconName::GitBranch,
                self.show_project && self.left == LeftPanel::Vcs,
                theme,
                cx,
                Cmd::ShowVcs,
            ))
            .child(self.tool_button(
                "tool-agents",
                IconName::Bot,
                self.show_bottom && self.bottom == BottomTab::Agents,
                theme,
                cx,
                Cmd::ShowAgents,
            ))
            .child(div().flex_1())
            .child(self.tool_button(
                "tool-problems",
                IconName::CircleAlert,
                self.show_bottom && self.bottom == BottomTab::Problems,
                theme,
                cx,
                Cmd::ToggleProblems,
            ))
            .child(self.tool_button(
                "tool-terminal",
                IconName::SquareTerminal,
                self.show_bottom && self.bottom == BottomTab::Terminal,
                theme,
                cx,
                Cmd::ToggleTerminal,
            ))
            .child(self.tool_button("tool-settings", IconName::Settings, false, theme, cx, Cmd::About));
        stripe.into_any_element()
    }

    fn tool_button(
        &self,
        id: &'static str,
        icon: IconName,
        active: bool,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
        cmd: Cmd,
    ) -> AnyElement {
        let click = cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| this.run(cmd, window, cx));
        div()
            .id(ElementId::Name(id.into()))
            .size(px(30.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.))
            .cursor_pointer()
            .when(active, |this| this.bg(theme.secondary))
            .hover(move |this| this.bg(theme.secondary_hover))
            .on_click(click)
            .child(Icon::new(icon).small().text_color(if active { theme.foreground } else { theme.fg_muted() }))
            .into_any_element()
    }

    fn render_bottom(&mut self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> AnyElement {
        let height = self.layout.heights[self.bottom.index()];
        let float_hover = theme.list_hover;
        // The dock draws its own tab bar; the workbench keeps the height, the
        // splitter and the Float control around it.
        let body: AnyElement = match self.dock.clone() {
            Some(dock) => div().size_full().min_h_0().child(dock).into_any_element(),
            None => self.render_bottom_tab(),
        };
        div()
            .h(px(height))
            .w_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .h(px(26.))
                    .w_full()
                    .flex()
                    .items_center()
                    .px_2()
                    .gap_2()
                    .bg(theme.tab_bar)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.fg_secondary())
                            .child(self.bottom.label()),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("bottom-float")
                            .h_full()
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_1()
                            .cursor_pointer()
                            .hover(move |this| this.bg(float_hover))
                            .on_click(cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| {
                                this.float_bottom(window, cx)
                            }))
                            .text_size(px(10.))
                            .text_color(theme.fg_secondary())
                            .child(Icon::new(IconName::Maximize2).xsmall())
                            .child("Float"),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    fn render_bottom_tab(&self) -> AnyElement {
        match self.bottom {
            BottomTab::Problems => div().flex_1().min_h_0().child(self.problems.clone()),
            BottomTab::Terminal => div().flex_1().min_h_0().child(self.terminal.clone()),
            BottomTab::Diff => div().flex_1().min_h_0().child(self.diff.clone()),
            BottomTab::Run => div().flex_1().min_h_0().child(self.run.clone()),
            BottomTab::Debug => div().flex_1().min_h_0().child(self.debug.clone()),
            BottomTab::Db => div().flex_1().min_h_0().child(self.db.clone()),
            BottomTab::Http => div().flex_1().min_h_0().child(self.http.clone()),
            BottomTab::Notebook => div().flex_1().min_h_0().child(self.notebook.clone()),
            BottomTab::Agents => div().flex_1().min_h_0().child(self.agents.clone()),
            BottomTab::Duplicates => div().flex_1().min_h_0().child(self.duplicates.clone()),
            BottomTab::Structural => div().flex_1().min_h_0().child(self.structural.clone()),
            BottomTab::Structure => div().flex_1().min_h_0().child(self.structure.clone()),
            BottomTab::Ssh => div().flex_1().min_h_0().child(self.ssh.clone()),
            BottomTab::Deploy => div().flex_1().min_h_0().child(self.deploy.clone()),
            BottomTab::Notifications => div().flex_1().min_h_0().child(self.notifications.clone()),
        }
        .into_any_element()
    }

    fn render_status_bar(&mut self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> AnyElement {
        let info: Option<TabInfo> = self.editor.read(cx).active_info();
        let (errors, warnings) = self.diagnostics.borrow().counts();
        let mut bar = div()
            .h(px(24.))
            .w_full()
            .px_3()
            .flex()
            .items_center()
            .gap_4()
            .bg(theme.status_bar)
            .border_t_1()
            .border_color(theme.status_bar_border)
            .text_size(px(11.));
        bar =
            bar.child(div().flex_1().min_w_0().truncate().text_color(theme.fg_secondary()).child(self.status.clone()));
        if let Some(branch) = self.vcs.read(cx).branch() {
            bar = bar.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_color(theme.fg_secondary())
                    .child(Icon::new(IconName::GitBranch).xsmall().text_color(theme.fg_muted()))
                    .child(branch),
            );
        }
        if let Some(lsp) = self.lsp.clone() {
            bar = bar.child(div().text_color(theme.fg_muted()).child(lsp));
        }
        if let Some(indexing) = self.indexing.clone() {
            bar = bar.child(div().text_color(theme.info).child(indexing));
        }
        if errors > 0 {
            bar = bar.child(div().text_color(theme.danger).child(format!("{errors} errors")));
        }
        if warnings > 0 {
            bar = bar.child(div().text_color(theme.warning).child(format!("{warnings} warnings")));
        }
        if let Some(info) = info {
            if info.read_only {
                bar = bar.child(div().text_color(theme.fg_muted()).child("Read-only"));
            }
            if info.dirty {
                bar = bar.child(div().text_color(theme.warning).child("Unsaved"));
            }
            bar = bar
                .child(div().text_color(theme.fg_muted()).child(info.line_ending))
                .child(div().text_color(theme.fg_muted()).child(info.encoding))
                .child(div().text_color(theme.fg_secondary()).child(info.language))
                .child(div().text_color(theme.fg_muted()).child(info.name));
        }
        let unread = self.notifications.read(cx).unread();
        let unread_hover = theme.list_hover;
        bar = bar.when(unread > 0, |this| {
            this.child(
                div()
                    .id("status-notifications")
                    .test_support()
                    .h(px(18.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(move |this| this.bg(unread_hover))
                    .on_click(cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                        this.show_notifications(cx);
                    }))
                    .text_color(theme.warning)
                    .child(Icon::new(IconName::CircleAlert).xsmall())
                    .child(format!("{unread}")),
            )
        });
        bar.child(div().text_color(theme.fg_muted()).child("GPUI")).into_any_element()
    }

    fn render_welcome(&mut self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> AnyElement {
        let recent = self.recent.list();
        let open_click = cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| this.pick_project(window, cx));
        let mut column = div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .bg(theme.background)
            .child(
                div()
                    .text_size(px(34.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child("OpenIDEA"),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(theme.fg_secondary())
                    .child("The IDE you know, rebuilt in Rust — now on GPUI."),
            )
            .child(Button::new("welcome-open-project").label("Open Project…").primary().on_click(open_click));
        if !recent.is_empty() {
            column =
                column.child(div().mt_2().text_size(px(11.)).text_color(theme.fg_muted()).child("RECENT PROJECTS"));
            for entry in recent.into_iter().take(8) {
                let path = entry.path.0.clone();
                let click = cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                    this.open_project(path.clone(), window, cx);
                });
                column = column.child(
                    div()
                        .id(ElementId::Name(format!("recent-{}", entry.name).into()))
                        .w(px(460.))
                        .h(px(30.))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_3()
                        .rounded(px(5.))
                        .cursor_pointer()
                        .hover(move |this| this.bg(theme.list_hover))
                        .on_click(click)
                        .child(Icon::new(IconName::Folder).small().text_color(theme.fg_muted()))
                        .child(div().text_size(px(12.5)).text_color(theme.foreground).child(entry.name))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(10.5))
                                .text_color(theme.fg_muted())
                                .child(entry.path.0.display().to_string()),
                        ),
                );
            }
        }
        column.into_any_element()
    }

    fn render_palette(&mut self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> AnyElement {
        let files_mode = self.palette_mode == PaletteMode::Files;
        let mut list = div().flex().flex_col().py_1();
        let count;

        if files_mode {
            let editor = self.editor.clone();
            for (index, (path, rel)) in self.quick.iter().enumerate() {
                let selected = index == self.palette_index;
                let path = path.clone();
                let editor = editor.clone();
                let click = cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                    this.close_palette(window, cx);
                    editor.update(cx, |editor, cx| editor.open_file_at(path.clone(), 0, 0, window, cx));
                });
                list = list.child(
                    div()
                        .id(ElementId::Name(format!("quick-file-{index}").into()))
                        .h(px(26.))
                        .px_3()
                        .mx_1()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .when(selected, |this| this.bg(theme.list_active))
                        .hover(move |this| this.bg(theme.list_hover))
                        .on_click(click)
                        .child(Icon::new(IconName::FileCode).xsmall().text_color(theme.fg_muted()))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(if selected { theme.accent_foreground } else { theme.foreground })
                                .child(rel.clone()),
                        ),
                );
            }
            count = self.quick.len();
            if count == 0 {
                list = list.child(div().p_3().text_size(px(12.)).text_color(theme.fg_muted()).child(
                    if self.project.is_some() {
                        "No matching file (the project index may still be building)"
                    } else {
                        "Open a project first"
                    },
                ));
            }
        } else {
            let entries = self.filtered_palette();
            count = entries.len();
            for (index, entry) in entries.iter().enumerate() {
                let selected = index == self.palette_index;
                let cmd = entry.cmd;
                let click = cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                    this.close_palette(window, cx);
                    this.run(cmd, window, cx);
                });
                list = list.child(
                    div()
                        .id(ElementId::Name(format!("palette-item-{index}").into()))
                        .h(px(28.))
                        .px_3()
                        .mx_1()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_6()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .when(selected, |this| this.bg(theme.list_active))
                        .hover(move |this| this.bg(theme.list_hover))
                        .on_click(click)
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(if selected { theme.accent_foreground } else { theme.foreground })
                                .child(entry.label),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.fg_muted())
                                .child(entry.shortcut.unwrap_or_default()),
                        ),
                );
            }
            // Plugin commands follow the workbench's own, as IDEA lists them in
            // one palette: `pluginId:commandId`, titled from the manifest.
            let plugin_commands = self.plugin_command_list();
            for (offset, (plugin_id, command_id, title)) in plugin_commands.iter().enumerate() {
                let index = entries.len() + offset;
                let selected = index == self.palette_index;
                let plugin_id = plugin_id.clone();
                let command_id = command_id.clone();
                let click = cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                    this.close_palette(window, cx);
                    this.run_plugin_command(plugin_id.clone(), command_id.clone(), None, cx);
                });
                list = list.child(
                    div()
                        .id(ElementId::Name(format!("palette-plugin-{offset}").into()))
                        .h(px(28.))
                        .px_3()
                        .mx_1()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_6()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .when(selected, |this| this.bg(theme.list_active))
                        .hover(move |this| this.bg(theme.list_hover))
                        .on_click(click)
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(if selected { theme.accent_foreground } else { theme.foreground })
                                .child(title.clone()),
                        )
                        .child(div().text_size(px(11.)).text_color(theme.fg_muted()).child("plugin")),
                );
            }
            if entries.is_empty() && plugin_commands.is_empty() {
                list = list
                    .child(div().p_3().text_size(px(12.)).text_color(theme.fg_muted()).child("No matching command"));
            }
        }

        let query = if self.palette_query.is_empty() {
            div().text_size(px(15.)).text_color(theme.fg_muted()).child(if files_mode {
                "Go to file…"
            } else {
                "Type a command…"
            })
        } else {
            div().text_size(px(15.)).text_color(theme.foreground).child(self.palette_query.clone())
        };

        let backdrop_close = cx.listener(|this: &mut Self, _: &MouseDownEvent, window, cx| {
            this.close_palette(window, cx);
        });

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(theme.overlay)
                    .on_mouse_down(MouseButton::Left, backdrop_close),
            )
            .child(
                div()
                    .id("command-palette")
                    .track_focus(&self.palette_focus)
                    .key_context("CommandPalette")
                    .on_key_down(cx.listener(Self::palette_key))
                    .mt(px(64.))
                    .w(px(560.))
                    .bg(theme.popover)
                    .border_1()
                    .border_color(theme.border_strong())
                    .rounded(px(8.))
                    .shadow_xl()
                    .overflow_hidden()
                    .child(div().p_3().border_b_1().border_color(theme.border).child(query))
                    .child(div().max_h(px(380.)).child(list))
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .border_t_1()
                            .border_color(theme.border)
                            .text_size(px(10.5))
                            .text_color(theme.fg_muted())
                            .child(format!(
                                "{count} {} · ↑↓ select · Enter {} · Esc close",
                                if files_mode { "files" } else { "commands" },
                                if files_mode { "open" } else { "run" },
                            )),
                    ),
            )
            .into_any_element()
    }
}

impl Focusable for Workbench {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Persist the window geometry as the user resizes or moves it. Only a
        // change touches the disk, so an idle frame is free.
        let bounds = window.bounds();
        let next = crate::layout::WindowState {
            x: bounds.origin.x.as_f32(),
            y: bounds.origin.y.as_f32(),
            width: bounds.size.width.as_f32(),
            height: bounds.size.height.as_f32(),
            maximized: self.window_state.maximized,
        };
        let changed = (next.x - self.window_state.x).abs() > 1.
            || (next.y - self.window_state.y).abs() > 1.
            || (next.width - self.window_state.width).abs() > 1.
            || (next.height - self.window_state.height).abs() > 1.;
        if changed {
            self.window_state = next;
            self.window_state.save(&self.config_dir);
        }
        if self.goto_submit {
            self.goto_submit = false;
            let text = self.goto_input.read(cx).value().trim().to_owned();
            match parse_goto(&text) {
                Some((line, col)) => {
                    self.editor.update(cx, |editor, cx| editor.goto_line(line, col, window, cx));
                    self.status = format!("Line {}", line + 1).into();
                    self.goto_open = false;
                }
                None => self.status = "Go to Line: enter a line number".into(),
            }
            cx.notify();
        }
        if self.find_entered {
            self.find_entered = false;
            self.find_step(1, window, cx);
        }
        if self.find_highlight_dirty {
            self.find_highlight_dirty = false;
            let query = if self.find_open { self.find_input.read(cx).value().trim().to_owned() } else { String::new() };
            let options = self.find_options;
            self.editor.update(cx, |editor, cx| editor.highlight_find(&query, options, cx));
        }
        let theme = cx.theme().clone();
        let menu_bar = self.render_menu_bar(&theme, cx);
        let stripe = self.render_stripe(&theme, cx);
        // These read `self` through entity handles; compute them before the
        // central split so the borrows stay disjoint.
        let tree = self.tree.clone();
        let search = self.search.clone();
        let vcs = self.vcs.clone();
        let editor = self.editor.clone();
        let left_content: AnyElement = match self.left {
            LeftPanel::Project => tree.into_any_element(),
            LeftPanel::Search => search.into_any_element(),
            LeftPanel::Vcs => vcs.into_any_element(),
        };
        let show_project = self.show_project;
        let show_bottom = self.show_bottom;
        let has_project = self.project.is_some();
        let welcome = if has_project { None } else { Some(self.render_welcome(&theme, cx)) };
        let left_width = self.layout.left_width;
        let center_width =
            (window.bounds().size.width.as_f32() - 40.0 - if show_project { left_width + 4.0 } else { 0.0 }).max(200.);
        let bottom_height = if show_bottom { self.layout.heights[self.bottom.index()] } else { 0.0 };
        self.terminal_viewport.set((center_width, bottom_height));
        let left_splitter_hover = theme.accent;
        let bottom_splitter_hover = theme.accent;
        let bottom = self.render_bottom(&theme, cx);
        let status_bar = self.render_status_bar(&theme, cx);
        let mut overlays = Vec::new();
        if let Some(bounds) = self.float_panel {
            let float_hover = theme.list_hover;
            let dock = cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| this.dock_bottom(window, cx));
            let drag = cx.listener(|this: &mut Self, event: &MouseDownEvent, _window, cx| {
                if let Some(panel) = this.float_panel {
                    this.float_drag = Some(FloatDrag {
                        start_x: event.position.x.as_f32(),
                        start_y: event.position.y.as_f32(),
                        panel_x: panel.x,
                        panel_y: panel.y,
                        preview: None,
                    });
                }
                cx.notify();
            });
            overlays.push(
                div()
                    .absolute()
                    .left(px(bounds.x))
                    .top(px(bounds.y))
                    .w(px(bounds.w))
                    .h(px(bounds.h))
                    .flex()
                    .flex_col()
                    .bg(theme.background)
                    // A docked card squares off against its edge; a floating
                    // one keeps the card look.
                    .when(bounds.edge.is_none(), |this| {
                        this.border_1()
                            .border_color(theme.border_strong())
                            .rounded(px(6.))
                            .shadow_xl()
                    })
                    .when_some(bounds.edge, |this, edge| {
                        this.border_color(theme.border_strong()).when(edge == DockEdge::Left, |this| this.border_r_1())
                            .when(edge == DockEdge::Right, |this| this.border_l_1())
                            .when(edge == DockEdge::Bottom, |this| this.border_t_1())
                    })
                    .occlude()
                    .child(
                        div()
                            .id("float-header")
                            .h(px(26.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .bg(theme.muted)
                            .border_b_1()
                            .border_color(theme.border)
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, drag)
                            .child(Icon::new(IconName::PanelsLeftBottom).xsmall().text_color(theme.info))
                            .child(div().text_size(px(11.)).text_color(theme.foreground).child(self.bottom.label()))
                            .child(div().flex_1())
                            .child(
                                div()
                                    .id("float-dock")
                                    .h(px(18.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .rounded(px(3.))
                                    .cursor_pointer()
                                    .border_1()
                                    .border_color(theme.border)
                                    .hover(move |this| this.bg(float_hover))
                                    .on_click(dock)
                                    .text_size(px(9.5))
                                    .text_color(theme.fg_secondary())
                                    .child(if bounds.edge.is_some() { "Undock" } else { "Dock" }),
                            )
                            .when_some(bounds.edge, |this, edge| {
                                this.child(
                                    div()
                                        .text_size(px(9.5))
                                        .text_color(theme.fg_muted())
                                        .child(format!("{} edge", edge.label())),
                                )
                            }),
                    )
                    .child(div().flex_1().min_h_0().child(self.render_bottom_tab()))
                    .into_any_element(),
            );
        }
        if let Some(kind) = self.open_menu {
            let dismiss = cx.listener(|this: &mut Self, _: &MouseDownEvent, _window, cx| {
                this.open_menu = None;
                cx.notify();
            });
            overlays.push(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(hsla(0., 0., 0., 0.))
                    .on_mouse_down(MouseButton::Left, dismiss)
                    .into_any_element(),
            );
            overlays.push(self.render_menu_dropdown(kind, &theme, cx));
        }
        if self.palette_open {
            overlays.push(self.render_palette(&theme, cx));
        }
        if self.keymap_dialog.read(cx).is_open() {
            let origin = dialog_origin(window.bounds().size);
            overlays.push(
                div()
                    .absolute()
                    .left(origin.x)
                    .top(origin.y)
                    .child(self.keymap_dialog.clone())
                    .into_any_element(),
            );
        }
        if self.settings_open {
            let list_hover = theme.list_hover;
            let settings = match &self.project {
                Some(project) => project.settings.effective(),
                None => self.host.global_settings.effective(),
            };
            let file = match &self.project {
                Some(project) => project.settings.file(SettingsScope::Global),
                None => self.host.global_settings.file(SettingsScope::Global),
            }
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "settings.toml".to_owned());
            let backdrop = cx.listener(|this: &mut Self, _: &MouseDownEvent, _window, cx| {
                this.settings_open = false;
                cx.notify();
            });
            let editor_minus =
                cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.change_editor_font_size(-1., cx));
            let editor_plus =
                cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.change_editor_font_size(1., cx));
            let terminal_minus =
                cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.change_terminal_font_size(-1., cx));
            let terminal_plus =
                cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.change_terminal_font_size(1., cx));
            let appearance = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                this.settings_open = false;
                this.appearance_open = true;
                cx.notify();
            });
            let close = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                this.settings_open = false;
                cx.notify();
            });
            let info = |label: &'static str, value: String| {
                div()
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(170.)).text_size(px(12.)).text_color(theme.fg_secondary()).child(label))
                    .child(div().text_size(px(12.)).text_color(theme.foreground).child(value))
            };
            let stepper = |label: &'static str,
                           minus: Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
                           plus: Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
                           value: f32,
                           minus_id: &'static str,
                           plus_id: &'static str| {
                div()
                    .h(px(30.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(170.)).text_size(px(12.)).text_color(theme.fg_secondary()).child(label))
                    .child(
                        div()
                            .id(minus_id)
                            .size(px(22.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .border_1()
                            .border_color(theme.border)
                            .hover(move |this| this.bg(list_hover))
                            .on_click(minus)
                            .child("−"),
                    )
                    .child(
                        div()
                            .w(px(42.))
                            .text_center()
                            .text_size(px(12.5))
                            .text_color(theme.foreground)
                            .child(format!("{value:.0}")),
                    )
                    .child(
                        div()
                            .id(plus_id)
                            .size(px(22.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .border_1()
                            .border_color(theme.border)
                            .hover(move |this| this.bg(list_hover))
                            .on_click(plus)
                            .child("+"),
                    )
            };
            let soft = settings.editor.soft_wrap;
            let insert_spaces = settings.editor.insert_spaces;
            let format_on_save = settings.editor.format_on_save;
            let copy_on_selection = settings.terminal.copy_on_selection;
            let soft_click = cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                this.persist_setting("editor.softWrap", serde_json::json!(!soft));
                this.editor.update(cx, |editor, cx| editor.set_soft_wrap_default(!soft, window, cx));
                cx.notify();
            });
            let insert_click = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                this.persist_setting("editor.insertSpaces", serde_json::json!(!insert_spaces));
                cx.notify();
            });
            let format_click = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                this.persist_setting("editor.formatOnSave", serde_json::json!(!format_on_save));
                cx.notify();
            });
            let copy_click = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                this.persist_setting("terminal.copyOnSelection", serde_json::json!(!copy_on_selection));
                cx.notify();
            });
            let tab_minus = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.change_tab_size(-1, cx));
            let tab_plus = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.change_tab_size(1, cx));
            let header = |label: &'static str| {
                div()
                    .pb_1()
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.fg_muted())
                    .child(label)
            };
            let toggle = |label: &'static str,
                          on: bool,
                          id: &'static str,
                          click: Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>| {
                div()
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(190.)).text_size(px(12.)).text_color(theme.fg_secondary()).child(label))
                    .child(
                        div()
                            .id(id)
                            .h(px(20.))
                            .min_w(px(44.))
                            .px_2()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(10.))
                            .cursor_pointer()
                            .when(on, |this| this.bg(theme.primary).text_color(theme.primary_foreground))
                            .when(!on, |this| this.bg(theme.muted).text_color(theme.fg_muted()))
                            .hover(move |this| this.opacity(0.9))
                            .on_click(click)
                            .text_size(px(9.5))
                            .child(if on { "On" } else { "Off" }),
                    )
            };
            let mut sidecar = div().w(px(150.)).flex_shrink_0().flex().flex_col().gap_1();
            for (section, label) in [
                (SettingsSection::Editor, "Editor"),
                (SettingsSection::Terminal, "Terminal"),
                (SettingsSection::Keymap, "Keymap"),
                (SettingsSection::Appearance, "Appearance"),
                (SettingsSection::General, "General"),
            ] {
                let selected = self.settings_section == section;
                let hover = list_hover;
                let pick = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                    this.settings_section = section;
                    cx.notify();
                });
                sidecar = sidecar.child(
                    div()
                        .id(ElementId::Name(format!("settings-section-{label}").into()))
                        .h(px(26.))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .text_size(px(12.))
                        .when(selected, |this| this.bg(theme.list_active).text_color(theme.accent_foreground))
                        .when(!selected, |this| this.text_color(theme.fg_secondary()).hover(move |this| this.bg(hover)))
                        .on_click(pick)
                        .child(label),
                );
            }
            let mut pane = div().flex_1().min_w_0().flex().flex_col().gap_1();
            match self.settings_section {
                SettingsSection::Editor => {
                    pane = pane
                        .child(header("Editor"))
                        .child(stepper(
                            "Font size",
                            Box::new(editor_minus),
                            Box::new(editor_plus),
                            self.editor_font_size,
                            "settings-editor-minus",
                            "settings-editor-plus",
                        ))
                        .child(stepper(
                            "Tab size",
                            Box::new(tab_minus),
                            Box::new(tab_plus),
                            settings.editor.tab_size as f32,
                            "settings-tab-minus",
                            "settings-tab-plus",
                        ))
                        .child(toggle("Soft wrap", soft, "settings-soft-wrap", Box::new(soft_click)))
                        .child(toggle("Insert spaces", insert_spaces, "settings-insert-spaces", Box::new(insert_click)))
                        .child(toggle(
                            "Format on save",
                            format_on_save,
                            "settings-format-save",
                            Box::new(format_click),
                        ));
                }
                SettingsSection::Terminal => {
                    pane = pane
                        .child(header("Terminal"))
                        .child(stepper(
                            "Font size",
                            Box::new(terminal_minus),
                            Box::new(terminal_plus),
                            self.terminal_font_size,
                            "settings-terminal-minus",
                            "settings-terminal-plus",
                        ))
                        .child(toggle(
                            "Copy on selection",
                            copy_on_selection,
                            "settings-copy-selection",
                            Box::new(copy_click),
                        ))
                        .child(info(
                            "Shell",
                            if settings.terminal.shell_path.trim().is_empty() {
                                "Login shell".to_owned()
                            } else {
                                settings.terminal.shell_path.clone()
                            },
                        ));
                }
                SettingsSection::Appearance => {
                    pane = pane
                        .child(header("Appearance"))
                        .child(
                            div()
                                .h(px(30.))
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(190.))
                                        .text_size(px(12.))
                                        .text_color(theme.fg_secondary())
                                        .child("Theme"),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .text_size(px(12.))
                                        .text_color(theme.foreground)
                                        .child(self.theme.label()),
                                )
                                .child(
                                    div()
                                        .id("settings-theme")
                                        .px_2()
                                        .py(px(2.))
                                        .rounded(px(4.))
                                        .cursor_pointer()
                                        .border_1()
                                        .border_color(theme.border)
                                        .hover(move |this| this.bg(list_hover))
                                        .on_click(appearance)
                                        .text_size(px(11.))
                                        .text_color(theme.link)
                                        .child("Change…"),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(theme.fg_muted())
                                .child("Seven built-in schemes from src/styles/tokens.css."),
                        );
                }
                SettingsSection::Keymap => {
                    let overrides = settings.keymap_overrides.clone();
                    let rows = crate::keymap::rows(cx, &overrides);
                    pane = pane
                        .child(header("Keymap"))
                        .child(info("Preset", settings.keymap.clone()))
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(theme.fg_muted())
                                .child("Click a shortcut to change it; blank removes the binding"),
                        );
                    let mut list = div()
                        .id("keymap-list")
                        .max_h(px(320.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_0p5();
                    for row in rows {
                        let id = row.id;
                        let label = row.label;
                        let current = row.current.clone();
                        let overridden = row.overridden;
                        let value = row.current.unwrap_or_else(|| "unbound".to_owned());
                        list = list.child(
                            div()
                                .id(ElementId::Name(format!("keymap-row-{id}").into()))
                                .h(px(22.))
                                .px_1()
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded(px(3.))
                                .cursor_pointer()
                                .hover(move |this| this.bg(list_hover))
                                .on_click(cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                                    this.open_keymap_dialog(id, label.into(), current.clone(), window, cx);
                                }))
                                .child(
                                    div()
                                        .w(px(230.))
                                        .truncate()
                                        .text_size(px(11.5))
                                        .text_color(theme.foreground)
                                        .child(label),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .text_size(px(10.5))
                                        .text_color(if overridden { theme.warning } else { theme.fg_muted() })
                                        .child(value),
                                ),
                        );
                    }
                    pane = pane.child(list);
                }
                SettingsSection::General => {
                    pane = pane
                        .child(header("General"))
                        .child(info("Settings file", file.clone()))
                        .child(info("Config directory", self.config_dir.display().to_string()))
                        .child(info("Data directory", self.data_dir.display().to_string()))
                        .child(info("Version", env!("CARGO_PKG_VERSION").to_owned()));
                }
            }
            let card = div()
                .w(px(760.))
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .bg(theme.popover)
                .border_1()
                .border_color(theme.border_strong())
                .rounded(px(8.))
                .shadow_xl()
                .occlude()
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child("Settings"),
                )
                .child(div().text_size(px(10.5)).text_color(theme.fg_muted()).child(format!("User settings · {file}")))
                .child(div().flex().gap_4().child(sidecar).child(pane))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(theme.fg_muted())
                                .child("Changes are written to the settings file immediately"),
                        )
                        .child(
                            div()
                                .id("settings-done")
                                .px_3()
                                .py_1()
                                .rounded(px(4.))
                                .cursor_pointer()
                                .bg(theme.primary)
                                .hover(move |this| this.opacity(0.9))
                                .on_click(close)
                                .text_size(px(11.))
                                .text_color(theme.primary_foreground)
                                .child("Done"),
                        ),
                );
            overlays.push(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("settings-backdrop")
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .bg(theme.overlay)
                            .on_mouse_down(MouseButton::Left, backdrop),
                    )
                    .child(card)
                    .into_any_element(),
            );
        }
        if self.breakpoints_open {
            let entries = self.breakpoints.all();
            let backdrop = cx.listener(|this: &mut Self, _: &MouseDownEvent, _window, cx| {
                this.breakpoints_open = false;
                cx.notify();
            });
            let selected_hint = match &self.selected_breakpoint {
                Some((path, line)) => {
                    let relative = path
                        .strip_prefix(self.project.as_ref().map(|project| &project.root).unwrap_or(path))
                        .unwrap_or(path);
                    format!("Selected {}:{} · Enter applies, empty clears", relative.display(), line + 1)
                }
                None => "Select a breakpoint to edit its condition".to_owned(),
            };
            let mut card = div()
                .w(px(540.))
                .p_4()
                .flex()
                .flex_col()
                .gap_1()
                .bg(theme.popover)
                .border_1()
                .border_color(theme.border_strong())
                .rounded(px(8.))
                .shadow_xl()
                .occlude()
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child("Breakpoints"),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.fg_muted())
                        .child("Ctrl+F8 toggles a breakpoint on the editor's caret line"),
                );
            let mut list = div().id("breakpoints-list").flex().flex_col().max_h(px(250.)).min_h_0().overflow_y_scroll();
            if entries.is_empty() {
                list = list
                    .child(div().py_4().text_size(px(12.)).text_color(theme.fg_muted()).child("No breakpoints set"));
            }
            let bp_hover = theme.list_hover;
            let selection = self.selected_breakpoint.clone();
            for (path, entry) in entries {
                let line = entry.line;
                let enabled = entry.enabled;
                let condition = entry.condition.clone();
                let selected = selection
                    .as_ref()
                    .is_some_and(|(selected_path, selected_line)| selected_path == &path && *selected_line == line);
                let select_path = path.clone();
                let toggle_path = path.clone();
                let open_path = path.clone();
                let remove_path = path.clone();
                let select = cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                    this.selected_breakpoint = Some((select_path.clone(), line));
                    let condition = this
                        .breakpoints
                        .find(&select_path, line)
                        .and_then(|entry| entry.condition.clone())
                        .unwrap_or_default();
                    this.condition_input
                        .update(cx, |state, cx| state.set_value(InputContent::new(condition), window, cx));
                    let log = this
                        .breakpoints
                        .find(&select_path, line)
                        .and_then(|entry| entry.log_message.clone())
                        .unwrap_or_default();
                    this.log_input.update(cx, |state, cx| state.set_value(InputContent::new(log), window, cx));
                    cx.notify();
                });
                let toggle = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                    let enabled = this.breakpoints.find(&toggle_path, line).map(|entry| !entry.enabled).unwrap_or(true);
                    this.breakpoints.set_enabled(&toggle_path, line, enabled);
                    this.breakpoints.save(&this.config_dir);
                    let map = this.breakpoints.map();
                    this.editor.update(cx, |editor, cx| editor.set_breakpoints(map, cx));
                    this.sync_breakpoints(toggle_path.clone());
                    cx.notify();
                });
                let open = cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                    this.breakpoints_open = false;
                    this.editor.update(cx, |editor, cx| editor.open_file_at(open_path.clone(), line, 0, window, cx));
                    cx.notify();
                });
                let remove = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                    this.breakpoints.remove(&remove_path, line);
                    if this.selected_breakpoint.as_ref().is_some_and(|(selected_path, selected_line)| {
                        selected_path == &remove_path && *selected_line == line
                    }) {
                        this.selected_breakpoint = None;
                    }
                    this.breakpoints.save(&this.config_dir);
                    let map = this.breakpoints.map();
                    this.editor.update(cx, |editor, cx| editor.set_breakpoints(map, cx));
                    this.sync_breakpoints(remove_path.clone());
                    this.status = format!("Breakpoint removed · {} left", this.breakpoints.count()).into();
                    cx.notify();
                });
                let relative = path
                    .strip_prefix(self.project.as_ref().map(|project| &project.root).unwrap_or(&path))
                    .unwrap_or(&path);
                let label = match (condition.as_deref(), entry.log_message.as_deref()) {
                    (Some(condition), _) => format!("{}:{}  if {condition}", relative.display(), line + 1),
                    (None, Some(message)) => format!("{}:{}  log {message}", relative.display(), line + 1),
                    (None, None) => format!("{}:{}", relative.display(), line + 1),
                };
                list = list.child(
                    div()
                        .h(px(26.))
                        .w_full()
                        .px_1()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(px(4.))
                        .when(selected, |this| this.bg(theme.list_active))
                        .hover(move |this| this.bg(bp_hover))
                        .child(
                            div()
                                .id(ElementId::Name(format!("bp-toggle-{}-{line}", relative.display()).into()))
                                .w(px(30.))
                                .text_center()
                                .text_size(px(10.))
                                .cursor_pointer()
                                .text_color(if enabled { theme.success } else { theme.fg_muted() })
                                .on_click(toggle)
                                .child(if enabled { "On" } else { "Off" }),
                        )
                        .child(
                            div()
                                .id(ElementId::Name(format!("bp-select-{}-{line}", relative.display()).into()))
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .cursor_pointer()
                                .text_size(px(11.5))
                                .text_color(if enabled { theme.foreground } else { theme.fg_muted() })
                                .on_click(select)
                                .child(label),
                        )
                        .child(
                            div()
                                .id(ElementId::Name(format!("bp-open-{}-{line}", relative.display()).into()))
                                .px_1()
                                .text_size(px(10.))
                                .cursor_pointer()
                                .text_color(theme.link)
                                .on_click(open)
                                .child("Open"),
                        )
                        .child(
                            div()
                                .id(ElementId::Name(format!("bp-remove-{}-{line}", relative.display()).into()))
                                .px_1()
                                .text_size(px(11.))
                                .cursor_pointer()
                                .text_color(theme.danger)
                                .on_click(remove)
                                .child("✕"),
                        ),
                );
            }
            card = card
                .child(list)
                .child(div().h(px(6.)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().w(px(70.)).text_size(px(11.)).text_color(theme.fg_muted()).child("Condition"))
                        .child(div().flex_1().min_w_0().child(Input::new(&self.condition_input))),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().w(px(70.)).text_size(px(11.)).text_color(theme.fg_muted()).child("Log message"))
                        .child(div().flex_1().min_w_0().child(Input::new(&self.log_input))),
                )
                .child(div().text_size(px(10.)).text_color(theme.fg_muted()).child(selected_hint));
            overlays.push(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("breakpoints-backdrop")
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .bg(theme.overlay)
                            .on_mouse_down(MouseButton::Left, backdrop),
                    )
                    .child(card)
                    .into_any_element(),
            );
        }
        if self.appearance_open {
            let backdrop = cx.listener(|this: &mut Self, _: &MouseDownEvent, _window, cx| {
                this.appearance_open = false;
                cx.notify();
            });
            let mut card = div()
                .w(px(360.))
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .bg(theme.popover)
                .border_1()
                .border_color(theme.border_strong())
                .rounded(px(8.))
                .shadow_xl()
                .occlude()
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child("Appearance"),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.fg_muted())
                        .child("Built-in themes from src/styles/tokens.css"),
                );
            let theme_hover = theme.list_hover;
            for choice in theme::ThemeChoice::all() {
                let selected = self.theme == choice;
                let click = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                    this.appearance_open = false;
                    this.set_theme(choice, cx);
                });
                card = card.child(
                    div()
                        .id(ElementId::Name(format!("theme-{}", choice.id()).into()))
                        .h(px(30.))
                        .px_3()
                        .flex()
                        .items_center()
                        .justify_between()
                        .rounded(px(5.))
                        .cursor_pointer()
                        .when(selected, |this| this.bg(theme.list_active))
                        .hover(move |this| this.bg(theme_hover))
                        .on_click(click)
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(if selected { theme.accent_foreground } else { theme.foreground })
                                .child(choice.label()),
                        )
                        .when(selected, |this| {
                            this.child(Icon::new(IconName::Check).xsmall().text_color(theme.accent_foreground))
                        }),
                );
            }
            overlays.push(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("appearance-backdrop")
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .bg(theme.overlay)
                            .on_mouse_down(MouseButton::Left, backdrop),
                    )
                    .child(card)
                    .into_any_element(),
            );
        }
        if self.recent_open {
            let backdrop = cx.listener(|this: &mut Self, _: &MouseDownEvent, _window, cx| {
                this.recent_open = false;
                cx.notify();
            });
            let browse = cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| {
                this.recent_open = false;
                this.pick_project(window, cx);
            });
            let recent_hover = theme.list_hover;
            let mut card = div()
                .w(px(520.))
                .p_4()
                .flex()
                .flex_col()
                .gap_1()
                .bg(theme.popover)
                .border_1()
                .border_color(theme.border_strong())
                .rounded(px(8.))
                .shadow_xl()
                .occlude()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(16.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.foreground)
                                .child("Recent Projects"),
                        )
                        .child(div().text_size(px(10.)).text_color(theme.fg_muted()).child("Ctrl+O to browse")),
                );
            let items = self.recent.list();
            if items.is_empty() {
                card = card.child(
                    div().py_4().text_size(px(11.5)).text_color(theme.fg_muted()).child("No recent projects yet"),
                );
            }
            for item in items.into_iter().take(10) {
                let path = item.path.clone().into_path_buf();
                let name = item.name.clone();
                let display = path.display().to_string();
                let active = self.project.as_ref().is_some_and(|project| project.root == path);
                let click = cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                    this.recent_open = false;
                    this.open_project(path.clone(), window, cx);
                });
                card = card.child(
                    div()
                        .id(ElementId::Name(format!("recent-{display}").into()))
                        .h(px(38.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .when(active, |this| this.bg(theme.list_active))
                        .hover(move |this| this.bg(recent_hover))
                        .on_click(click)
                        .child(Icon::new(IconName::Folder).xsmall().text_color(theme.info))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(12.))
                                        .text_color(if active { theme.fg_strong() } else { theme.foreground })
                                        .child(name),
                                )
                                .child(div().truncate().text_size(px(9.5)).text_color(theme.fg_muted()).child(display)),
                        )
                        .when_some(item.branch.clone(), |this, branch| {
                            this.child(div().text_size(px(9.)).text_color(theme.fg_muted()).child(branch))
                        }),
                );
            }
            card = card.child(
                div().pt_2().flex().gap_2().child(div().flex_1()).child(
                    div()
                        .id("recent-browse")
                        .h(px(24.))
                        .px_3()
                        .flex()
                        .items_center()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .border_1()
                        .border_color(theme.border)
                        .hover(move |this| this.bg(recent_hover))
                        .on_click(browse)
                        .text_size(px(11.))
                        .text_color(theme.fg_secondary())
                        .child("Browse…"),
                ),
            );
            overlays.push(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("recent-backdrop")
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .bg(theme.overlay)
                            .on_mouse_down(MouseButton::Left, backdrop),
                    )
                    .child(card)
                    .into_any_element(),
            );
        }
        if self.goto_open {
            let goto_hover = theme.list_hover;
            let backdrop = cx.listener(|this: &mut Self, _: &MouseDownEvent, _window, cx| {
                this.goto_open = false;
                cx.notify();
            });
            let cancel = cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| {
                this.goto_open = false;
                this.editor.update(cx, |editor, cx| editor.focus_current(window, cx));
                cx.notify();
            });
            let card = div()
                .w(px(380.))
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .bg(theme.popover)
                .border_1()
                .border_color(theme.border_strong())
                .rounded(px(8.))
                .shadow_xl()
                .occlude()
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child("Go to Line"),
                )
                .child(Input::new(&self.goto_input))
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(theme.fg_muted())
                        .child("Use line or line:column (1-based), then Enter."),
                )
                .child(
                    div().flex().gap_2().justify_end().child(
                        div()
                            .id("goto-cancel")
                            .h(px(24.))
                            .px_3()
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .border_1()
                            .border_color(theme.border)
                            .hover(move |this| this.bg(goto_hover))
                            .on_click(cancel)
                            .text_size(px(11.))
                            .text_color(theme.fg_secondary())
                            .child("Cancel"),
                    ),
                );
            overlays.push(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("goto-backdrop")
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .bg(theme.overlay)
                            .on_mouse_down(MouseButton::Left, backdrop),
                    )
                    .child(card)
                    .into_any_element(),
            );
        }
        if self.find_open {
            let find_hover = theme.list_hover;
            let query = self.find_input.read(cx).value().trim().to_owned();
            let options = self.find_options;
            let count = if query.is_empty() { 0 } else { self.editor.read(cx).find_matches(&query, options).len() };
            let label = if count == 0 {
                "0".to_owned()
            } else if self.find_searched {
                format!("{}/{}", self.find_index + 1, count)
            } else {
                format!("{count}")
            };
            let prev = cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| this.find_step(-1, window, cx));
            let next = cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| this.find_step(1, window, cx));
            let close = cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| {
                this.find_open = false;
                this.find_highlight_dirty = true;
                this.editor.update(cx, |editor, cx| editor.focus_current(window, cx));
                cx.notify();
            });
            overlays.push(
                div()
                    .absolute()
                    .top(px(36.))
                    .right(px(18.))
                    .w(px(470.))
                    .p_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .bg(theme.popover)
                    .border_1()
                    .border_color(theme.border_strong())
                    .rounded(px(6.))
                    .shadow_lg()
                    .occlude()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.find_input)))
                    .child(option_toggle(
                        "find-case",
                        "Aa",
                        "Match case",
                        options.case_sensitive,
                        &theme,
                        find_hover,
                        cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                            this.find_options.case_sensitive = !this.find_options.case_sensitive;
                            this.find_highlight_dirty = true;
                            this.find_searched = false;
                            cx.notify();
                        }),
                    ))
                    .child(option_toggle(
                        "find-regex",
                        ".*",
                        "Regex",
                        options.regex,
                        &theme,
                        find_hover,
                        cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                            this.find_options.regex = !this.find_options.regex;
                            this.find_highlight_dirty = true;
                            this.find_searched = false;
                            cx.notify();
                        }),
                    ))
                    .child(div().text_size(px(10.)).text_color(theme.fg_muted()).child(label))
                    .child(
                        div()
                            .id("find-prev")
                            .h(px(20.))
                            .px_1()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(move |this| this.bg(find_hover))
                            .on_click(prev)
                            .child(Icon::new(IconName::ChevronUp).xsmall().text_color(theme.fg_secondary())),
                    )
                    .child(
                        div()
                            .id("find-next")
                            .h(px(20.))
                            .px_1()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(move |this| this.bg(find_hover))
                            .on_click(next)
                            .child(Icon::new(IconName::ChevronDown).xsmall().text_color(theme.fg_secondary())),
                    )
                    .child(
                        div()
                            .id("find-close")
                            .h(px(20.))
                            .px_1()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(move |this| this.bg(find_hover))
                            .on_click(close)
                            .child(Icon::new(IconName::X).xsmall().text_color(theme.fg_secondary())),
                    )
                    .into_any_element(),
            );
        }
        // Rename: a centred dialog over a dimmed workbench.
        if self.rename_open {
            let origin = dialog_origin(window.bounds().size);
            let dismiss = cx.listener(|this: &mut Self, _: &MouseDownEvent, _window, cx| {
                this.rename_open = false;
                this.rename.update(cx, |dialog, cx| dialog.close(cx));
                cx.notify();
            });
            overlays.push(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(hsla(0., 0., 0., 0.25))
                    .on_mouse_down(MouseButton::Left, dismiss)
                    .into_any_element(),
            );
            overlays.push(
                div()
                    .absolute()
                    .left(origin.x)
                    .top(origin.y)
                    .child(self.rename.clone())
                    .into_any_element(),
            );
        }
        // Context actions: a popup near the caret, above the editor.
        if self.intentions.read(cx).open {
            // Under the editor's title area, near the caret's line: the editor
            // does not expose its caret's window-relative position, so the popup
            // sits where a context menu reads naturally (top-left of the text).
            let center_width = window.bounds().size.width.as_f32();
            let origin = crate::intentions::popup_origin(
                gpui_kit::point(px(if show_project { left_width + 60. } else { 60. }), px(120.)),
                gpui_kit::size(px(center_width), window.bounds().size.height),
            );
            overlays.push(
                div()
                    .absolute()
                    .left(origin.x)
                    .top(origin.y)
                    .child(self.intentions.clone())
                    .into_any_element(),
            );
        }
        // Dock preview: while a card is dragged near a stripe, shade the strip
        // it would snap into.
        if let Some(preview) = self.float_drag.and_then(|drag| drag.preview) {
            let bounds = window.bounds();
            let (x, y, w, h) = match preview {
                DockEdge::Left => (0., 0., 220., bounds.size.height.as_f32()),
                DockEdge::Right => (bounds.size.width.as_f32() - 220., 0., 220., bounds.size.height.as_f32()),
                DockEdge::Bottom => (0., bounds.size.height.as_f32() - 220., bounds.size.width.as_f32(), 220.),
            };
            overlays.push(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(w))
                    .h(px(h))
                    .bg(theme.accent.opacity(0.18))
                    .border_1()
                    .border_color(theme.accent.opacity(0.5))
                    .into_any_element(),
            );
        }
        // Balloons: bottom-right stack, hidden while the Notifications tool
        // window is on screen (IDEA shows either the stack or the list).
        if !self.notifications_visible() {
            let balloons = self.notifications.update(cx, |log, cx| log.render_balloons(&theme, cx));
            if !balloons.is_empty() {
                overlays.push(
                    div()
                        .absolute()
                        .bottom(px(34.))
                        .right(px(16.) )
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(balloons)
                        .into_any_element(),
                );
            }
        }
        if self.plugins_open {
            let backdrop = cx.listener(|this: &mut Self, _: &MouseDownEvent, _window, cx| {
                this.plugins_open = false;
                cx.notify();
            });
            let close = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                this.plugins_open = false;
                cx.notify();
            });
            let plugin_hover = theme.list_hover;
            let install = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                let Some(plugins) = this.plugins.clone() else { return };
                if let Some(dir) = rfd::FileDialog::new().set_title("Install Plugin").pick_folder() {
                    this.plugin_action(
                        move || plugins.install_from_dir(&dir).map(|_| ()).map_err(|err| err.to_string()),
                        cx,
                    );
                }
            });
            let mut list = div().id("plugins-list").flex_1().min_h_0().flex().flex_col().gap_1().overflow_y_scroll();
            if self.plugins_list.is_empty() {
                list = list
                    .child(div().py_6().text_size(px(11.5)).text_color(theme.fg_muted()).child("No plugins installed"));
            }
            for plugin in &self.plugins_list {
                let id = plugin.id.clone();
                let enabled = plugin.enabled;
                let status = match plugin.status {
                    PluginStatus::Active => ("active", theme.success),
                    PluginStatus::Static => ("static", theme.info),
                    PluginStatus::Disabled => ("disabled", theme.fg_muted()),
                    PluginStatus::Failed => ("failed", theme.danger),
                };
                let plugins = self.plugins.clone();
                let toggle_id = id.clone();
                let toggle = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                    if let Some(plugins) = plugins.clone() {
                        let id = toggle_id.clone();
                        let enabled = enabled;
                        this.plugin_action(
                            move || plugins.set_enabled(&id, !enabled).map(|_| ()).map_err(|err| err.to_string()),
                            cx,
                        );
                    }
                });
                let plugins = self.plugins.clone();
                let reload_id = id.clone();
                let reload = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                    if let Some(plugins) = plugins.clone() {
                        let id = reload_id.clone();
                        this.plugin_action(move || plugins.reload(&id).map(|_| ()).map_err(|err| err.to_string()), cx);
                    }
                });
                let plugins = self.plugins.clone();
                let remove_id = id.clone();
                let remove = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                    if let Some(plugins) = plugins.clone() {
                        let id = remove_id.clone();
                        this.plugin_action(move || plugins.uninstall(&id).map_err(|err| err.to_string()), cx);
                    }
                });
                list = list.child(
                    div()
                        .w_full()
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .rounded(px(5.))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.muted)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(12.5))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.foreground)
                                        .child(format!("{} {}", plugin.name, plugin.version)),
                                )
                                .child(
                                    div()
                                        .px_1()
                                        .rounded(px(2.))
                                        .bg(theme.background)
                                        .text_size(px(8.5))
                                        .text_color(status.1)
                                        .child(status.0),
                                )
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .id(ElementId::Name(format!("plugin-toggle-{}", plugin.id).into()))
                                        .h(px(18.))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .rounded(px(3.))
                                        .cursor_pointer()
                                        .border_1()
                                        .border_color(theme.border)
                                        .hover(move |this| this.bg(plugin_hover))
                                        .on_click(toggle)
                                        .text_size(px(9.))
                                        .text_color(theme.fg_secondary())
                                        .child(if enabled { "Disable" } else { "Enable" }),
                                )
                                .child(
                                    div()
                                        .id(ElementId::Name(format!("plugin-reload-{}", plugin.id).into()))
                                        .h(px(18.))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .rounded(px(3.))
                                        .cursor_pointer()
                                        .border_1()
                                        .border_color(theme.border)
                                        .hover(move |this| this.bg(plugin_hover))
                                        .on_click(reload)
                                        .text_size(px(9.))
                                        .text_color(theme.fg_secondary())
                                        .child("Reload"),
                                )
                                .child(
                                    div()
                                        .id(ElementId::Name(format!("plugin-remove-{}", plugin.id).into()))
                                        .h(px(18.))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .rounded(px(3.))
                                        .cursor_pointer()
                                        .border_1()
                                        .border_color(theme.border)
                                        .hover(move |this| this.bg(plugin_hover))
                                        .on_click(remove)
                                        .text_size(px(9.))
                                        .text_color(theme.danger)
                                        .child("Remove"),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(9.5))
                                .text_color(theme.fg_muted())
                                .child(format!("{} · {}", plugin.id, plugin.description)),
                        )
                        .when_some(plugin.error.clone(), |this, error| {
                            this.child(div().text_size(px(9.5)).text_color(theme.danger).child(error))
                        }),
                );
            }
            let plugins_dir = self
                .config_dir
                .parent()
                .map(|_| self.data_dir.join("plugins").display().to_string())
                .unwrap_or_else(|| "plugins".to_owned());
            let card = div()
                .w(px(780.))
                .h(px(520.))
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .bg(theme.popover)
                .border_1()
                .border_color(theme.border_strong())
                .rounded(px(8.))
                .shadow_xl()
                .occlude()
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.foreground)
                        .child("Plugins"),
                )
                .child(div().text_size(px(10.)).text_color(theme.fg_muted()).child(plugins_dir))
                .child(list)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when_some(self.plugins_notice.clone(), |this, notice| {
                            this.child(div().flex_1().text_size(px(10.)).text_color(theme.warning).child(notice))
                        })
                        .when(self.plugins_notice.is_none(), |this| this.child(div().flex_1()))
                        .child(
                            div()
                                .id("plugins-install")
                                .h(px(24.))
                                .px_3()
                                .flex()
                                .items_center()
                                .rounded(px(4.))
                                .cursor_pointer()
                                .border_1()
                                .border_color(theme.border)
                                .hover(move |this| this.bg(plugin_hover))
                                .on_click(install)
                                .text_size(px(10.5))
                                .text_color(theme.fg_secondary())
                                .child("Install from folder…"),
                        )
                        .child(
                            div()
                                .id("plugins-close")
                                .h(px(24.))
                                .px_3()
                                .flex()
                                .items_center()
                                .rounded(px(4.))
                                .cursor_pointer()
                                .bg(theme.primary)
                                .hover(move |this| this.opacity(0.9))
                                .on_click(close)
                                .text_size(px(10.5))
                                .text_color(theme.primary_foreground)
                                .child("Close"),
                        ),
                );
            overlays.push(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("plugins-backdrop")
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .bg(theme.overlay)
                            .on_mouse_down(MouseButton::Left, backdrop),
                    )
                    .child(card)
                    .into_any_element(),
            );
        }
        if self.about_open {
            let close = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                this.about_open = false;
                cx.notify();
            });
            overlays.push(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme.overlay)
                    .child(
                        div()
                            .w(px(420.))
                            .p_5()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .bg(theme.popover)
                            .border_1()
                            .border_color(theme.border_strong())
                            .rounded(px(8.))
                            .shadow_xl()
                            .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(theme.foreground).child("OpenIDEA"))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme.fg_secondary())
                                    .child(format!("v{} — native GPUI shell", env!("CARGO_PKG_VERSION"))),
                            )
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(theme.fg_muted())
                                    .child("Backend: ide-host — the same crates the Tauri build used. Renderer: gpui-kit / gpui-component."),
                            )
                            .child(div().text_size(px(11.)).text_color(theme.fg_muted()).child(format!("Config: {}", self.config_dir.display())))
                            .child(div().text_size(px(11.)).text_color(theme.fg_muted()).child(format!("Data: {}", self.data_dir.display())))
                            .child(
                                Button::new("about-close").label("Close").on_click(close),
                            ),
                    )
                    .into_any_element(),
            );
        }

        div()
            .id("workbench-root")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .key_context(actions::CONTEXT)
            .track_focus(&self.focus_handle)
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(cx.listener(|this, _: &SaveFile, window, cx| this.run(Cmd::Save, window, cx)))
            .on_action(cx.listener(|this, _: &SaveAll, window, cx| this.run(Cmd::SaveAll, window, cx)))
            .on_action(cx.listener(|this, _: &QuickOpen, window, cx| this.run(Cmd::QuickOpen, window, cx)))
            .on_action(cx.listener(|this, _: &FindInPath, window, cx| this.run(Cmd::FindInPath, window, cx)))
            .on_action(cx.listener(|this, _: &ShowProject, window, cx| this.run(Cmd::ShowProject, window, cx)))
            .on_action(cx.listener(|this, _: &ShowSearch, window, cx| this.run(Cmd::ShowSearch, window, cx)))
            .on_action(cx.listener(|this, _: &ShowVcs, window, cx| this.run(Cmd::ShowVcs, window, cx)))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| this.run(Cmd::CloseTab, window, cx)))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.run(Cmd::NextTab, window, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.run(Cmd::PrevTab, window, cx)))
            .on_action(cx.listener(|this, _: &OpenProject, window, cx| this.run(Cmd::OpenProject, window, cx)))
            .on_action(cx.listener(|this, _: &ToggleProject, window, cx| this.run(Cmd::ToggleProject, window, cx)))
            .on_action(cx.listener(|this, _: &ToggleProblems, window, cx| this.run(Cmd::ToggleProblems, window, cx)))
            .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| this.run(Cmd::ToggleTerminal, window, cx)))
            .on_action(cx.listener(|this, _: &CommandPalette, window, cx| this.run(Cmd::CommandPalette, window, cx)))
            .on_action(cx.listener(|this, _: &RefreshProject, window, cx| this.run(Cmd::RefreshProject, window, cx)))
            .on_action(cx.listener(|this, _: &ToggleTheme, _window, cx| this.run(Cmd::ToggleTheme, _window, cx)))
            .on_action(cx.listener(|this, _: &Run, window, cx| this.run(Cmd::Run, window, cx)))
            .on_action(cx.listener(|this, _: &StopRun, window, cx| this.run(Cmd::StopRun, window, cx)))
            .on_action(cx.listener(|this, _: &ShowRun, window, cx| this.run(Cmd::ShowRun, window, cx)))
            .on_action(cx.listener(|this, _: &Debug, window, cx| this.run(Cmd::Debug, window, cx)))
            .on_action(cx.listener(|this, _: &ShowDebug, window, cx| this.run(Cmd::ShowDebug, window, cx)))
            .on_action(cx.listener(|this, _: &ShowDb, window, cx| this.run(Cmd::ShowDb, window, cx)))
            .on_action(cx.listener(|this, _: &ShowHttp, window, cx| this.run(Cmd::ShowHttp, window, cx)))
            .on_action(cx.listener(|this, _: &ShowNotebook, window, cx| this.run(Cmd::ShowNotebook, window, cx)))
            .on_action(cx.listener(|this, _: &ShowAgents, window, cx| this.run(Cmd::ShowAgents, window, cx)))
            .on_action(
                cx.listener(|this, _: &ShowNotifications, window, cx| this.run(Cmd::ShowNotifications, window, cx)),
            )
            .on_action(cx.listener(|this, _: &RunHttpAtCaret, window, cx| this.run(Cmd::RunHttpAtCaret, window, cx)))
            .on_action(cx.listener(|this, _: &StopDebug, window, cx| this.run(Cmd::StopDebug, window, cx)))
            .on_action(cx.listener(|this, _: &ResumeDebug, window, cx| this.run(Cmd::ResumeDebug, window, cx)))
            .on_action(cx.listener(|this, _: &PauseDebug, window, cx| this.run(Cmd::PauseDebug, window, cx)))
            .on_action(cx.listener(|this, _: &StepInto, window, cx| this.run(Cmd::StepInto, window, cx)))
            .on_action(cx.listener(|this, _: &StepOver, window, cx| this.run(Cmd::StepOver, window, cx)))
            .on_action(cx.listener(|this, _: &StepOut, window, cx| this.run(Cmd::StepOut, window, cx)))
            .on_action(
                cx.listener(|this, _: &ToggleBreakpoint, window, cx| this.run(Cmd::ToggleBreakpoint, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ViewBreakpoints, window, cx| this.run(Cmd::ViewBreakpoints, window, cx)))
            .on_action(cx.listener(|this, _: &Settings, window, cx| this.run(Cmd::Settings, window, cx)))
            .on_action(cx.listener(|this, _: &GoToLine, window, cx| this.run(Cmd::GoToLine, window, cx)))
            .on_action(cx.listener(|this, _: &FindInEditor, window, cx| this.run(Cmd::FindInEditor, window, cx)))
            .on_action(cx.listener(|this, _: &Intentions, window, cx| this.run(Cmd::Intentions, window, cx)))
            .on_action(cx.listener(|this, _: &Rename, window, cx| this.run(Cmd::Rename, window, cx)))
            .on_action(cx.listener(|this, _: &ShowDuplicates, window, cx| this.run(Cmd::ShowDuplicates, window, cx)))
            .on_action(
                cx.listener(|this, _: &ShowStructural, window, cx| this.run(Cmd::ShowStructural, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ShowStructure, window, cx| this.run(Cmd::ShowStructure, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowSsh, window, cx| this.run(Cmd::ShowSsh, window, cx)))
            .on_action(
                cx.listener(|this, _: &ShowDeploy, window, cx| this.run(Cmd::ShowDeploy, window, cx)),
            )
            .on_action(cx.listener(|this, _: &Undo, window, cx| this.run(Cmd::Undo, window, cx)))
            .on_action(cx.listener(|this, _: &Redo, window, cx| this.run(Cmd::Redo, window, cx)))
            .on_action(cx.listener(|this, _: &Cut, window, cx| this.run(Cmd::Cut, window, cx)))
            .on_action(cx.listener(|this, _: &Copy, window, cx| this.run(Cmd::Copy, window, cx)))
            .on_action(cx.listener(|this, _: &Paste, window, cx| this.run(Cmd::Paste, window, cx)))
            .on_action(cx.listener(|this, _: &SelectAll, window, cx| this.run(Cmd::SelectAll, window, cx)))
            .on_action(cx.listener(|this, _: &CloseFind, window, cx| {
                if this.find_open {
                    this.find_open = false;
                    this.find_highlight_dirty = true;
                    this.editor.update(cx, |editor, cx| editor.focus_current(window, cx));
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &Quit, _window, cx| this.run(Cmd::Quit, _window, cx)))
            .on_action(cx.listener(|this, _: &About, _window, cx| this.run(Cmd::About, _window, cx)))
            .child(menu_bar)
            .when(self.project.as_ref().is_some_and(|project| !project.is_trusted()), |this| {
                let trust = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.trust_project(cx));
                this.child(
                    div()
                        .h(px(28.))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_3()
                        .bg(theme.warning.opacity(0.12))
                        .border_b_1()
                        .border_color(theme.warning.opacity(0.4))
                        .text_size(px(11.5))
                        .text_color(theme.warning)
                        .child(Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.warning))
                        .child("Restricted Mode: language servers, Git hooks and run configurations are disabled.")
                        .child(div().flex_1())
                        .child(Button::new("trust-project").label("Trust Project").small().on_click(trust)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .child(stripe)
                    .when_some(welcome, |this, welcome| {
                        // Without a project only the Notifications tool window can
                        // be open; it docks under the welcome screen so backend
                        // messages stay reachable before any folder is opened.
                        let show_notifications_drawer = show_bottom
                            && self.bottom == BottomTab::Notifications
                            && self.float_panel.is_none();
                        this.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .flex()
                                .flex_col()
                                .child(welcome)
                                .when(show_notifications_drawer, |this| {
                                    let height = self.layout.heights[BottomTab::Notifications.index()];
                                    this.child(
                                        div()
                                            .h(px(height))
                                            .flex_shrink_0()
                                            .w_full()
                                            .flex()
                                            .flex_col()
                                            .bg(theme.background)
                                            .border_t_1()
                                            .border_color(theme.border)
                                            .child(self.notifications.clone()),
                                    )
                                }),
                        )
                    })
                    .when(has_project, |this| {
                        this.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .flex()
                                .when(show_project, |this| {
                                    this.child(
                                        div()
                                            .w(px(left_width))
                                            .h_full()
                                            .border_r_1()
                                            .border_color(theme.border)
                                            .child(left_content),
                                    )
                                    .child(
                                        div()
                                            .id("left-splitter")
                                            .w(px(4.))
                                            .h_full()
                                            .flex_shrink_0()
                                            .cursor_col_resize()
                                            .bg(theme.border)
                                            .hover(move |this| this.bg(left_splitter_hover))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|this: &mut Self, event: &MouseDownEvent, _window, cx| {
                                                    this.resize_drag = Some(ResizeDrag {
                                                        target: ResizeTarget::Left,
                                                        start: event.position,
                                                        size: this.layout.left_width,
                                                    });
                                                    cx.notify();
                                                }),
                                            ),
                                    )
                                })
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .h_full()
                                        .flex()
                                        .flex_col()
                                        .child(div().flex_1().min_h_0().child(editor))
                                        .when(show_bottom && self.float_panel.is_none(), |this| {
                                            this.child(
                                                div()
                                                    .id("bottom-splitter")
                                                    .h(px(4.))
                                                    .w_full()
                                                    .flex_shrink_0()
                                                    .cursor_row_resize()
                                                    .bg(theme.border)
                                                    .hover(move |this| this.bg(bottom_splitter_hover))
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        cx.listener(
                                                            |this: &mut Self, event: &MouseDownEvent, _window, cx| {
                                                                this.resize_drag = Some(ResizeDrag {
                                                                    target: ResizeTarget::Bottom,
                                                                    start: event.position,
                                                                    size: this.layout.heights[this.bottom.index()],
                                                                });
                                                                cx.notify();
                                                            },
                                                        ),
                                                    ),
                                            )
                                            .child(bottom)
                                        }),
                                ),
                        )
                    }),
            )
            .child(status_bar)
            .on_key_down(cx.listener(|this: &mut Self, event: &KeyDownEvent, window, cx| {
                this.intentions_key(event, window, cx);
            }))
            .on_mouse_move(cx.listener(|this: &mut Self, event: &MouseMoveEvent, _window, cx| {
                if let Some(mut drag) = this.float_drag {
                    let width = _window.bounds().size.width.as_f32();
                    let height = _window.bounds().size.height.as_f32();
                    let cursor_x = event.position.x.as_f32();
                    let cursor_y = event.position.y.as_f32();
                    if let Some(panel) = this.float_panel.as_mut() {
                        // A drag away from a dock pulls the card free again; the
                        // pointer keeps its offset inside the card.
                        if panel.edge.is_some() {
                            let w = 720_f32.min((width - 40.).max(200.));
                            let h = 480_f32.min((height - 80.).max(160.));
                            *panel = FloatPanel {
                                x: (cursor_x - w / 2.).clamp(-200., width),
                                y: (cursor_y - 20.).clamp(-50., height),
                                w,
                                h,
                                edge: None,
                            };
                            drag = FloatDrag {
                                start_x: cursor_x,
                                start_y: cursor_y,
                                panel_x: panel.x,
                                panel_y: panel.y,
                                preview: None,
                            };
                            this.float_drag = Some(drag);
                            cx.notify();
                            return;
                        }
                        let dx = cursor_x - drag.start_x;
                        let dy = cursor_y - drag.start_y;
                        panel.x = (drag.panel_x + dx).clamp(-200., 2000.);
                        panel.y = (drag.panel_y + dy).clamp(-50., 2000.);
                        // Preview only: the card keeps floating until release.
                        drag.preview = snap_edge(panel.x, panel.y, width, height);
                        this.float_drag = Some(drag);
                        cx.notify();
                    }
                    return;
                }
                let Some(drag) = this.resize_drag else { return };
                let dx = event.position.x.as_f32() - drag.start.x.as_f32();
                let dy = event.position.y.as_f32() - drag.start.y.as_f32();
                match drag.target {
                    ResizeTarget::Left => {
                        this.layout.left_width = (drag.size + dx).clamp(160., 600.);
                    }
                    ResizeTarget::Bottom => {
                        let index = this.bottom.index();
                        this.layout.heights[index] = (drag.size - dy).clamp(120., 760.);
                    }
                }
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this: &mut Self, _: &MouseUpEvent, window, cx| {
                    let preview = this.float_drag.take().and_then(|drag| drag.preview);
                    // Release near an edge: the card docks there; anywhere else
                    // it stays floating with its preview cleared.
                    if let Some(panel) = this.float_panel.as_mut() {
                        let width = window.bounds().size.width.as_f32();
                        let height = window.bounds().size.height.as_f32();
                        match preview.or_else(|| snap_edge(panel.x, panel.y, width, height)) {
                            Some(edge) => {
                                *panel = docked_bounds(edge, width, height);
                                this.status = format!("{} docked to the {} edge", this.bottom.label(), edge.label()).into();
                            }
                            None => {
                                panel.edge = None;
                            }
                        }
                        cx.notify();
                    }
                    if this.resize_drag.take().is_some() {
                        this.layout.save(&this.config_dir);
                        this.status = format!(
                            "Layout saved · left {}px · bottom {}px",
                            this.layout.left_width.round() as u32,
                            this.layout.heights[this.bottom.index()].round() as u32
                        )
                        .into();
                        cx.notify();
                    }
                }),
            )
            .children(overlays)
    }
}

/// The identifier under a selection/caret: `active_position`'s byte range when
/// something is selected, otherwise the word the caret sits in.
fn symbol_at(text: &str, range: std::ops::Range<usize>) -> Option<String> {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    if range.start < range.end {
        return text.get(range.clone()).filter(|word| word.chars().all(is_ident)).map(str::to_owned);
    }
    let caret = range.start.min(text.len());
    let bytes = text.as_bytes();
    let mut start = caret;
    while start > 0 && is_ident(bytes[start - 1] as char) {
        start -= 1;
    }
    let mut end = caret;
    while end < bytes.len() && is_ident(bytes[end] as char) {
        end += 1;
    }
    (end > start).then(|| text[start..end].to_owned())
}

#[cfg(test)]
mod symbol_tests {
    use super::symbol_at;

    #[test]
    fn finds_the_word_under_a_caret() {
        let text = "fn alpha() { beta(); }";
        // Caret inside "alpha".
        assert_eq!(symbol_at(text, 6..6).as_deref(), Some("alpha"));
        // Caret at the very start of a word.
        assert_eq!(symbol_at(text, 3..3).as_deref(), Some("alpha"));
        // Caret just past the last letter of a word.
        assert_eq!(symbol_at(text, 7..7).as_deref(), Some("alpha"));
        // Offset 0 is inside the leading "fn".
        assert_eq!(symbol_at(text, 0..0).as_deref(), Some("fn"));
        // At a word boundary the preceding word wins (IDEA behaves the same).
        assert_eq!(symbol_at(text, 2..2).as_deref(), Some("fn"));
        // Right after a word the word still wins.
        assert_eq!(symbol_at(text, 8..8).as_deref(), Some("alpha"));
        // Inside punctuation or a gap between words nothing is a symbol.
        assert_eq!(symbol_at(text, 19..19), None);
        // A caret inside "beta" picks it up.
        assert_eq!(symbol_at(text, 15..15).as_deref(), Some("beta"));
    }

    #[test]
    fn a_selection_is_used_when_it_is_an_identifier() {
        let text = "let value = other;";
        assert_eq!(symbol_at(text, 4..9).as_deref(), Some("value"));
        // A selection spanning punctuation is not a symbol.
        assert_eq!(symbol_at(text, 3..10), None);
    }
}

#[cfg(test)]
mod float_tests {
    use super::{DockEdge, docked_bounds, float_bounds, snap_edge};

    #[test]
    fn float_bounds_center_and_clamp() {
        let panel = float_bounds(1600., 1000.);
        assert_eq!((panel.x, panel.y, panel.w, panel.h), (440., 80., 720., 480.));
        assert_eq!(panel.edge, None);

        let small = float_bounds(600., 300.);
        assert!(small.w <= 560. && small.h <= 220.);
        assert!(small.x >= 10.);
    }

    #[test]
    fn snap_edge_picks_the_nearest_edge() {
        // Nothing near an edge.
        assert_eq!(snap_edge(600., 400., 1200., 800.), None);
        // Within the snap distance of each edge.
        assert_eq!(snap_edge(10., 300., 1200., 800.), Some(DockEdge::Left));
        assert_eq!(snap_edge(1195., 300., 1200., 800.), Some(DockEdge::Right));
        assert_eq!(snap_edge(600., 790., 1200., 800.), Some(DockEdge::Bottom));
        // The bottom wins at the bottom corners: the wide dock is reachable.
        assert_eq!(snap_edge(5., 795., 1200., 800.), Some(DockEdge::Bottom));
    }

    #[test]
    fn docked_bounds_span_the_edge() {
        let left = docked_bounds(DockEdge::Left, 1200., 800.);
        assert_eq!((left.x, left.y, left.h, left.edge), (0., 0., 800., Some(DockEdge::Left)));
        assert!(left.w >= 240. && left.w <= 560. && left.w < 1200.);

        let right = docked_bounds(DockEdge::Right, 1200., 800.);
        assert_eq!(right.x + right.w, 1200.);
        assert_eq!(right.edge, Some(DockEdge::Right));

        let bottom = docked_bounds(DockEdge::Bottom, 1200., 800.);
        assert_eq!((bottom.x, bottom.w, bottom.y + bottom.h), (0., 1200., 800.));
        assert!(bottom.h >= 180. && bottom.h < 800.);
        assert_eq!(bottom.edge, Some(DockEdge::Bottom));
    }

    #[test]
    fn docked_bounds_stay_inside_a_small_window() {
        let left = docked_bounds(DockEdge::Left, 300., 400.);
        assert!(left.w <= 220. && left.x == 0.);
        let bottom = docked_bounds(DockEdge::Bottom, 300., 200.);
        assert!(bottom.h <= 80. && bottom.h > 0.);
    }
}
