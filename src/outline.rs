//! Heading-outline state — the ephemeral state of the jump-through outline overlay (the `o` key).
//!
//! [`OutlineState`] holds the current markdown note's headings (already parsed by the controller
//! via [`crate::mdnote::headings`]) as a flat list plus a cursor, closely mirroring
//! [`crate::linknav::LinkNavState`] but simpler still: there is no resolution, no anchor, no
//! back-stack — just a list the user moves a cursor over and confirms to scroll the content pane to
//! that heading's source line. Construction takes the pre-computed items; the cursor starts at 0 and
//! is driven by the run loop (`j`/`k`/arrows and confirm).
//!
//! Pure (no I/O): the controller reads the note and parses its headings, then passes the finished
//! [`OutlineItem`]s in. This module only holds the result.

/// One heading in the note's outline, as shown in the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineItem {
    /// ATX heading level 1..=6 — drives the row's indent (level-1 steps).
    pub level: u8,
    /// The heading text (markers stripped, trimmed), from [`crate::mdnote::Heading::text`].
    pub text: String,
    /// The heading's 1-based source line, carried so confirming scrolls the note to it.
    pub line: usize,
}

/// Live state of the heading-outline overlay while it is open.
///
/// Created by [`crate::controller::Controller::open_outline`] when the user presses `o` on a
/// markdown note, and destroyed when they jump to a heading or cancel.
pub struct OutlineState {
    /// The note's headings, in source order.
    items: Vec<OutlineItem>,
    /// Cursor position within `items`. Driven by the run loop; clamped to the list.
    cursor: usize,
}

impl OutlineState {
    /// Build a new `OutlineState` over the given (source-ordered) heading items, cursor at 0.
    pub fn new(items: Vec<OutlineItem>) -> Self {
        Self { items, cursor: 0 }
    }

    /// The heading items, in source order. Exposed for the Presenter projection and tests.
    pub fn items(&self) -> &[OutlineItem] {
        &self.items
    }

    /// The cursor position within the item list.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether there are no headings at all (the controller does not open the overlay in that case,
    /// but the predicate keeps the state total).
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Move the cursor by `delta` rows, clamped to `[0, items.len()-1]` so it never runs off either
    /// end. A no-op (cursor stays 0) when the list is empty.
    pub fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            self.cursor = 0;
            return;
        }
        let max = self.items.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
    }

    /// Set the cursor to `idx`, clamped to `[0, items.len()-1]`. A no-op when the list is empty.
    pub fn set_cursor(&mut self, idx: usize) {
        if self.items.is_empty() {
            self.cursor = 0;
            return;
        }
        self.cursor = idx.min(self.items.len() - 1);
    }

    /// The item at the current cursor position, or `None` when the list is empty.
    pub fn selected(&self) -> Option<&OutlineItem> {
        self.items.get(self.cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(level: u8, text: &str, line: usize) -> OutlineItem {
        OutlineItem {
            level,
            text: text.to_string(),
            line,
        }
    }

    fn state() -> OutlineState {
        OutlineState::new(vec![
            item(1, "One", 1),
            item(2, "Two", 4),
            item(3, "Three", 6),
        ])
    }

    #[test]
    fn new_starts_at_cursor_zero_and_selects_first() {
        let s = state();
        assert_eq!(s.cursor(), 0);
        assert_eq!(s.selected().unwrap().text, "One");
        assert!(!s.is_empty());
    }

    #[test]
    fn move_selection_clamps_at_both_ends() {
        let mut s = state();
        s.move_selection(-1); // already at the top → stays
        assert_eq!(s.cursor(), 0);
        s.move_selection(1);
        assert_eq!(s.cursor(), 1);
        s.move_selection(10); // past the end → last row
        assert_eq!(s.cursor(), 2);
        assert_eq!(s.selected().unwrap().text, "Three");
        assert_eq!(s.selected().unwrap().line, 6);
        s.move_selection(-10); // past the top → first row
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn set_cursor_clamps_to_the_list() {
        let mut s = state();
        s.set_cursor(1);
        assert_eq!(s.cursor(), 1);
        assert_eq!(s.selected().unwrap().text, "Two");
        assert_eq!(s.selected().unwrap().level, 2);
        s.set_cursor(99);
        assert_eq!(s.cursor(), 2, "clamped to the last row");
    }

    #[test]
    fn empty_state_is_inert() {
        let mut s = OutlineState::new(Vec::new());
        assert!(s.is_empty());
        assert_eq!(s.selected(), None);
        s.move_selection(1);
        assert_eq!(s.cursor(), 0);
        s.set_cursor(3);
        assert_eq!(s.cursor(), 0);
    }
}
