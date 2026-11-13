//! The named color tokens every widget reads.
//!
//! Ported from OpenTUI's Rust framework (`packages/rust/crates/opentui/src/theme.rs`):
//! widgets read the theme when they draw, so the whole look changes with one
//! struct. Three palettes ship: explicit dark and light, and `terminal`,
//! which inherits the terminal's own colors.
//!
//! Depth comes from three background layers instead of borders:
//! `background` < `surface` < `surface_raised`.

use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
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

    pub const fn terminal() -> Self {
        Self {
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
    }
}
