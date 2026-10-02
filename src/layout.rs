//! The n-column layout: pure geometry and bookkeeping, no system calls.
//!
//! Windows live in a per-screen list of columns; each column is a stack of
//! rows. `columns: 2.5` means at most two full columns plus a fractional
//! tail: the first `ceil(n)-1` columns each take `1/n` of the width and the
//! last one takes the remainder.

use std::collections::{BTreeMap, BTreeSet};

use crate::types::{Direction, Rect, ScreenInfo, WindowInfo};

/// Upper bound on a usable column count; beyond this a request is a mistake
/// and "as many as fit" is the safe reading.
const MAX_USABLE_COLUMNS: usize = 64;

/// Smallest window we are willing to produce or accept as real.
pub const MIN_WINDOW_WIDTH: i32 = 40;
pub const MIN_WINDOW_HEIGHT: i32 = 40;

/// Layout configuration validated at use sites.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutConfig {
    /// Number of columns as a fraction; at least 1.0.
    pub columns: f64,
}

impl LayoutConfig {
    #[must_use]
    pub const fn new(columns: f64) -> Self {
        Self { columns }
    }

    /// Largest number of on-screen columns this config ever creates.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn max_column_count(self) -> usize {
        if !self.columns.is_finite() || self.columns < 1.0 {
            1
        } else {
            // Clamped before converting: a column count that large is not a
            // real request, and "as many as fit" is the safe reading.
            let limit = f64::from(i32::try_from(MAX_USABLE_COLUMNS).unwrap_or(i32::MAX));
            let count = self.columns.ceil().min(limit) as usize;
            count.max(1)
        }
    }

    /// Validity used by the CLI/IPC boundary.
    #[must_use]
    pub fn is_valid(self) -> bool {
        self.columns.is_finite() && self.columns >= 1.0
    }
}

/// Floor one pixel share to `i32`; every share is within a screen width.
#[allow(clippy::cast_possible_truncation)]
fn floor_to_i32(share: f64) -> i32 {
    share.floor() as i32
}

/// Split `total` pixels proportionally to `weights`, preserving the total.
pub fn partition_i32(total: i32, weights: &[f64]) -> Vec<i32> {
    let count = weights.len();
    if count == 0 {
        return Vec::new();
    }
    let weight_sum: f64 = weights.iter().sum();
    if weight_sum <= 0.0 || !weight_sum.is_finite() {
        let count_i32 = i32::try_from(count).unwrap_or(i32::MAX);
        let even = total / count_i32;
        let mut sizes = vec![even; count];
        let mut remainder = total - even * count_i32;
        let mut index = 0;
        while remainder > 0 {
            sizes[index % count] += 1;
            remainder -= 1;
            index += 1;
        }
        return sizes;
    }
    // Pixel arithmetic: `total` is a screen dimension and every share of it
    // is within i32, so these casts cannot lose a meaningful value.
    let raw: Vec<f64> = weights
        .iter()
        .map(|weight| f64::from(total) * weight / weight_sum)
        .collect();
    let mut sizes: Vec<i32> = raw.iter().copied().map(floor_to_i32).collect();
    let mut remainder = total - sizes.iter().sum::<i32>();
    // largest fractional part first, ties to the lower index
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by(|&a, &b| {
        let fa = raw[a].fract();
        let fb = raw[b].fract();
        fb.partial_cmp(&fa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    let mut cursor = 0;
    while remainder > 0 {
        sizes[order[cursor % count]] += 1;
        remainder -= 1;
        cursor += 1;
    }
    while remainder < 0 {
        sizes[order[(count - 1) - (cursor % count)]] -= 1;
        remainder += 1;
        cursor += 1;
    }
    sizes
}

/// Split `frame` horizontally into `count` column frames following the
/// fractional `columns` semantics: each of the first `count-1` columns
/// takes weight `1/columns`, the last takes the remainder.
#[must_use]
pub fn split_columns(frame: Rect, count: usize, columns: f64) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![frame];
    }
    let slot = 1.0 / columns;
    let mut weights = vec![slot; count - 1];
    // `count` is the number of columns on screen: a handful, never 2^53.
    #[allow(clippy::cast_precision_loss)]
    let count_f64 = count as f64;
    weights.push((1.0 - slot * (count_f64 - 1.0)).max(0.001));
    let widths = partition_i32(frame.width, &weights);
    let mut x = frame.x;
    widths
        .into_iter()
        .map(|width| {
            let rect = Rect::new(x, frame.y, width, frame.height);
            x += width;
            rect
        })
        .collect()
}

/// Split `frame` vertically into `count` row frames, optionally weighted.
#[must_use]
pub fn split_rows(frame: Rect, count: usize, weights: Option<&[f64]>) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![frame];
    }
    let usable = weights.filter(|weights| {
        weights.len() == count && weights.iter().all(|w| *w > 0.0 && w.is_finite())
    });
    let weights: Vec<f64> = match usable {
        Some(weights) => weights.to_vec(),
        None => vec![1.0; count],
    };
    let heights = partition_i32(frame.height, &weights);
    let mut y = frame.y;
    heights
        .into_iter()
        .map(|height| {
            let rect = Rect::new(frame.x, y, frame.width, height);
            y += height;
            rect
        })
        .collect()
}

#[cfg(test)]
mod tests_partition {
    use super::partition_i32;

    #[test]
    fn thirds_of_ten() {
        assert_eq!(partition_i32(10, &[1.0, 1.0, 1.0]), vec![4, 3, 3]);
    }

    #[test]
    fn forty_forty_twenty() {
        assert_eq!(partition_i32(100, &[0.4, 0.4, 0.2]), vec![40, 40, 20]);
    }

    #[test]
    fn preserves_total_on_odd_weights() {
        let sizes = partition_i32(1000, &[0.3, 0.3, 0.25, 0.15]);
        assert_eq!(sizes.iter().sum::<i32>(), 1000);
        assert_eq!(sizes, vec![300, 300, 250, 150]);
    }

    #[test]
    fn degenerate_weights_fall_back_to_even() {
        assert_eq!(partition_i32(10, &[0.0, 0.0]), vec![5, 5]);
    }
}

/// Bookkeeping for the layout: where every window sits, which are
/// fullscreened, and remembered row heights.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LayoutState {
    /// Per screen: ordered columns of window keys.
    pub columns_by_screen: BTreeMap<String, Vec<Vec<String>>>,
    /// Windows the user fullscreened through mwm.
    pub fullscreen_keys: BTreeSet<String>,
    /// Row height weights remembered from user resizing, by window key.
    pub row_weights_by_key: BTreeMap<String, f64>,
}

impl LayoutState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop state for windows and screens that no longer exist.
    pub fn keep_only(
        &mut self,
        visible_keys: &BTreeSet<String>,
        visible_screen_keys: &BTreeSet<String>,
    ) {
        self.fullscreen_keys
            .retain(|key| visible_keys.contains(key));
        self.row_weights_by_key
            .retain(|key, _| visible_keys.contains(key));
        self.columns_by_screen
            .retain(|screen_key, _| visible_screen_keys.contains(screen_key));
        for columns in self.columns_by_screen.values_mut() {
            for column in columns.iter_mut() {
                column.retain(|key| visible_keys.contains(key));
            }
            columns.retain(|column| !column.is_empty());
        }
    }

    /// Where a window currently sits: (screen key, column index, row index).
    #[must_use]
    pub fn find(&self, key: &str) -> Option<(String, usize, usize)> {
        for (screen_key, columns) in &self.columns_by_screen {
            for (column_index, column) in columns.iter().enumerate() {
                if let Some(row_index) = column.iter().position(|k| k == key) {
                    return Some((screen_key.clone(), column_index, row_index));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests_split {
    use super::{split_columns, split_rows};
    use crate::types::Rect;

    fn frame() -> Rect {
        Rect::new(0, 0, 1000, 500)
    }

    #[test]
    fn fractional_columns_three_windows() {
        let widths: Vec<i32> = split_columns(frame(), 3, 2.5)
            .iter()
            .map(|r| r.width)
            .collect();
        assert_eq!(widths, vec![400, 400, 200]);
    }

    #[test]
    fn fractional_columns_two_windows() {
        let widths: Vec<i32> = split_columns(frame(), 2, 2.5)
            .iter()
            .map(|r| r.width)
            .collect();
        assert_eq!(widths, vec![400, 600]);
    }

    #[test]
    fn single_window_takes_all() {
        let rects = split_columns(frame(), 1, 2.5);
        assert_eq!(rects, vec![frame()]);
    }

    #[test]
    fn rows_equal_by_default() {
        let heights: Vec<i32> = split_rows(frame(), 3, None)
            .iter()
            .map(|r| r.height)
            .collect();
        assert_eq!(heights, vec![167, 167, 166]);
    }

    #[test]
    fn rows_weighted() {
        let heights: Vec<i32> = split_rows(frame(), 2, Some(&[3.0, 1.0]))
            .iter()
            .map(|r| r.height)
            .collect();
        assert_eq!(heights, vec![375, 125]);
    }

    #[test]
    fn rows_unusable_weights_fall_back_to_even() {
        let heights: Vec<i32> = split_rows(frame(), 2, Some(&[0.0, 1.0]))
            .iter()
            .map(|r| r.height)
            .collect();
        assert_eq!(heights, vec![250, 250]);
    }
}

/// The layout engine: applies the column model to real window sets.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutEngine {
    /// Layout state: columns per screen, fullscreen set, row weights.
    pub state: LayoutState,
    /// Column configuration.
    pub config: LayoutConfig,
}

impl LayoutEngine {
    #[must_use]
    pub fn new(config: LayoutConfig) -> Self {
        Self {
            state: LayoutState::new(),
            config,
        }
    }

    /// Merge the observed windows into the screen's columns: drop gone
    /// keys, place new keys (new column while allowed, else append to the
    /// last column), and merge excess tail columns.
    pub fn reconcile(&mut self, screen_key: &str, windows: &[WindowInfo]) -> Vec<Vec<String>> {
        let visible_keys: Vec<String> = windows.iter().map(|w| w.key.clone()).collect();
        let visible_set: BTreeSet<String> = visible_keys.iter().cloned().collect();
        let window_count = visible_keys.len();
        let mut columns: Vec<Vec<String>> = self
            .state
            .columns_by_screen
            .get(screen_key)
            .cloned()
            .unwrap_or_default();
        for column in &mut columns {
            column.retain(|key| visible_set.contains(key));
        }
        columns.retain(|column| !column.is_empty());
        let known: BTreeSet<String> = columns.iter().flatten().cloned().collect();
        for key in visible_keys {
            if known.contains(&key) {
                continue;
            }
            if columns.is_empty() || columns.len() < self.config.max_column_count() {
                columns.push(vec![key]);
            } else {
                columns.last_mut().expect("checked non-empty").push(key);
            }
        }
        let target = window_count.min(self.config.max_column_count());
        while columns.len() > target {
            let extra = columns.pop().unwrap_or_default();
            columns.last_mut().expect("checked non-empty").extend(extra);
        }
        self.state
            .columns_by_screen
            .insert(screen_key.to_string(), columns.clone());
        columns
    }

    /// Frames every window on a screen should have right now.
    #[must_use]
    pub fn layout_targets(
        &self,
        screen: &ScreenInfo,
        columns: &[Vec<String>],
    ) -> BTreeMap<String, Rect> {
        let column_frames = split_columns(screen.frame, columns.len(), self.config.columns);
        let mut targets = BTreeMap::new();
        for (column, frame) in columns.iter().zip(column_frames) {
            let weights: Vec<f64> = self.column_weights(column);
            for (key, frame) in column
                .iter()
                .zip(split_rows(frame, column.len(), Some(&weights)))
            {
                targets.insert(key.clone(), frame);
            }
        }
        targets
    }

    /// Per-column row weights: a key's own remembered weight when valid,
    /// else the column's mean known weight (or equal split).
    fn column_weights(&self, column: &[String]) -> Vec<f64> {
        let known: Vec<f64> = column
            .iter()
            .filter_map(|key| self.state.row_weights_by_key.get(key))
            .filter(|w| **w > 0.0 && w.is_finite())
            .copied()
            .collect();
        let fallback = if known.is_empty() {
            1.0
        } else {
            // a column has a handful of rows
            #[allow(clippy::cast_precision_loss)]
            let row_count = known.len() as f64;
            known.iter().sum::<f64>() / row_count
        };
        column
            .iter()
            .map(|key| {
                self.state
                    .row_weights_by_key
                    .get(key)
                    .filter(|w| **w > 0.0 && w.is_finite())
                    .copied()
                    .unwrap_or(fallback)
            })
            .collect()
    }
}

impl LayoutEngine {
    /// Move a window within the grid, i3-style. Returns whether anything
    /// changed.
    pub fn move_window(&mut self, key: &str, direction: Direction) -> bool {
        let Some((screen_key, column_index, row_index)) = self.state.find(key) else {
            return false;
        };
        let mut columns = self
            .state
            .columns_by_screen
            .get(&screen_key)
            .cloned()
            .unwrap_or_default();
        let changed = match direction {
            Direction::Up => {
                if row_index == 0 {
                    false
                } else {
                    let column = &mut columns[column_index];
                    column.swap(row_index, row_index - 1);
                    true
                }
            }
            Direction::Down => {
                let column = &columns.get(column_index).cloned().unwrap_or_default();
                if row_index + 1 >= column.len() {
                    false
                } else {
                    let column = &mut columns[column_index];
                    column.swap(row_index, row_index + 1);
                    true
                }
            }
            Direction::Left | Direction::Right => {
                let forward = direction == Direction::Right;
                let last = columns.len().saturating_sub(1);
                let at_edge = if forward {
                    column_index >= last
                } else {
                    column_index == 0
                };
                if at_edge {
                    let source_len = columns.get(column_index).map_or(0, Vec::len);
                    let allowed = columns.len() < self.config.max_column_count() && source_len > 1;
                    if !allowed {
                        false
                    } else if forward {
                        columns.insert(column_index + 1, Vec::new());
                        true
                    } else {
                        columns.insert(0, Vec::new());
                        true
                    }
                } else {
                    true
                }
            }
        };
        if !changed {
            return false;
        }
        // For left/right moves the insert of a fresh column shifts indices.
        match direction {
            Direction::Left | Direction::Right => {
                let forward = direction == Direction::Right;
                let at_edge = if forward {
                    column_index + 1 >= columns.len()
                } else {
                    column_index == 0
                };
                let (source_index, target) = if !at_edge {
                    let target = if forward {
                        column_index + 1
                    } else {
                        column_index - 1
                    };
                    (column_index, target)
                } else if forward {
                    (column_index, column_index + 1)
                } else {
                    (column_index + 1, 0)
                };
                self.state.row_weights_by_key.remove(key);
                let source = &mut columns[source_index];
                let old_row = source.iter().position(|k| k == key).unwrap_or(0);
                source.retain(|k| k != key);
                let insert_at = old_row.min(columns.get(target).map_or(0, Vec::len));
                columns
                    .get_mut(target)
                    .expect("target exists")
                    .insert(insert_at, key.to_string());
            }
            Direction::Up | Direction::Down => {}
        }
        columns.retain(|column| !column.is_empty());
        self.state.columns_by_screen.insert(screen_key, columns);
        true
    }

    /// Toggle fullscreen for a key on a screen. Returns true when the
    /// window is now fullscreen.
    pub fn toggle_fullscreen(&mut self, key: &str, same_screen_keys: &[String]) -> bool {
        if self.state.fullscreen_keys.contains(key) {
            self.state.fullscreen_keys.remove(key);
            return false;
        }
        for other in same_screen_keys {
            self.state.fullscreen_keys.remove(other);
        }
        self.state.fullscreen_keys.insert(key.to_string());
        true
    }

    /// The window a focus command should move to, grid-first with a
    /// geometric fallback.
    #[must_use]
    pub fn focus_target(
        &self,
        key: &str,
        direction: Direction,
        windows: &[WindowInfo],
    ) -> Option<String> {
        let current = windows.iter().find(|w| w.key == key)?;
        let Some((screen_key, column_index, row_index)) = self.state.find(key) else {
            return geometric_focus_target(current, direction, windows).map(|w| w.key.clone());
        };
        let columns = self.state.columns_by_screen.get(&screen_key)?;
        match direction {
            Direction::Up | Direction::Down => {
                let column = columns.get(column_index)?;
                let target_row = if direction == Direction::Down {
                    row_index + 1
                } else {
                    row_index.checked_sub(1)?
                };
                column.get(target_row).cloned()
            }
            Direction::Left | Direction::Right => {
                let target_column = if direction == Direction::Right {
                    column_index + 1
                } else {
                    column_index.checked_sub(1)?
                };
                let column = columns.get(target_column)?;
                let nearest = column
                    .iter()
                    .map(|k| windows.iter().find(|w| &w.key == k))
                    .collect::<Option<Vec<_>>>()?;
                nearest
                    .into_iter()
                    .min_by(|a, b| {
                        let da = (a.frame.center_y() - current.frame.center_y()).abs();
                        let db = (b.frame.center_y() - current.frame.center_y()).abs();
                        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|w| w.key.clone())
            }
        }
    }
}

/// Nearest window purely by geometry, used when the grid has no opinion.
#[must_use]
fn geometric_focus_target<'a>(
    current: &WindowInfo,
    direction: Direction,
    windows: &'a [WindowInfo],
) -> Option<&'a WindowInfo> {
    let possible = windows.iter().filter(|w| match direction {
        Direction::Left => w.frame.center_x() < current.frame.center_x(),
        Direction::Right => w.frame.center_x() > current.frame.center_x(),
        Direction::Up => w.frame.center_y() < current.frame.center_y(),
        Direction::Down => w.frame.center_y() > current.frame.center_y(),
    });
    possible.min_by(|a, b| {
        let da = current.frame.distance_to(a.frame);
        let db = current.frame.distance_to(b.frame);
        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
    })
}

#[cfg(test)]
mod tests_state {
    use super::LayoutState;
    use std::collections::BTreeSet;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn keep_only_drops_gone_windows_screens_and_state() {
        let mut state = LayoutState::new();
        state.columns_by_screen.insert(
            "s".into(),
            vec![vec!["a".into(), "b".into()], vec!["c".into()]],
        );
        state.fullscreen_keys.insert("fs".into());
        state.row_weights_by_key.insert("b".into(), 3.0);
        state.keep_only(&set(&["a"]), &set(&["s"]));
        assert_eq!(state.columns_by_screen["s"], vec![vec!["a".to_string()]]);
        assert!(!state.fullscreen_keys.contains("fs"));
        assert!(!state.row_weights_by_key.contains_key("b"));
    }

    #[test]
    fn find_locates_and_misses() {
        let mut state = LayoutState::new();
        state
            .columns_by_screen
            .insert("s".into(), vec![vec!["a".into(), "b".into()]]);
        assert_eq!(state.find("b"), Some(("s".into(), 0, 1)));
        assert_eq!(state.find("zz"), None);
    }
}

#[cfg(test)]
mod tests_engine {
    use super::{LayoutConfig, LayoutEngine};
    use crate::types::{Direction, Rect, WindowInfo};

    fn window(key: &str, x: i32, y: i32) -> WindowInfo {
        WindowInfo {
            key: key.into(),
            pid: 1,
            title: String::new(),
            frame: Rect::new(x, y, 100, 100),
            screen_key: "s".into(),
            order: 0,
        }
    }

    fn engine(columns: f64) -> LayoutEngine {
        LayoutEngine::new(LayoutConfig::new(columns))
    }

    #[test]
    fn reconcile_places_new_windows_in_new_columns_until_full() {
        let mut engine = engine(2.0);
        let windows = [window("a", 0, 0), window("b", 0, 200), window("c", 500, 0)];
        let columns = engine.reconcile("s", &windows);
        // max_column_count(2.0) is 2 — the third window merges into the tail.
        assert_eq!(columns, vec![vec!["a"], vec!["b", "c"]]);
    }

    #[test]
    fn reconcile_appends_to_last_column_when_full() {
        let mut engine = engine(1.0);
        let windows = [window("a", 0, 0), window("b", 0, 200)];
        let columns = engine.reconcile("s", &windows);
        assert_eq!(columns, vec![vec!["a", "b"]]);
    }

    #[test]
    fn reconcile_merges_tail_when_windows_disappear() {
        let mut engine = engine(2.0);
        engine.reconcile(
            "s",
            &[window("a", 0, 0), window("b", 500, 0), window("c", 0, 200)],
        );
        // after the first reconcile with 3 windows we hold 2 columns
        let columns = engine.reconcile("s", &[window("a", 0, 0)]);
        assert_eq!(columns, vec![vec!["a"]]);
    }

    #[test]
    fn layout_targets_fractional_columns() {
        let mut engine = engine(2.5);
        let windows = [window("a", 0, 0), window("b", 400, 0), window("c", 800, 0)];
        let columns = engine.reconcile("s", &windows);
        let screen = crate::types::ScreenInfo {
            key: "s".into(),
            frame: Rect::new(0, 0, 1000, 500),
        };
        let targets = engine.layout_targets(&screen, &columns);
        assert_eq!(targets["a"].width, 400);
        assert_eq!(targets["b"].width, 400);
        assert_eq!(targets["c"].width, 200);
    }

    #[test]
    fn layout_targets_respects_remembered_row_weights() {
        let mut engine = engine(1.0);
        engine.reconcile("s", &[window("a", 0, 0), window("b", 0, 200)]);
        engine.state.row_weights_by_key.insert("a".into(), 3.0);
        engine.state.row_weights_by_key.insert("b".into(), 1.0);
        let screen = crate::types::ScreenInfo {
            key: "s".into(),
            frame: Rect::new(0, 0, 1000, 500),
        };
        let targets = engine.layout_targets(&screen, &[vec!["a".into(), "b".into()]]);
        assert_eq!(targets["a"].height, 375);
        assert_eq!(targets["b"].height, 125);
    }

    #[test]
    fn layout_targets_gives_new_rows_the_mean_of_known_weights() {
        let mut engine = engine(1.0);
        engine.reconcile("s", &[window("a", 0, 0), window("b", 0, 200)]);
        engine.state.row_weights_by_key.insert("a".into(), 3.0);
        let screen = crate::types::ScreenInfo {
            key: "s".into(),
            frame: Rect::new(0, 0, 1000, 500),
        };
        let targets = engine.layout_targets(&screen, &[vec!["a".into(), "b".into()]]);
        assert_eq!(targets["a"].height, 250);
        assert_eq!(targets["b"].height, 250);
    }

    #[test]
    fn move_window_swaps_rows_vertically() {
        let mut engine = engine(1.0);
        engine.reconcile("s", &[window("a", 0, 0), window("b", 0, 200)]);
        assert!(engine.move_window("b", Direction::Up));
        assert_eq!(
            engine.state.columns_by_screen["s"],
            vec![vec!["b".to_string(), "a".to_string()]]
        );
        assert!(!engine.move_window("b", Direction::Up));
    }

    #[test]
    fn move_window_moves_between_columns() {
        let mut engine = engine(2.0);
        engine.reconcile("s", &[window("a", 0, 0), window("b", 500, 0)]);
        assert!(engine.move_window("b", Direction::Left));
        assert_eq!(
            engine.state.columns_by_screen["s"],
            vec![vec!["b".to_string(), "a".to_string()]]
        );
    }

    #[test]
    fn move_window_creates_new_column_at_edge_when_allowed() {
        let mut engine = engine(2.0);
        // seed a two-row column with room for one more column
        engine
            .state
            .columns_by_screen
            .insert("s".into(), vec![vec!["a".into(), "b".into()]]);
        assert!(engine.move_window("a", Direction::Left));
        assert_eq!(
            engine.state.columns_by_screen["s"],
            vec![vec!["a".to_string()], vec!["b".to_string()]]
        );
    }

    #[test]
    fn move_window_creates_column_at_right_edge() {
        let mut engine = engine(2.0);
        engine
            .state
            .columns_by_screen
            .insert("s".into(), vec![vec!["a".into(), "b".into()]]);
        assert!(engine.move_window("b", Direction::Right));
        assert_eq!(
            engine.state.columns_by_screen["s"],
            vec![vec!["a".to_string()], vec!["b".to_string()]]
        );
    }

    #[test]
    fn move_window_refuses_new_column_when_source_sole() {
        let mut engine = engine(2.0);
        engine.reconcile("s", &[window("a", 0, 0)]);
        assert!(!engine.move_window("a", Direction::Left));
    }

    #[test]
    fn fullscreen_toggles_exclusively_per_screen() {
        let mut engine = engine(2.0);
        engine.reconcile("s", &[window("a", 0, 0), window("b", 500, 0)]);
        assert!(engine.toggle_fullscreen("a", &["a".into(), "b".into()]));
        assert!(engine.state.fullscreen_keys.contains("a"));
        assert!(engine.toggle_fullscreen("b", &["a".into(), "b".into()]));
        assert!(!engine.state.fullscreen_keys.contains("a"));
        assert!(engine.state.fullscreen_keys.contains("b"));
        assert!(!engine.toggle_fullscreen("b", &[]));
    }

    #[test]
    fn focus_target_walks_the_grid_and_falls_back() {
        let mut engine = engine(2.0);
        let windows = [window("a", 0, 0), window("b", 0, 200), window("c", 500, 0)];
        engine.reconcile("s", &windows);
        // grid after reconcile: [[a], [b, c]]
        assert_eq!(engine.focus_target("a", Direction::Down, &windows), None);
        assert_eq!(
            engine.focus_target("a", Direction::Right, &windows),
            Some("c".into())
        );
        assert_eq!(
            engine.focus_target("b", Direction::Down, &windows),
            Some("c".into())
        );
        assert_eq!(
            engine.focus_target("c", Direction::Up, &windows),
            Some("b".into())
        );
        assert_eq!(engine.focus_target("zz", Direction::Left, &windows), None);
    }
}
