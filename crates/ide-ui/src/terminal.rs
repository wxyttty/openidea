//! The integrated terminal.
//!
//! `ide-terminal` owns the PTY; this panel turns its byte stream into a screen
//! with `vt100` and paints it as a monospace grid, so the terminal is native
//! Rust from the shell to the pixels. Keys go back through `TerminalManager`,
//! and `IdeEvent::TerminalExited` marks the session finished.

use crate::theme::IdeColors as _;
use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_core::{IdeEvent, SharedSink, TerminalId};
use ide_host::Host;
use ide_terminal::TerminalOptions;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::num::NonZeroU16;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const COLS: u16 = 120;
const ROWS: u16 = 16;
const DEFAULT_FONT_SIZE: f32 = 12.;

/// IDEA's terminal ANSI palette (`--terminal-ansi-*` in `tokens.css`).
const ANSI: [u32; 16] = [
    0x000000, 0xf27481, 0x6bcc62, 0xe0ce70, 0x5594fa, 0xc092fa, 0x47ccbd, 0xced0d6, 0x4e5157, 0xff6b7a, 0x67ff59,
    0xffec1a, 0x3399ff, 0xd970ff, 0x40ffe9, 0xffffff,
];

struct Session {
    id: TerminalId,
    title: SharedString,
    parser: Arc<Mutex<vt100::Parser>>,
    exited: Arc<AtomicBool>,
}

pub struct TerminalPanel {
    host: Arc<Host>,
    project: Option<Arc<ide_host::Project>>,
    sessions: Vec<Session>,
    active: usize,
    grid: (u16, u16),
    /// Content area (width, height) of the terminal panel, set by the workbench.
    viewport: Rc<Cell<(f32, f32)>>,
    notice: Option<SharedString>,
    font_size: f32,
    focus_handle: FocusHandle,
}

impl TerminalPanel {
    pub fn new(host: Arc<Host>, viewport: Rc<Cell<(f32, f32)>>, cx: &mut Context<Self>) -> Self {
        Self {
            host,
            project: None,
            sessions: Vec::new(),
            active: 0,
            grid: (COLS, ROWS),
            viewport,
            notice: None,
            font_size: DEFAULT_FONT_SIZE,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        self.font_size = size.clamp(8., 28.);
        cx.notify();
    }

    pub fn set_project(&mut self, project: &Arc<ide_host::Project>, cx: &mut Context<Self>) {
        for session in std::mem::take(&mut self.sessions) {
            let _ = self.host.terminals.kill(session.id);
        }
        self.active = 0;
        self.project = Some(project.clone());
        self.notice = None;
        cx.notify();
    }

    /// Start a shell if the panel has none yet, then focus it.
    pub fn ensure(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sessions.is_empty() {
            self.start(window, cx);
        }
        let handle = self.focus_handle.clone();
        window.focus(&handle, cx);
        cx.notify();
    }

    fn start(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.notice = None;
        let cwd = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        let (cols, rows) = self.grid;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            NonZeroU16::new(rows).unwrap(),
            NonZeroU16::new(cols).unwrap(),
            2000,
        )));
        let exited = Arc::new(AtomicBool::new(false));
        let (tx, mut rx) = unbounded::<()>();
        let parser_writer = parser.clone();
        let exited_writer = exited.clone();
        let sink: SharedSink = Arc::new(|event: IdeEvent| {
            if let IdeEvent::TerminalExited { .. } = event {
                // The workbench also receives this on its window sink.
            }
        });
        let on_output: Box<dyn Fn(String) + Send> = Box::new(move |data| {
            if let Ok(mut parser) = parser_writer.lock() {
                parser.process(data.as_bytes());
            }
            let _ = tx.unbounded_send(());
        });
        let options = TerminalOptions {
            cwd: cwd.clone().into(),
            shell: None,
            args: Vec::new(),
            env: BTreeMap::new(),
            cols,
            rows,
            shell_integration: false,
        };
        match self.host.terminals.spawn(options, sink, on_output) {
            Ok(info) => {
                tracing::debug!(terminal = ?info.id, shell = %info.title, "terminal spawned");
                let title: SharedString = format!("Terminal {}", self.sessions.len() + 1).into();
                self.sessions.push(Session { id: info.id, title, parser, exited: exited_writer });
                self.active = self.sessions.len() - 1;
                cx.spawn(async move |this, cx| {
                    while rx.next().await.is_some() {
                        if this.update(cx, |_, cx| cx.notify()).is_err() {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(err) => {
                self.notice = Some(format!("Cannot start terminal: {err}").into());
                tracing::warn!("terminal spawn failed: {err}");
            }
        }
        cx.notify();
    }

    pub fn mark_exited(&mut self, terminal: TerminalId, code: Option<i32>, cx: &mut Context<Self>) {
        if let Some(session) = self.sessions.iter().find(|session| session.id == terminal) {
            session.exited.store(true, Ordering::Release);
            self.notice = Some(match code {
                Some(code) => format!("Terminal exited with code {code}").into(),
                None => "Terminal exited".into(),
            });
            cx.notify();
        }
    }

    fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sessions.is_empty() {
            self.start(window, cx);
        } else {
            let index = self.active.min(self.sessions.len() - 1);
            let old = self.sessions.remove(index);
            let _ = self.host.terminals.kill(old.id);
            self.active = index.min(self.sessions.len());
            self.start(window, cx);
            if index + 1 < self.sessions.len() {
                if let Some(session) = self.sessions.pop() {
                    self.sessions.insert(index, session);
                }
            }
            self.active = index.min(self.sessions.len().saturating_sub(1));
        }
        let handle = self.focus_handle.clone();
        window.focus(&handle, cx);
        cx.notify();
    }

    fn kill(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get(self.active) else { return };
        let id = session.id;
        let _ = self.host.terminals.kill(id);
        session.exited.store(true, Ordering::Release);
        self.notice = Some("Terminal process killed".into());
        cx.notify();
    }

    fn close(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.sessions.len() {
            return;
        }
        let session = self.sessions.remove(index);
        let _ = self.host.terminals.kill(session.id);
        if self.sessions.is_empty() {
            self.active = 0;
        } else {
            self.active = self.active.min(self.sessions.len() - 1);
        }
        cx.notify();
    }

    /// Resize every session's VT grid and PTY to the panel's cell size.
    fn resize(&mut self, cols: u16, rows: u16) {
        if self.grid == (cols, rows) {
            return;
        }
        self.grid = (cols, rows);
        for session in &self.sessions {
            if let Ok(mut parser) = session.parser.lock() {
                if let (Some(rows), Some(cols)) = (NonZeroU16::new(rows), NonZeroU16::new(cols)) {
                    parser.set_size(rows, cols);
                }
            }
            let _ = self.host.terminals.resize(session.id, cols, rows);
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get(self.active) else { return };
        let id = session.id;
        let keystroke = &event.keystroke;
        let mut bytes = String::new();
        if keystroke.modifiers.control {
            if let Some(character) = keystroke.key.chars().next() {
                let lower = character.to_ascii_lowercase();
                if lower.is_ascii_alphabetic() || matches!(lower, '[' | '\\' | ']' | '^' | '_' | '@' | ' ') {
                    bytes.push(char::from((lower as u8) & 0x1f));
                }
            }
        } else if keystroke.modifiers.alt {
            bytes.push('\x1b');
            if let Some(text) = &keystroke.key_char {
                bytes.push_str(text);
            }
        } else {
            match keystroke.key.as_str() {
                "enter" => bytes.push('\r'),
                "backspace" => bytes.push('\x7f'),
                "tab" => bytes.push('\t'),
                "escape" => bytes.push('\x1b'),
                "up" => bytes.push_str("\x1b[A"),
                "down" => bytes.push_str("\x1b[B"),
                "right" => bytes.push_str("\x1b[C"),
                "left" => bytes.push_str("\x1b[D"),
                "home" => bytes.push_str("\x1b[H"),
                "end" => bytes.push_str("\x1b[F"),
                "delete" => bytes.push_str("\x1b[3~"),
                "pageup" => bytes.push_str("\x1b[5~"),
                "pagedown" => bytes.push_str("\x1b[6~"),
                _ => {
                    if let Some(text) = &keystroke.key_char {
                        bytes.push_str(text);
                    }
                }
            }
        }
        if !bytes.is_empty() {
            if let Err(err) = self.host.terminals.write(id, &bytes) {
                self.notice = Some(format!("Terminal write failed: {err}").into());
            }
            cx.notify();
        }
    }

    fn scroll(&mut self, lines: i32, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get(self.active) else { return };
        let Ok(mut parser) = session.parser.lock() else { return };
        let current = parser.screen().scrollback() as i32;
        let max = parser.screen().scrollback_capacity() as i32;
        let next = (current + lines).clamp(0, max) as usize;
        parser.screen_mut().set_scrollback(next);
        cx.notify();
    }
}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let font_size = self.font_size;
        let row_height = font_size + 5.;
        // The panel sits right of the 340 px tool-window column inside the
        // 366 px bottom panel; derive the grid from the real window width so
        // maximising the window reflows the shell.
        let (viewport_w, viewport_h) = self.viewport.get();
        let fallback = window.bounds().size;
        let (viewport_w, viewport_h) =
            if viewport_w > 0. { (viewport_w, viewport_h) } else { (fallback.width.as_f32() - 340., 366.) };
        let avail_w = (viewport_w - 10.).max(160.0_f32);
        let avail_h = (viewport_h - 26. - 8.).max(80.0_f32);
        let cell_w = (font_size * 0.6019_f32).max(4.0_f32);
        let cols = ((avail_w / cell_w).floor() as u16).clamp(20, 300);
        let rows_count = ((avail_h / row_height).floor() as u16).clamp(4, 120);
        self.resize(cols, rows_count);

        let (rows, scrollback) = match self.sessions.get(self.active) {
            Some(session) => {
                let Ok(parser) = session.parser.lock() else {
                    return div().into_any_element();
                };
                let screen = parser.screen();
                let (row_count, col_count) = screen.size();
                let (cursor_row, cursor_col) = screen.cursor_position();
                let cursor = if screen.hide_cursor() { None } else { Some((cursor_row, cursor_col)) };
                let mut rows: Vec<AnyElement> = Vec::with_capacity(row_count.get() as usize);
                for row in 0..row_count.get() {
                    rows.push(render_row(screen, row, col_count.get(), cursor, &theme, row_height));
                }
                (rows, screen.scrollback())
            }
            None => (Vec::new(), 0),
        };
        let running = self.sessions.get(self.active).is_some_and(|session| !session.exited.load(Ordering::Acquire));
        let title: SharedString =
            self.sessions.get(self.active).map(|session| session.title.clone()).unwrap_or_else(|| "Terminal".into());
        let tabs: Vec<(usize, SharedString, bool)> = self
            .sessions
            .iter()
            .enumerate()
            .map(|(index, session)| (index, session.title.clone(), index == self.active))
            .collect();
        let restart = cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| this.restart(window, cx));
        let kill = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.kill(cx));
        let new_tab = cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| this.start(window, cx));
        let start = cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| this.ensure(window, cx));
        let focus = cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| {
            let handle = this.focus_handle.clone();
            window.focus(&handle, cx);
        });
        let term_bg = theme.background;
        let hover_bg = theme.border;
        let tab_fg = theme.fg_strong();

        let mut toolbar = div()
            .h(px(26.))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .bg(theme.muted)
            .border_b_1()
            .border_color(theme.border)
            .child(Icon::new(IconName::SquareTerminal).xsmall().text_color(theme.fg_secondary()));
        for (index, label, active) in tabs {
            let activate = cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                this.active = index.min(this.sessions.len().saturating_sub(1));
                let handle = this.focus_handle.clone();
                window.focus(&handle, cx);
                cx.notify();
            });
            let close = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| this.close(index, cx));
            toolbar = toolbar.child(
                div()
                    .h(px(20.))
                    .pl_2()
                    .pr_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded(px(4.))
                    .when(active, |this| this.bg(theme.background))
                    .child(
                        div()
                            .id(ElementId::Name(format!("terminal-tab-{index}").into()))
                            .pr_1()
                            .text_size(px(10.5))
                            .whitespace_nowrap()
                            .cursor_pointer()
                            .text_color(if active { tab_fg } else { theme.fg_muted() })
                            .on_click(activate)
                            .child(label),
                    )
                    .child(
                        div()
                            .id(ElementId::Name(format!("terminal-tab-close-{index}").into()))
                            .size(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(move |this| this.bg(hover_bg))
                            .on_click(close)
                            .child(Icon::new(IconName::X).xsmall().text_color(theme.fg_muted())),
                    ),
            );
        }
        let toolbar = toolbar
            .child(
                div()
                    .id("terminal-new-tab")
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(move |this| this.bg(hover_bg))
                    .on_click(new_tab)
                    .child(Icon::new(IconName::Plus).xsmall().text_color(theme.fg_muted())),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(theme.fg_muted())
                    .child(format!("{title} · {}x{}", self.grid.0, self.grid.1)),
            )
            .when(scrollback > 0, |this| {
                this.child(
                    div().text_size(px(10.)).text_color(theme.warning).child(format!("scrollback {scrollback} (End)")),
                )
            })
            .child(
                div()
                    .id("terminal-new")
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(move |this| this.bg(hover_bg))
                    .on_click(restart)
                    .child(Icon::new(IconName::RefreshCw).xsmall().text_color(theme.fg_muted())),
            )
            .when(running, |this| {
                this.child(
                    div()
                        .id("terminal-kill")
                        .size(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(move |this| this.bg(hover_bg))
                        .on_click(kill)
                        .child(Icon::new(IconName::X).xsmall().text_color(theme.fg_muted())),
                )
            });

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(term_bg)
            .child(toolbar)
            .when_some(self.notice.clone(), |this, notice| {
                this.child(div().px_2().py_1().text_size(px(10.5)).text_color(theme.warning).child(notice))
            })
            .child(
                div()
                    .id("terminal-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .p_1()
                    .track_focus(&self.focus_handle)
                    .cursor_text()
                    .text_size(px(font_size))
                    .font_family(theme.mono_font_family.clone())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this: &mut Self, _: &MouseDownEvent, window, cx| {
                            let handle = this.focus_handle.clone();
                            window.focus(&handle, cx);
                        }),
                    )
                    .on_click(focus)
                    .on_key_down(cx.listener(Self::on_key))
                    .on_scroll(cx.listener(move |this: &mut Self, event: &ScrollEvent, _window, cx| {
                        let lines = match event.delta {
                            ScrollDelta::Lines(point) => point.y as i32,
                            ScrollDelta::Pixels(point) => (point.y / px(row_height)).round() as i32,
                        };
                        this.scroll(-lines, cx);
                    }))
                    .when(self.sessions.is_empty(), |this| {
                        this.child(
                            div()
                                .size_full()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap_3()
                                .text_size(px(font_size))
                                .text_color(theme.fg_muted())
                                .child("No terminal session")
                                .child(
                                    gpui_kit::component::button::Button::new("terminal-start")
                                        .label("Start Terminal")
                                        .primary()
                                        .on_click(start),
                                ),
                        )
                    })
                    .when(!self.sessions.is_empty(), move |this| this.child(div().flex().flex_col().children(rows))),
            )
            .into_any_element()
    }
}

fn render_row(
    screen: &vt100::Screen,
    row: u16,
    cols: u16,
    cursor: Option<(u16, u16)>,
    theme: &gpui_kit::component::Theme,
    row_height: f32,
) -> AnyElement {
    #[derive(PartialEq)]
    struct Style {
        fg: gpui_kit::Hsla,
        bg: gpui_kit::Hsla,
        bold: bool,
        underline: bool,
    }
    let mut runs: Vec<(String, Style)> = Vec::new();
    for col in 0..cols {
        let Some(cell) = screen.cell(row, col) else { continue };
        if cell.is_wide_continuation() {
            continue;
        }
        let mut fg = ansi_color(cell.fgcolor(), theme.foreground);
        let mut bg = ansi_color(cell.bgcolor(), theme.background);
        if cell.inverse() {
            std::mem::swap(&mut fg, &mut bg);
        }
        if Some((row, col)) == cursor {
            std::mem::swap(&mut fg, &mut bg);
            if cell.contents().is_empty() {
                // A cursor on a blank cell still needs a visible block.
                runs.push((" ".to_owned(), Style { fg, bg, bold: cell.bold(), underline: cell.underline() }));
                continue;
            }
        }
        let style = Style { fg, bg, bold: cell.bold(), underline: cell.underline() };
        let text = cell.contents();
        match runs.last_mut() {
            Some((content, last)) if *last == style => content.push_str(text),
            _ => runs.push((text.to_owned(), style)),
        }
    }
    div()
        .h(px(row_height))
        .flex()
        .flex_row()
        .items_center()
        .whitespace_nowrap()
        .children(runs.into_iter().map(move |(text, style)| {
            div()
                .h_full()
                .flex()
                .items_center()
                .bg(style.bg)
                .text_color(style.fg)
                .when(style.bold, |this| this.font_weight(FontWeight::BOLD))
                .when(style.underline, |this| this.underline())
                .child(text)
        }))
        .into_any_element()
}

fn ansi_color(color: vt100::Color, default: gpui_kit::Hsla) -> gpui_kit::Hsla {
    match color {
        vt100::Color::Default => default,
        vt100::Color::Rgb(r, g, b) => gpui_kit::rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32).into(),
        vt100::Color::Idx(index) => {
            let rgb = if index < 16 {
                ANSI[index as usize]
            } else if index < 232 {
                let index = index - 16;
                let levels = [0u32, 0x5f, 0x87, 0xaf, 0xd7, 0xff];
                let r = levels[(index / 36) as usize];
                let g = levels[((index % 36) / 6) as usize];
                let b = levels[(index % 6) as usize];
                (r << 16) | (g << 8) | b
            } else {
                let level = 8 + (index - 232) as u32 * 10;
                (level << 16) | (level << 8) | level
            };
            gpui_kit::rgb(rgb).into()
        }
    }
}
