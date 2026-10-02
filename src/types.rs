//! Cross-cutting value types shared by every module.

use std::collections::BTreeSet;
use std::fmt;

/// A rectangle in top-left-origin screen coordinates (pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Geometry helpers: some are only used by the macOS platform layer and the
/// layout tests, so a non-macOS build would otherwise warn.
#[allow(dead_code)]
impl Rect {
    #[must_use]
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    #[must_use]
    pub const fn right(self) -> i32 {
        self.x + self.width
    }

    #[must_use]
    pub const fn bottom(self) -> i32 {
        self.y + self.height
    }

    #[must_use]
    pub const fn center_x(self) -> f64 {
        self.x as f64 + self.width as f64 / 2.0
    }

    #[must_use]
    pub const fn center_y(self) -> f64 {
        self.y as f64 + self.height as f64 / 2.0
    }

    #[must_use]
    pub const fn contains_point(self, x: f64, y: f64) -> bool {
        self.x as f64 <= x
            && x <= self.right() as f64
            && self.y as f64 <= y
            && y <= self.bottom() as f64
    }

    #[must_use]
    pub fn intersection_area(self, other: Self) -> i64 {
        let width = (self.right().min(other.right()) - self.x.max(other.x)).max(0);
        let height = (self.bottom().min(other.bottom()) - self.y.max(other.y)).max(0);
        i64::from(width) * i64::from(height)
    }

    #[must_use]
    pub fn distance_to(self, other: Self) -> f64 {
        (self.center_x() - other.center_x()).abs() + (self.center_y() - other.center_y()).abs()
    }

    #[must_use]
    pub fn as_key(self) -> String {
        format!("{},{},{},{}", self.x, self.y, self.width, self.height)
    }
}

/// The four directions a focus/move command can act in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// Lowercase name used on the wire and in the CLI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
            Self::Down => "down",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            _ => None,
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Keyboard modifiers mwm understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Modifier {
    Cmd,
    Ctrl,
    Alt,
    Shift,
}

#[allow(dead_code)]
impl Modifier {
    /// Canonical lowercase name used in chord strings and JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cmd => "cmd",
            Self::Ctrl => "ctrl",
            Self::Alt => "alt",
            Self::Shift => "shift",
        }
    }

    /// Accepts `cmd/cmd_l/cmd_r`, `ctrl/...`, `alt/option`, `shift/...`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let normalised = value.trim().to_ascii_lowercase();
        Self::parse_normalised(&normalised)
    }

    fn parse_normalised(normalised: &str) -> Option<Self> {
        match normalised {
            "cmd" | "cmd_l" | "cmd_r" | "command" => Some(Self::Cmd),
            "ctrl" | "ctrl_l" | "ctrl_r" | "control" => Some(Self::Ctrl),
            "alt" | "alt_l" | "alt_r" | "option" => Some(Self::Alt),
            "shift" | "shift_l" | "shift_r" => Some(Self::Shift),
            _ => None,
        }
    }
}

/// A set of modifiers, kept sorted for cheap comparison.
pub type ModifierSet = BTreeSet<Modifier>;

/// One attached display, in top-left-origin coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenInfo {
    /// Stable identity for the layout state (geometry-derived).
    pub key: String,
    pub frame: Rect,
}

/// One manageable window on some screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// Stable identity for the layout state (`pid:window_number` or similar).
    pub key: String,
    pub pid: i32,
    pub title: String,
    pub frame: Rect,
    pub screen_key: String,
    /// Sort order used to break ties when picking representatives.
    pub order: u64,
}
