//! OpenIDEA's palettes, mapped from `src/styles/tokens.css` onto the
//! `gpui-component` theme.
//!
//! The web UI resolves colours from CSS custom properties (`--bg-app`,
//! `--fg-muted`, …) and selects a palette with `data-color-theme`. The native
//! UI keeps the same source of truth by porting the exact values here: Islands
//! Dark/Light, High Contrast, Solarized Dark/Light and One Dark/Light. The
//! chosen theme is stored in `<config>/ui.json`.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use std::path::Path;

/// `gpui-component` colours for one OpenIDEA theme.
struct Palette {
    /// `--bg-app`
    app: u32,
    /// `--bg-elev`
    elev: u32,
    /// `--bg-popup`
    popup: u32,
    /// `--bg-hover`
    hover: u32,
    /// `--bg-selected`
    selected: u32,
    /// `--bg-editor`
    editor: u32,
    /// `--border`
    border: u32,
    /// `--border-strong`
    border_strong: u32,
    /// `--fg`
    fg: u32,
    /// `--fg-strong`
    fg_strong: u32,
    /// `--fg-secondary`
    fg_secondary: u32,
    /// `--fg-muted`
    fg_muted: u32,
    /// `--accent`
    accent: u32,
    /// `--link`
    link: u32,
    /// `--danger`
    danger: u32,
    /// `--warning`
    warning: u32,
    /// `--success`
    success: u32,
    /// `--info`
    info: u32,
}

const ISLANDS_DARK: Palette = Palette {
    app: 0x191a1c,
    elev: 0x212326,
    popup: 0x26282c,
    hover: 0x2e2f30,
    selected: 0x2a4371,
    editor: 0x191a1c,
    border: 0x26282c,
    border_strong: 0x40434a,
    fg: 0xd1d3d9,
    fg_strong: 0xdfe1e5,
    fg_secondary: 0x9fa2a8,
    fg_muted: 0x8c8f95,
    accent: 0x3871e1,
    link: 0x71a1fe,
    danger: 0xf57e84,
    warning: 0xd59637,
    success: 0x6db083,
    info: 0x548af7,
};

const ISLANDS_LIGHT: Palette = Palette {
    app: 0xffffff,
    elev: 0xf7f8f9,
    popup: 0xffffff,
    hover: 0xededed,
    selected: 0xd0dffe,
    editor: 0xffffff,
    border: 0xe9eaee,
    border_strong: 0xd1d3d9,
    fg: 0x000000,
    fg_strong: 0x000000,
    fg_secondary: 0x5f6269,
    fg_muted: 0x6c707e,
    accent: 0x3871e1,
    link: 0x2f5eb9,
    danger: 0xc54e58,
    warning: 0xa56906,
    success: 0x338555,
    info: 0x3871e1,
};

/// `data-theme="dark-hc"` / `data-contrast="high"`.
const HIGH_CONTRAST: Palette = Palette {
    app: 0x000000,
    elev: 0x0e0f11,
    popup: 0x111214,
    hover: 0x1c1e22,
    selected: 0x1f3a73,
    editor: 0x000000,
    border: 0x45474d,
    border_strong: 0x6f737a,
    fg: 0xe3e4e8,
    fg_strong: 0xffffff,
    fg_secondary: 0xccced3,
    fg_muted: 0xb6b8be,
    accent: 0x8fb3ff,
    link: 0xa7c5ff,
    danger: 0xff9b9f,
    warning: 0xf2c55c,
    success: 0x8ec39d,
    info: 0x92b7ff,
};

const SOLARIZED_DARK: Palette = Palette {
    app: 0x002b36,
    elev: 0x073642,
    popup: 0x073642,
    hover: 0x073642,
    selected: 0x0a4a5a,
    editor: 0x002b36,
    border: 0x073642,
    border_strong: 0x586e75,
    fg: 0x839496,
    fg_strong: 0x93a1a1,
    fg_secondary: 0x839496,
    fg_muted: 0x586e75,
    accent: 0x268bd2,
    link: 0x268bd2,
    danger: 0xdc322f,
    warning: 0xb58900,
    success: 0x859900,
    info: 0x268bd2,
};

const SOLARIZED_LIGHT: Palette = Palette {
    app: 0xfdf6e3,
    elev: 0xeee8d5,
    popup: 0xfdf6e3,
    hover: 0xeee8d5,
    selected: 0xd9d2bd,
    editor: 0xfdf6e3,
    border: 0xeee8d5,
    border_strong: 0x93a1a1,
    fg: 0x657b83,
    fg_strong: 0x586e75,
    fg_secondary: 0x657b83,
    fg_muted: 0x93a1a1,
    accent: 0x268bd2,
    link: 0x268bd2,
    danger: 0xdc322f,
    warning: 0xb58900,
    success: 0x859900,
    info: 0x268bd2,
};

const ONE_DARK: Palette = Palette {
    app: 0x282c34,
    elev: 0x21252b,
    popup: 0x252830,
    hover: 0x31363f,
    selected: 0x3e4451,
    editor: 0x282c34,
    border: 0x181a1f,
    border_strong: 0x3e4451,
    fg: 0xabb2bf,
    fg_strong: 0xdcdfe4,
    fg_secondary: 0xabb2bf,
    fg_muted: 0x828997,
    accent: 0x528bff,
    link: 0x61afef,
    danger: 0xe06c75,
    warning: 0xe5c07b,
    success: 0x98c379,
    info: 0x61afef,
};

const ONE_LIGHT: Palette = Palette {
    app: 0xfafafa,
    elev: 0xeaeaeb,
    popup: 0xf2f2f2,
    hover: 0xe5e5e6,
    selected: 0xd8d9db,
    editor: 0xfafafa,
    border: 0xdbdbdc,
    border_strong: 0xbfbfc0,
    fg: 0x383a42,
    fg_strong: 0x202227,
    fg_secondary: 0x696c77,
    fg_muted: 0xa0a1a7,
    accent: 0x526fff,
    link: 0x4078f2,
    danger: 0xe45649,
    warning: 0xc18401,
    success: 0x50a14f,
    info: 0x4078f2,
};

/// The built-in appearance choices (the same ids as `src/platform/themes.ts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeChoice {
    IslandsDark,
    IslandsLight,
    HighContrast,
    SolarizedDark,
    SolarizedLight,
    OneDark,
    OneLight,
}

impl ThemeChoice {
    pub fn all() -> [ThemeChoice; 7] {
        [
            ThemeChoice::IslandsDark,
            ThemeChoice::IslandsLight,
            ThemeChoice::HighContrast,
            ThemeChoice::SolarizedDark,
            ThemeChoice::SolarizedLight,
            ThemeChoice::OneDark,
            ThemeChoice::OneLight,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::IslandsDark => "Islands Dark",
            ThemeChoice::IslandsLight => "Islands Light",
            ThemeChoice::HighContrast => "High Contrast",
            ThemeChoice::SolarizedDark => "Solarized Dark",
            ThemeChoice::SolarizedLight => "Solarized Light",
            ThemeChoice::OneDark => "One Dark",
            ThemeChoice::OneLight => "One Light",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            ThemeChoice::IslandsDark => "dark",
            ThemeChoice::IslandsLight => "light",
            ThemeChoice::HighContrast => "dark-hc",
            ThemeChoice::SolarizedDark => "solarized-dark",
            ThemeChoice::SolarizedLight => "solarized-light",
            ThemeChoice::OneDark => "one-dark",
            ThemeChoice::OneLight => "one-light",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        ThemeChoice::all().into_iter().find(|choice| choice.id() == id)
    }

    pub fn is_dark(self) -> bool {
        matches!(
            self,
            ThemeChoice::IslandsDark | ThemeChoice::HighContrast | ThemeChoice::SolarizedDark | ThemeChoice::OneDark
        )
    }

    /// The light/dark counterpart of the same family (what "Toggle Theme" does).
    pub fn counterpart(self) -> Self {
        match self {
            ThemeChoice::IslandsDark => ThemeChoice::IslandsLight,
            ThemeChoice::IslandsLight => ThemeChoice::IslandsDark,
            ThemeChoice::HighContrast => ThemeChoice::IslandsLight,
            ThemeChoice::SolarizedDark => ThemeChoice::SolarizedLight,
            ThemeChoice::SolarizedLight => ThemeChoice::SolarizedDark,
            ThemeChoice::OneDark => ThemeChoice::OneLight,
            ThemeChoice::OneLight => ThemeChoice::OneDark,
        }
    }

    fn palette(self) -> &'static Palette {
        match self {
            ThemeChoice::IslandsDark => &ISLANDS_DARK,
            ThemeChoice::IslandsLight => &ISLANDS_LIGHT,
            ThemeChoice::HighContrast => &HIGH_CONTRAST,
            ThemeChoice::SolarizedDark => &SOLARIZED_DARK,
            ThemeChoice::SolarizedLight => &SOLARIZED_LIGHT,
            ThemeChoice::OneDark => &ONE_DARK,
            ThemeChoice::OneLight => &ONE_LIGHT,
        }
    }
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// Install the OpenIDEA theme before the first window opens.
pub fn install(cx: &mut App, dark: bool) {
    install_choice(cx, if dark { ThemeChoice::IslandsDark } else { ThemeChoice::IslandsLight });
}

pub fn install_choice(cx: &mut App, choice: ThemeChoice) {
    Theme::change(if choice.is_dark() { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    apply(cx, choice);
}

/// Re-apply a palette without reloading the registered theme file.
pub fn apply(cx: &mut App, choice: ThemeChoice) {
    let p = choice.palette();
    Theme::update(cx, |theme| {
        // UI text is 13px in the Islands themes, the editor 13px at 100% zoom.
        theme.font_size = px(13.);
        theme.mono_font_size = px(13.);
        theme.radius = px(6.);
        theme.radius_lg = px(10.);
        theme.shadow = true;
        // GPUI's ".SystemUIFont" alias does not resolve on every Linux setup;
        // name an installed family there instead of letting text fall back to a
        // glyph-less placeholder.
        #[cfg(target_os = "linux")]
        {
            theme.font_family = "DejaVu Sans".into();
            theme.mono_font_family = "DejaVu Sans Mono".into();
        }
        #[cfg(target_os = "windows")]
        {
            theme.font_family = "Segoe UI".into();
            theme.mono_font_family = "Consolas".into();
        }

        let colors = &mut theme.colors;
        colors.background = c(p.app);
        colors.foreground = c(p.fg);
        colors.border = c(p.border);
        colors.muted = c(p.elev);
        colors.muted_foreground = c(p.fg_muted);
        colors.popover = c(p.popup);
        colors.popover_foreground = c(p.fg);
        colors.primary = c(p.accent);
        colors.primary_hover = c(p.accent);
        colors.primary_active = c(p.accent);
        colors.primary_foreground = c(0xffffff);
        colors.secondary = c(p.elev);
        colors.secondary_hover = c(p.hover);
        colors.secondary_active = c(p.hover);
        colors.secondary_foreground = c(p.fg);
        colors.accent = c(p.selected);
        colors.accent_foreground = c(p.fg_strong);
        colors.ring = c(p.accent);
        colors.link = c(p.link);
        colors.link_hover = c(p.link);
        colors.link_active = c(p.link);
        colors.danger = c(p.danger);
        colors.danger_foreground = c(0xffffff);
        colors.success = c(p.success);
        colors.success_foreground = c(0xffffff);
        colors.warning = c(p.warning);
        colors.warning_foreground = c(0xffffff);
        colors.info = c(p.info);
        colors.info_foreground = c(0xffffff);
        colors.selection = c(p.selected);
        colors.caret = c(p.fg);
        colors.sidebar = c(p.app);
        colors.sidebar_foreground = c(p.fg);
        colors.sidebar_border = c(p.border);
        colors.sidebar_accent = c(p.hover);
        colors.sidebar_accent_foreground = c(p.fg_strong);
        colors.sidebar_primary = c(p.accent);
        colors.sidebar_primary_foreground = c(0xffffff);
        colors.tab = c(p.app);
        colors.tab_active = c(p.editor);
        colors.tab_bar = c(p.elev);
        colors.tab_foreground = c(p.fg_secondary);
        colors.tab_active_foreground = c(p.fg_strong);
        colors.title_bar = c(p.elev);
        colors.title_bar_border = c(p.border);
        colors.status_bar = c(p.elev);
        colors.status_bar_border = c(p.border);
        colors.list = c(p.editor);
        colors.list_hover = c(p.hover);
        colors.list_active = c(p.selected);
        colors.list_active_border = c(p.accent);
        colors.list_even = c(p.elev);
        colors.list_head = c(p.elev);
        colors.input = c(p.app);
        colors.progress_bar = c(p.accent);
        colors.scrollbar = c(p.app);
        colors.scrollbar_thumb = c(p.border_strong);
        colors.scrollbar_thumb_hover = c(p.fg_muted);
        colors.window_border = c(p.border_strong);
        colors.overlay = hsla(0., 0., 0., 0.35);
    });
}

fn preferences_file(config_dir: &Path) -> std::path::PathBuf {
    config_dir.join("ui.json")
}

/// The stored appearance, if `ui.json` has one.
pub fn load_choice(config_dir: &Path) -> Option<ThemeChoice> {
    let bytes = std::fs::read(preferences_file(config_dir)).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value.get("theme")?.as_str().and_then(ThemeChoice::from_id)
}

/// Persist the appearance without dropping other UI preferences in the file.
pub fn save_choice(config_dir: &Path, choice: ThemeChoice) {
    let file = preferences_file(config_dir);
    let mut value: serde_json::Value = std::fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !value.is_object() {
        value = serde_json::json!({});
    }
    value["theme"] = serde_json::Value::String(choice.id().to_owned());
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(&value) {
        let _ = std::fs::write(file, bytes);
    }
}

/// Token names used by the workbench, mapped onto `gpui-component`'s colour
/// slots so call sites keep reading the design-token vocabulary.
pub trait IdeColors {
    fn fg(&self) -> Hsla;
    fn fg_strong(&self) -> Hsla;
    fn fg_secondary(&self) -> Hsla;
    fn fg_muted(&self) -> Hsla;
    fn border_strong(&self) -> Hsla;
}

impl IdeColors for Theme {
    fn fg(&self) -> Hsla {
        self.foreground
    }
    fn fg_strong(&self) -> Hsla {
        self.foreground
    }
    fn fg_secondary(&self) -> Hsla {
        self.secondary_foreground
    }
    fn fg_muted(&self) -> Hsla {
        self.muted_foreground
    }
    fn border_strong(&self) -> Hsla {
        self.border
    }
}
