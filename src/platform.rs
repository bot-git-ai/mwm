//! The seam between pure mwm logic and the operating system.
//!
//! Everything above this trait is testable on any OS; the Darwin
//! implementation lives in `platform_darwin.rs` (compiled only on macOS),
//! and `platform_stub.rs` covers every other target so the crate builds and
//! its logic tests run anywhere.

use std::collections::BTreeSet;

use crate::types::{Direction, Modifier, Rect, ScreenInfo, WindowInfo};

/// Result of querying the window system. The daemon treats errors as
/// "nothing changed" and carries on; the pure logic never sees them.
pub type QueryResult<T> = Result<T, ()>;

/// Operations the daemon needs from the window system.
pub trait WindowSystem {
    /// True when this process has the Accessibility permission macOS
    /// requires for reading and setting window geometry.
    fn accessibility_trusted(&self) -> bool;
    /// Prompt the user for the permission if it is missing.
    fn prompt_for_accessibility(&self) -> bool;

    /// Re-read the window system and drop handles for windows that are gone.
    /// Called before every layout pass, because window handles are only as
    /// good as the last refresh.
    fn refresh(&mut self);

    /// All attached screens, top-left origin.
    fn screens(&self) -> QueryResult<Vec<ScreenInfo>>;
    /// All manageable windows, ordered left-to-right, top-to-bottom.
    fn windows(&self) -> QueryResult<Vec<WindowInfo>>;
    /// The focused window, if any.
    fn focused_window(&self) -> Option<WindowInfo>;

    /// Move and resize a window to an exact frame.
    fn set_frame(&self, window: &WindowInfo, frame: Rect) -> bool;
    /// Bring a window to the front and focus it.
    fn focus_window(&self, window: &WindowInfo) -> bool;
    /// Ask the window's close button to close it.
    fn close_window(&self, window: &WindowInfo) -> bool;
    /// Switch to another desktop (Space) by number.
    fn switch_desktop(&self, desktop: u8) -> bool;

    /// Start listening for window-list changes; call `on_change` on the
    /// daemon's main thread when windows appear/move/resize/disappear.
    fn watch_windows(&mut self, on_change: Box<dyn FnMut() + Send>) -> bool;
    /// Stop watching; safe to call more than once.
    fn unwatch_windows(&mut self);
    /// True while a keyboard hook is installed that can observe and consume
    /// key presses. When false the daemon still serves IPC but has no
    /// keybindings. The callback returns true to consume the key press.
    fn watch_keys(&mut self, on_key: Box<dyn FnMut(KeyEvent) -> bool + Send>) -> bool;
    /// Remove the keyboard hook.
    fn unwatch_keys(&mut self);
}

/// A key press observed by the hook, already normalised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    /// The modifiers held during the press.
    pub modifiers: BTreeSet<Modifier>,
    /// The main key, normalised to lowercase (a-z, 0-9, arrow names, `vk:NN`).
    pub key: KeyName,
}

/// Normalised key names.
///
/// Only the macOS platform layer builds these; on other targets they are
/// part of the seam but never constructed, so do not warn about it there.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyName {
    Letter(char),
    Digit(u8),
    Arrow(Direction),
    Special(&'static str),
}

impl KeyName {
    /// Canonical string form used in chord strings and JSON.
    #[must_use]
    pub fn as_str(&self) -> String {
        match self {
            Self::Letter(letter) => letter.to_string(),
            Self::Digit(digit) => digit.to_string(),
            Self::Arrow(direction) => direction.as_str().to_string(),
            Self::Special(name) => (*name).to_string(),
        }
    }
}

/// Arrow-key codes on macOS (the `HIToolbox` virtual key codes), used by the macOS
/// macOS key hook.
#[allow(dead_code)]
pub mod darwin_key_codes {
    pub const LEFT: u8 = 0x7B;
    pub const RIGHT: u8 = 0x7C;
    pub const DOWN: u8 = 0x7D;
    pub const UP: u8 = 0x7E;
}

#[cfg(test)]
mod tests {
    use super::{darwin_key_codes, KeyEvent, KeyName};
    use crate::types::{Direction, Modifier};
    use std::collections::BTreeSet;

    #[test]
    fn key_names_render_canonically() {
        assert_eq!(KeyName::Letter('h').as_str(), "h");
        assert_eq!(KeyName::Digit(0).as_str(), "0");
        assert_eq!(KeyName::Arrow(Direction::Left).as_str(), "left");
        assert_eq!(KeyName::Arrow(Direction::Down).as_str(), "down");
        assert_eq!(KeyName::Special("space").as_str(), "space");
    }

    #[test]
    fn key_events_carry_their_modifiers() {
        let event = KeyEvent {
            modifiers: BTreeSet::from([Modifier::Alt, Modifier::Shift]),
            key: KeyName::Letter('q'),
        };
        assert_eq!(event.key.as_str(), "q");
        assert!(event.modifiers.contains(&Modifier::Alt));
        assert!(event.modifiers.contains(&Modifier::Shift));
    }

    #[test]
    fn arrow_key_codes_are_the_macos_ones() {
        assert_eq!(darwin_key_codes::LEFT, 0x7B);
        assert_eq!(darwin_key_codes::RIGHT, 0x7C);
        assert_eq!(darwin_key_codes::DOWN, 0x7D);
        assert_eq!(darwin_key_codes::UP, 0x7E);
    }
}
