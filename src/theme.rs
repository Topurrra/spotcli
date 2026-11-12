//! The named color tokens every widget reads.
//!
//! Ported from OpenTUI's Rust framework (`packages/rust/crates/opentui/src/theme.rs`):
//! widgets read the theme when they draw, so the whole look changes with one
//! struct. Three palettes ship: explicit dark and light, and `terminal`,
//! which inherits the terminal's own colors.

use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    /// The app background, behind everything.
    pub background: Color,
    /// Panels and other raised areas.
    pub surface: Color,
    pub foreground: Color,
    /// Secondary text: hints, descriptions, placeholders.
    pub muted: Color,
    pub accent: Color,
    pub border: Color,
    /// Borders and highlights of the focused widget.
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
            background: Color::Rgb(0x12, 0x16, 0x1c),
            surface: Color::Rgb(0x1a, 0x20, 0x28),
            foreground: Color::Rgb(0xdb, 0xe2, 0xea),
            muted: Color::Rgb(0x7d, 0x8a, 0x99),
            accent: Color::Rgb(0x68, 0xd3, 0xa8),
            border: Color::Rgb(0x43, 0x53, 0x63),
            focus: Color::Rgb(0x5f, 0xb3, 0xff),
            selection_bg: Color::Rgb(0x2f, 0x4a, 0x63),
            selection_fg: Color::Rgb(0xff, 0xff, 0xff),
            highlight_bg: Color::Rgb(0x26, 0x38, 0x4a),
            highlight_fg: Color::Rgb(0xff, 0xff, 0xff),
            input_bg: Color::Rgb(0x1e, 0x25, 0x2e),
            input_focused_bg: Color::Rgb(0x24, 0x30, 0x40),
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
            background: Color::Rgb(0xf7, 0xf8, 0xfa),
            surface: Color::Rgb(0xec, 0xef, 0xf3),
            foreground: Color::Rgb(0x1f, 0x29, 0x33),
            muted: Color::Rgb(0x6b, 0x77, 0x85),
            accent: Color::Rgb(0x0f, 0x8a, 0x64),
            border: Color::Rgb(0xb8, 0xc2, 0xcc),
            focus: Color::Rgb(0x1f, 0x6f, 0xeb),
            selection_bg: Color::Rgb(0xc8, 0xdd, 0xf7),
            selection_fg: Color::Rgb(0x1f, 0x29, 0x33),
            highlight_bg: Color::Rgb(0xdc, 0xe7, 0xf3),
            highlight_fg: Color::Rgb(0x0b, 0x16, 0x20),
            input_bg: Color::Rgb(0xec, 0xef, 0xf3),
            input_focused_bg: Color::Rgb(0xe1, 0xe8, 0xf0),
            cursor: Color::Rgb(0x1f, 0x29, 0x33),
            track: Color::Rgb(0xdd, 0xe3, 0xe9),
            thumb: Color::Rgb(0x8a, 0x97, 0xa6),
            success: Color::Rgb(0x0f, 0x8a, 0x64),
            warning: Color::Rgb(0xb7, 0x79, 0x1f),
            error: Color::Rgb(0xc5, 0x30, 0x30),
        }
    }

    pub const fn terminal() -> Self {
        Self {
            background: Color::Reset,
            surface: Color::Reset,
            foreground: Color::Reset,
            muted: Color::DarkGray,
            accent: Color::Cyan,
            border: Color::DarkGray,
            focus: Color::Blue,
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

    pub fn text(&self) -> Style {
        Style::default().fg(self.foreground)
    }

    pub fn muted_text(&self) -> Style {
        Style::default().fg(self.muted)
    }

    pub fn border(&self, focused: bool) -> Style {
        Style::default().fg(if focused { self.focus } else { self.border })
    }

    pub fn highlight(&self) -> Style {
        Style::default()
            .bg(self.highlight_bg)
            .fg(self.highlight_fg)
            .add_modifier(Modifier::BOLD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_palette_has_explicit_colors() {
        let theme = Theme::dark();
        assert_ne!(theme.background, Color::Reset);
        assert_ne!(theme.accent, theme.foreground);
    }

    #[test]
    fn infers_mode_from_background_brightness() {
        assert_eq!(Theme::for_background(0xffffff), Theme::light());
        assert_eq!(Theme::for_background(0x000000), Theme::dark());
        assert_eq!(Theme::for_background(0xf0f0f0), Theme::light());
    }
}
