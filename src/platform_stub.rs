//! Non-macOS stub used so the crate builds and pure tests run anywhere.
//! Every call fails or returns empty; the daemon refuses to start on it.

use std::collections::BTreeSet;

use crate::platform::{KeyEvent, KeyName, QueryResult, WindowSystem};
use crate::types::{ScreenInfo, WindowInfo};

/// A [`WindowSystem`] whose every operation fails.
#[derive(Debug, Default)]
pub struct StubWindowSystem;

impl StubWindowSystem {
    /// A stub that always reports "not macOS".
    pub fn new() -> Self {
        Self
    }
}

impl WindowSystem for StubWindowSystem {
    fn accessibility_trusted(&self) -> bool {
        false
    }

    fn prompt_for_accessibility(&self) -> bool {
        false
    }

    fn refresh(&mut self) {}

    fn screens(&self) -> QueryResult<Vec<ScreenInfo>> {
        Err(())
    }

    fn windows(&self) -> QueryResult<Vec<WindowInfo>> {
        Err(())
    }

    fn focused_window(&self) -> Option<WindowInfo> {
        None
    }

    fn set_frame(&self, _window: &WindowInfo, _frame: crate::types::Rect) -> bool {
        false
    }

    fn focus_window(&self, _window: &WindowInfo) -> bool {
        false
    }

    fn close_window(&self, _window: &WindowInfo) -> bool {
        false
    }

    fn switch_desktop(&self, _desktop: u8) -> bool {
        false
    }

    fn watch_windows(&mut self, _on_change: Box<dyn FnMut() + Send>) -> bool {
        false
    }

    fn unwatch_windows(&mut self) {}

    fn watch_keys(&mut self, _on_key: Box<dyn FnMut(KeyEvent) -> bool + Send>) -> bool {
        false
    }

    fn unwatch_keys(&mut self) {}
}

/// Keys never observed on the stub.
#[allow(dead_code)]
fn no_keys() -> BTreeSet<KeyName> {
    BTreeSet::new()
}
