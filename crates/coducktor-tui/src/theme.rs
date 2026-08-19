use std::env;

use ratatui::style::Color;

/// The supported named themes. There is deliberately no system theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    Dark,
    LazyVim,
    Lakes,
}

impl ThemeName {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Self::Dark),
            "lazyvim" | "lazy-vim" => Some(Self::LazyVim),
            "lakes" | "lakes-and-light" => Some(Self::Lakes),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::LazyVim => "lazyvim",
            Self::Lakes => "lakes",
        }
    }
}

/// Terminal color capability detected at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorCapability {
    TrueColor,
    Ansi256,
    Ansi16,
}

impl ColorCapability {
    pub fn detect() -> Self {
        let color_term = env::var("COLORTERM")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if color_term == "truecolor" || color_term == "24bit" {
            return Self::TrueColor;
        }
        if env::var("TERM")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("256")
        {
            Self::Ansi256
        } else {
            Self::Ansi16
        }
    }

    fn color(self, rgb: (u8, u8, u8), indexed: u8, ansi: u8) -> Color {
        match self {
            Self::TrueColor => Color::Rgb(rgb.0, rgb.1, rgb.2),
            Self::Ansi256 => Color::Indexed(indexed),
            Self::Ansi16 => Color::Indexed(ansi),
        }
    }
}

/// Semantic colors used by the shell and later screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemePalette {
    pub bg: Color,
    pub surface: Color,
    pub border: Color,
    pub fg: Color,
    pub soft_fg: Color,
    pub accent: Color,
    pub add: Color,
    pub del: Color,
    pub queued: Color,
    pub running: Color,
    pub waiting: Color,
    pub review: Color,
    pub done: Color,
    pub failed: Color,
    pub cancelled: Color,
}

/// A named palette plus the capability used to quantize it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub name: ThemeName,
    pub capability: ColorCapability,
    pub palette: ThemePalette,
}

impl Theme {
    pub fn detect() -> Self {
        Self::new(ThemeName::LazyVim, ColorCapability::detect())
    }

    pub fn new(name: ThemeName, capability: ColorCapability) -> Self {
        let palette = match name {
            ThemeName::Dark => palette(
                capability,
                (20, 22, 25),
                (29, 32, 36),
                (61, 66, 74),
                (235, 238, 242),
                (157, 164, 174),
                (168, 216, 80),
            ),
            ThemeName::LazyVim => palette(
                capability,
                (26, 27, 38),
                (36, 40, 59),
                (65, 72, 104),
                (192, 202, 245),
                (130, 139, 184),
                (122, 162, 247),
            ),
            ThemeName::Lakes => lakes_palette(capability),
        };
        Self {
            name,
            capability,
            palette,
        }
    }
}

/// Lakes & Light's parchment and sepia palette, adapted from its Omarchy theme.
fn lakes_palette(capability: ColorCapability) -> ThemePalette {
    ThemePalette {
        bg: capability.color((245, 228, 216), 224, 15),
        surface: capability.color((246, 231, 220), 224, 15),
        border: capability.color((184, 171, 162), 248, 8),
        fg: capability.color((58, 25, 17), 52, 0),
        soft_fg: capability.color((127, 121, 116), 243, 8),
        accent: capability.color((128, 95, 54), 95, 3),
        add: capability.color((139, 95, 15), 94, 2),
        del: capability.color((86, 43, 0), 52, 1),
        queued: capability.color((127, 121, 116), 243, 8),
        running: capability.color((128, 95, 54), 95, 3),
        waiting: capability.color((123, 85, 33), 94, 3),
        review: capability.color((86, 56, 25), 52, 5),
        done: capability.color((101, 62, 0), 58, 2),
        failed: capability.color((86, 43, 0), 52, 1),
        cancelled: capability.color((127, 121, 116), 243, 8),
    }
}

fn palette(
    capability: ColorCapability,
    bg: (u8, u8, u8),
    surface: (u8, u8, u8),
    border: (u8, u8, u8),
    fg: (u8, u8, u8),
    soft_fg: (u8, u8, u8),
    accent: (u8, u8, u8),
) -> ThemePalette {
    ThemePalette {
        bg: capability.color(bg, 235, 0),
        surface: capability.color(surface, 236, 0),
        border: capability.color(border, 240, 8),
        fg: capability.color(fg, 255, 15),
        soft_fg: capability.color(soft_fg, 245, 7),
        accent: capability.color(accent, 113, 10),
        add: capability.color((120, 200, 120), 78, 2),
        del: capability.color((240, 110, 110), 203, 1),
        queued: capability.color((145, 150, 165), 245, 7),
        running: capability.color((100, 180, 255), 75, 12),
        waiting: capability.color((245, 190, 80), 178, 11),
        review: capability.color((190, 130, 245), 141, 13),
        done: capability.color((120, 205, 135), 78, 2),
        failed: capability.color((245, 105, 105), 203, 1),
        cancelled: capability.color((125, 130, 140), 243, 8),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_named_themes() {
        assert_eq!(ThemeName::parse("dark"), Some(ThemeName::Dark));
        assert_eq!(ThemeName::parse("lazy-vim"), Some(ThemeName::LazyVim));
        assert_eq!(ThemeName::parse("lakes-and-light"), Some(ThemeName::Lakes));
        assert_eq!(ThemeName::parse("system"), None);
    }

    #[test]
    fn lakes_uses_the_source_parchment_and_umber_colors() {
        let theme = Theme::new(ThemeName::Lakes, ColorCapability::TrueColor);
        assert_eq!(theme.palette.bg, Color::Rgb(245, 228, 216));
        assert_eq!(theme.palette.fg, Color::Rgb(58, 25, 17));
        assert_eq!(theme.palette.accent, Color::Rgb(128, 95, 54));
    }

    #[test]
    fn detects_lazyvim_as_the_default_theme() {
        assert_eq!(Theme::detect().name, ThemeName::LazyVim);
    }

    #[test]
    fn capability_changes_color_representation() {
        assert!(matches!(
            Theme::new(ThemeName::Dark, ColorCapability::TrueColor)
                .palette
                .bg,
            Color::Rgb(..)
        ));
        assert!(matches!(
            Theme::new(ThemeName::Dark, ColorCapability::Ansi256)
                .palette
                .bg,
            Color::Indexed(235)
        ));
    }
}
