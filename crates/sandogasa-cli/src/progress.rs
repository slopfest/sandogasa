// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A one-line progress counter on stderr, for a phase that works
//! through a known number of items and would otherwise look hung.
//!
//! The line reads `comparing provides  7/39: evolution-data-server`,
//! rewritten in place as each item starts. The count is padded to the
//! width of the total, so the colon stays in one column while the
//! count grows. It is drawn only when stderr is a terminal: a log or a
//! pipe gets nothing, and a verbose run, which prints a line per step,
//! should not draw it either, as the two would overwrite each other.
//! [`Progress::tick`] can be called from parallel workers.

use std::io::{IsTerminal, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The widest line drawn, so it never wraps on an 80-column terminal:
/// a wrapped line cannot be rewritten in place.
pub const WIDTH: usize = 79;

/// A progress counter for one phase. The line is cleared when it is
/// dropped, so an early return leaves no stale counter behind.
pub struct Progress {
    label: String,
    total: usize,
    started: AtomicUsize,
    draw: bool,
    out: Mutex<()>,
}

/// The counter's line: the label, the count padded to the total's
/// width, and the item, cut to `width` characters.
pub fn line(label: &str, n: usize, total: usize, item: &str, width: usize) -> String {
    let digits = total.to_string().len();
    let full = format!("{label} {n:>digits$}/{total}: {item}");
    full.chars().take(width).collect()
}

impl Progress {
    /// A counter that draws only when `draw` is true.
    pub fn new(label: &str, total: usize, draw: bool) -> Self {
        Progress {
            label: label.to_string(),
            total,
            started: AtomicUsize::new(0),
            draw: draw && total > 0,
            out: Mutex::new(()),
        }
    }

    /// A counter on stderr: drawn when stderr is a terminal, unless
    /// `verbose` output is printing its own lines.
    pub fn stderr(label: &str, total: usize, verbose: bool) -> Self {
        Self::new(label, total, !verbose && std::io::stderr().is_terminal())
    }

    /// Count `item` as started, and redraw the line.
    pub fn tick(&self, item: &str) {
        let n = self.started.fetch_add(1, Ordering::Relaxed) + 1;
        if !self.draw {
            return;
        }
        let shown = line(&self.label, n.min(self.total), self.total, item, WIDTH);
        let _guard = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "\r{shown}\x1b[K");
        let _ = err.flush();
    }

    /// Print `message` on its own line, clearing the counter first so
    /// the two do not run together; the next tick redraws the counter.
    pub fn eprintln(&self, message: &str) {
        let _guard = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let mut err = std::io::stderr().lock();
        if self.draw && self.started() > 0 {
            let _ = write!(err, "\r\x1b[K");
        }
        let _ = writeln!(err, "{message}");
    }

    /// How many items have started.
    pub fn started(&self) -> usize {
        self.started.load(Ordering::Relaxed)
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if self.draw && self.started() > 0 {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r\x1b[K");
            let _ = err.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_count_is_padded_so_the_colon_stays_put() {
        let a = line("comparing provides", 7, 39, "libical", WIDTH);
        let b = line("comparing provides", 23, 39, "evolution-data-server", WIDTH);
        assert_eq!(a, "comparing provides  7/39: libical");
        assert_eq!(b, "comparing provides 23/39: evolution-data-server");
        assert_eq!(a.find(':'), b.find(':'));
        assert_eq!(line("x", 5, 100, "y", WIDTH), "x   5/100: y");
    }

    #[test]
    fn a_long_line_is_cut_so_it_never_wraps() {
        let item = "a".repeat(200);
        assert_eq!(line("checking", 1, 9, &item, WIDTH).chars().count(), WIDTH);
    }

    #[test]
    fn ticks_count_from_any_thread_without_drawing() {
        let p = Progress::new("working", 40, false);
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| (0..10).for_each(|_| p.tick("item")));
            }
        });
        assert_eq!(p.started(), 40);
        // Nothing to count: nothing drawn either way.
        assert!(!Progress::new("idle", 0, true).draw);
    }
}
