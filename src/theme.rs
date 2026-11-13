//! The named color tokens every widget reads.
//!
//! Ported from OpenTUI's Rust framework (`packages/rust/crates/opentui/src/theme.rs`):
//! widgets read the theme when they draw, so the whole look changes with one
//! struct. Five named palettes ship: `dark` and `light` (explicit colors),
//! `catppuccin` (Catppuccin Mocha), `gruvbox` (Gruvbox Dark), and `terminal`,
//! which inherits the terminal's own colors.
//!
//! Depth comes from three background layers instead of borders:
//! `background` < `surface` < `surface_raised` (in the dark palettes; the
//! light palette runs the other way round).

use ratatui::style::{Color, Modifier, Style};

/// Every palette name `by_name` accepts, in the order `Theme::next` cycles.
pub const NAMES: &[&str] = &["dark", "light", "catppuccin", "gruvbox", "terminal"];

/// Look up a palette by name, case-insensitively. `auto` is not a palette;
/// the caller resolves it by asking the terminal.
pub fn by_name(name: &str) -> Option<Theme> {
    match name.trim().to_ascii_lowercase().as_str() {
        "dark" => Some(Theme::dark()),
        "light" => Some(Theme::light()),
        "catppuccin" => Some(Theme::catppuccin()),
        "gruvbox" => Some(Theme::gruvbox()),
        "terminal" => Some(Theme::terminal()),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    /// The registry name of this palette (one of `NAMES`).
    pub name: &'static str,
    /// The app background, behind everything.
    pub background: Color,
    /// Panels: one layer above the background.
    pub surface: Color,
    /// Focused panels and the now-playing bar: one layer above `surface`.
    pub surface_raised: Color,
    pub foreground: Color,
    /// Secondary text: hints, descriptions, placeholders (>= 4.5:1 on surface).
    pub muted: Color,
    /// Tertiary text: numbers, durations, dividers (about 55% of foreground).
    pub dim: Color,
    pub accent: Color,
    /// Unfilled progress track and other hairlines.
    pub border: Color,
    /// Legacy: rendering now uses `accent` for focus.
    pub focus: Color,
    /// Text selected with the mouse or keyboard.
    pub selection_bg: Color,
    pub selection_fg: Color,
    /// The current item of a list or tab bar.
    pub highlight_bg: Color,
    pub highlight_fg: Color,
    /// Text input fields.
    pub input_bg: Color,
    pub input_focused_bg: Color,
    pub cursor: Color,
    /// Slider and scrollbar tracks.
    pub track: Color,
    /// Slider and scrollbar thumbs.
    pub thumb: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
}

impl Theme {
    pub const fn dark() -> Self {
        Self {
            name: "dark",
            background: Color::Rgb(0x0f, 0x12, 0x17),
            surface: Color::Rgb(0x16, 0x1b, 0x22),
            surface_raised: Color::Rgb(0x1d, 0x23, 0x2c),
            foreground: Color::Rgb(0xdb, 0xe2, 0xea),
            muted: Color::Rgb(0x8c, 0x98, 0xa8),
            dim: Color::Rgb(0x64, 0x6d, 0x79),
            accent: Color::Rgb(0x68, 0xd3, 0xa8),
            border: Color::Rgb(0x3a, 0x45, 0x52),
            focus: Color::Rgb(0x68, 0xd3, 0xa8),
            selection_bg: Color::Rgb(0x2f, 0x4a, 0x63),
            selection_fg: Color::Rgb(0xff, 0xff, 0xff),
            highlight_bg: Color::Rgb(0x2a, 0x33, 0x40),
            highlight_fg: Color::Rgb(0xff, 0xff, 0xff),
            input_bg: Color::Rgb(0x1d, 0x23, 0x2c),
            input_focused_bg: Color::Rgb(0x24, 0x2c, 0x37),
            cursor: Color::Rgb(0xdb, 0xe2, 0xea),
            track: Color::Rgb(0x25, 0x2b, 0x33),
            thumb: Color::Rgb(0x6b, 0x7a, 0x8c),
            success: Color::Rgb(0x68, 0xd3, 0xa8),
            warning: Color::Rgb(0xf0, 0xc6, 0x74),
            error: Color::Rgb(0xff, 0x6b, 0x6b),
        }
    }

    pub const fn light() -> Self {
        Self {
            name: "light",
            background: Color::Rgb(0xf6, 0xf7, 0xf9),
            surface: Color::Rgb(0xed, 0xf0, 0xf4),
            surface_raised: Color::Rgb(0xe3, 0xe8, 0xee),
            foreground: Color::Rgb(0x1f, 0x29, 0x33),
            muted: Color::Rgb(0x58, 0x63, 0x71),
            dim: Color::Rgb(0x80, 0x8b, 0x99),
            accent: Color::Rgb(0x0f, 0x8a, 0x64),
            border: Color::Rgb(0xb8, 0xc2, 0xcc),
            focus: Color::Rgb(0x0f, 0x8a, 0x64),
            selection_bg: Color::Rgb(0xc8, 0xdd, 0xf7),
            selection_fg: Color::Rgb(0x1f, 0x29, 0x33),
            highlight_bg: Color::Rgb(0xd6, 0xde, 0xe8),
            highlight_fg: Color::Rgb(0x0b, 0x16, 0x20),
            input_bg: Color::Rgb(0xe3, 0xe8, 0xee),
            input_focused_bg: Color::Rgb(0xda, 0xe1, 0xe9),
            cursor: Color::Rgb(0x1f, 0x29, 0x33),
            track: Color::Rgb(0xdd, 0xe3, 0xe9),
            thumb: Color::Rgb(0x8a, 0x97, 0xa6),
            success: Color::Rgb(0x0f, 0x8a, 0x64),
            warning: Color::Rgb(0xb7, 0x79, 0x1f),
            error: Color::Rgb(0xc5, 0x30, 0x30),
        }
    }

    /// Catppuccin Mocha (MIT, https://github.com/catppuccin/catppuccin).
    /// Layers: crust < base < surface0. Accent is mauve.
    pub const fn catppuccin() -> Self {
        Self {
            name: "catppuccin",
            background: Color::Rgb(0x11, 0x11, 0x1b), // crust
            surface: Color::Rgb(0x1e, 0x1e, 0x2e),    // base
            surface_raised: Color::Rgb(0x31, 0x32, 0x44), // surface0
            foreground: Color::Rgb(0xcd, 0xd6, 0xf4), // text
            muted: Color::Rgb(0xa6, 0xad, 0xc8),      // subtext0
            dim: Color::Rgb(0x6c, 0x70, 0x86),        // overlay0
            accent: Color::Rgb(0xcb, 0xa6, 0xf7),     // mauve
            border: Color::Rgb(0x45, 0x47, 0x5a),     // surface1
            focus: Color::Rgb(0xcb, 0xa6, 0xf7),      // mauve
            selection_bg: Color::Rgb(0x58, 0x5b, 0x70), // surface2
            selection_fg: Color::Rgb(0xcd, 0xd6, 0xf4), // text
            highlight_bg: Color::Rgb(0x45, 0x47, 0x5a), // surface1
            highlight_fg: Color::Rgb(0xcd, 0xd6, 0xf4), // text
            input_bg: Color::Rgb(0x31, 0x32, 0x44),   // surface0
            input_focused_bg: Color::Rgb(0x45, 0x47, 0x5a), // surface1
            cursor: Color::Rgb(0xf5, 0xe0, 0xdc),     // rosewater
            track: Color::Rgb(0x45, 0x47, 0x5a),      // surface1
            thumb: Color::Rgb(0x93, 0x99, 0xb2),      // overlay2
            success: Color::Rgb(0xa6, 0xe3, 0xa1),    // green
            warning: Color::Rgb(0xf9, 0xe2, 0xaf),    // yellow
            error: Color::Rgb(0xf3, 0x8b, 0xa8),      // red
        }
    }

    /// Gruvbox Dark (MIT, https://github.com/morhetz/gruvbox). Background is
    /// the hard `bg0_h`; the surface and raised layers step up from `bg0_s`
    /// and `bg1`, with the raised layer lifted a little so it sits clearly
    /// above the surface. Accent is orange.
    pub const fn gruvbox() -> Self {
        Self {
            name: "gruvbox",
            background: Color::Rgb(0x1d, 0x20, 0x21), // bg0_h
            surface: Color::Rgb(0x32, 0x30, 0x2f),    // bg0_s
            surface_raised: Color::Rgb(0x45, 0x40, 0x3d), // bg1 lifted toward bg2
            foreground: Color::Rgb(0xeb, 0xdb, 0xb2), // fg
            muted: Color::Rgb(0xbd, 0xae, 0x93),      // fg3
            dim: Color::Rgb(0x92, 0x83, 0x74),        // gray
            accent: Color::Rgb(0xfe, 0x80, 0x19),     // orange
            border: Color::Rgb(0x50, 0x49, 0x45),     // bg2
            focus: Color::Rgb(0xfe, 0x80, 0x19),      // orange
            selection_bg: Color::Rgb(0x66, 0x5c, 0x54), // bg3
            selection_fg: Color::Rgb(0xfb, 0xf1, 0xc7), // fg0
            highlight_bg: Color::Rgb(0x50, 0x49, 0x45), // bg2
            highlight_fg: Color::Rgb(0xfb, 0xf1, 0xc7), // fg0
            input_bg: Color::Rgb(0x3c, 0x38, 0x36),   // bg1
            input_focused_bg: Color::Rgb(0x50, 0x49, 0x45), // bg2
            cursor: Color::Rgb(0xfb, 0xf1, 0xc7),     // fg0
            track: Color::Rgb(0x3c, 0x38, 0x36),      // bg1
            thumb: Color::Rgb(0x92, 0x83, 0x74),      // gray
            success: Color::Rgb(0xb8, 0xbb, 0x26),    // green
            warning: Color::Rgb(0xfa, 0xbd, 0x2f),    // yellow
            error: Color::Rgb(0xfb, 0x49, 0x34),      // red
        }
    }

    pub const fn terminal() -> Self {
        Self {
            name: "terminal",
            background: Color::Reset,
            surface: Color::Reset,
            surface_raised: Color::Reset,
            foreground: Color::Reset,
            muted: Color::DarkGray,
            dim: Color::DarkGray,
            accent: Color::Cyan,
            border: Color::DarkGray,
            focus: Color::Cyan,
            selection_bg: Color::Blue,
            selection_fg: Color::White,
            highlight_bg: Color::DarkGray,
            highlight_fg: Color::White,
            input_bg: Color::Reset,
            input_focused_bg: Color::Reset,
            cursor: Color::Reset,
            track: Color::DarkGray,
            thumb: Color::Gray,
            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,
        }
    }

    /// Pick dark or light from the terminal's background brightness, the
    /// way OpenTUI's `inferThemeModeFromBackgroundColor` does.
    pub fn for_background(hex: u32) -> Self {
        let r = (hex >> 16) & 0xff;
        let g = (hex >> 8) & 0xff;
        let b = hex & 0xff;
        if (r * 299 + g * 587 + b * 114) / 1000 > 128 {
            Self::light()
        } else {
            Self::dark()
        }
    }

    /// The following palette in `NAMES` order, wrapping after `terminal`.
    /// A theme whose name is not in `NAMES` cycles from the start.
    pub fn next(self) -> Theme {
        let index = NAMES
            .iter()
            .position(|name| *name == self.name)
            .unwrap_or(NAMES.len() - 1);
        by_name(NAMES[(index + 1) % NAMES.len()]).unwrap_or_else(Self::dark)
    }

    /// The terminal palette has no background layers (they are all `Reset`),
    /// so hierarchy is carried by modifiers instead.
    pub fn is_terminal(&self) -> bool {
        self.surface == Color::Reset
    }

    pub fn text(&self) -> Style {
        Style::default().fg(self.foreground)
    }

    pub fn muted_text(&self) -> Style {
        Style::default().fg(self.muted)
    }

    /// Tertiary text; `DIM` stands in for the lighter color on the terminal palette.
    pub fn dim_text(&self) -> Style {
        let style = Style::default().fg(self.dim);
        if self.is_terminal() {
            style.add_modifier(Modifier::DIM)
        } else {
            style
        }
    }

    pub fn border(&self, focused: bool) -> Style {
        Style::default().fg(if focused { self.focus } else { self.border })
    }

    /// The background layer of a panel.
    pub fn panel_bg(&self, focused: bool) -> Color {
        if focused {
            self.surface_raised
        } else {
            self.surface
        }
    }

    pub fn highlight(&self) -> Style {
        Style::default()
            .bg(self.highlight_bg)
            .fg(self.highlight_fg)
            .add_modifier(Modifier::BOLD)
    }

    /// Style of the selected row of a list (background only; text keeps its
    /// own colors).
    pub fn selected(&self, focused: bool) -> Style {
        if self.is_terminal() {
            if focused {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            }
        } else if focused {
            Style::default().bg(self.highlight_bg)
        } else {
            Style::default().bg(self.surface_raised)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG 2.x relative luminance of an sRGB color.
    fn relative_luminance(color: Color) -> f64 {
        let Color::Rgb(r, g, b) = color else {
            panic!("contrast needs an RGB color, got {color:?}");
        };
        let linear = |channel: u8| {
            let s = f64::from(channel) / 255.0;
            if s <= 0.03928 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    }

    /// WCAG contrast ratio between two colors, from 1.0 to 21.0.
    fn contrast(a: Color, b: Color) -> f64 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn truecolor_themes() -> Vec<Theme> {
        NAMES
            .iter()
            .filter(|name| **name != "terminal")
            .map(|name| by_name(name).expect("registered name"))
            .collect()
    }

    #[test]
    fn dark_palette_has_explicit_colors() {
        let theme = Theme::dark();
        assert_ne!(theme.background, Color::Reset);
        assert_ne!(theme.accent, theme.foreground);
        assert_ne!(theme.surface, theme.surface_raised);
        assert!(!theme.is_terminal());
        assert!(Theme::terminal().is_terminal());
    }

    #[test]
    fn infers_mode_from_background_brightness() {
        assert_eq!(Theme::for_background(0xffffff), Theme::light());
        assert_eq!(Theme::for_background(0x000000), Theme::dark());
        assert_eq!(Theme::for_background(0xf0f0f0), Theme::light());
        assert_eq!(Theme::for_background(0xffffff).name, "light");
        assert_eq!(Theme::for_background(0x000000).name, "dark");
    }

    #[test]
    fn every_registered_name_resolves() {
        for name in NAMES {
            let theme = by_name(name).unwrap_or_else(|| panic!("{name} should resolve"));
            assert_eq!(theme.name, *name);
            assert_eq!(by_name(&name.to_ascii_uppercase()), Some(theme));
        }
        assert_eq!(by_name("  Gruvbox "), Some(Theme::gruvbox()));
        assert_eq!(by_name("auto"), None);
        assert_eq!(by_name("solarized"), None);
    }

    #[test]
    fn next_cycles_through_all_and_wraps() {
        let mut theme = Theme::dark();
        let mut seen = Vec::new();
        for _ in 0..NAMES.len() {
            theme = theme.next();
            seen.push(theme.name);
        }
        assert_eq!(seen, vec!["light", "catppuccin", "gruvbox", "terminal", "dark"]);
        assert_eq!(theme, Theme::dark());
        assert_eq!(Theme::terminal().next().name, "dark");
    }

    #[test]
    fn truecolor_layers_are_distinct() {
        for theme in truecolor_themes() {
            assert_ne!(theme.background, theme.surface, "{}", theme.name);
            assert_ne!(theme.surface, theme.surface_raised, "{}", theme.name);
            assert_ne!(theme.background, theme.surface_raised, "{}", theme.name);
        }
    }

    #[test]
    fn truecolor_text_contrast() {
        for theme in truecolor_themes() {
            let body = contrast(theme.foreground, theme.background);
            assert!(body >= 7.0, "{}: foreground/background {body:.2} < 7", theme.name);
            let muted = contrast(theme.muted, theme.surface);
            assert!(muted >= 4.5, "{}: muted/surface {muted:.2} < 4.5", theme.name);
        }
    }
}
