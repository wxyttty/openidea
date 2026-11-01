//! The native OpenIDEA workbench, built on the GPUI ecosystem.
//!
//! This crate replaces the React/CodeMirror renderer and the Tauri IPC layer of
//! the old shell: the UI runs inside the process on [`gpui_kit`] (GPUI +
//! `gpui-base` + `gpui-component`) and talks to the backend through the same
//! `ide-host` facade the Tauri commands used, without serialization.
//!
//! Layout of the workbench:
//!
//! - [`workbench`] – window root: menu bar, tool-window stripes, central split,
//!   status bar, command palette and the welcome screen.
//! - [`project_tree`] – lazily loaded project tree over `ide-vfs`.
//! - [`editor`] – tabbed code editor over `gpui-component`'s `EditorState`,
//!   with tree-sitter syntax highlighting and `ide-vfs` load/save.
//! - [`theme`] – the OpenIDEA palettes from `src/styles/tokens.css`, mapped onto
//!   `gpui-component`'s theme.
//! - [`events`] – the `ide-core::EventSink` bridge that feeds backend events
//!   into the GPUI main thread.
//!
//! Migration status and the phase plan live in `docs/gpui-migration.md`.

pub mod actions;
pub mod breakpoints;
pub mod db;
pub mod debug;
pub mod diff;
pub mod editor;
pub mod events;
pub mod http;
pub mod layout;
pub mod lsp;
pub mod notebook;
pub mod problems;
pub mod project_tree;
pub mod run;
pub mod search;
pub mod terminal;
pub mod theme;
pub mod vcs;
pub mod workbench;

pub use workbench::{Workbench, WorkbenchConfig};
