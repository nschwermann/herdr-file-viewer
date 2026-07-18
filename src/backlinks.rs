//! Backlinks state — the ephemeral state of the "what links here" overlay (the `G` key).
//!
//! [`BacklinksState`] holds the notes that link *to* the current one (already computed by the
//! controller as a reverse lookup over the cached [`crate::vault_index::VaultIndex`]) as a flat
//! list plus a cursor, closely mirroring [`crate::linknav::LinkNavState`] and
//! [`crate::outline::OutlineState`]. It is the inbound mirror of the `g` wikilink navigator: `g`
//! lists the links *out* of the note, `G` lists the notes that link *in*. There is no query, no
//! fuzzy matching, and no resolved/unresolved distinction — every backlink is a real vault note —
//! just a list the user moves a cursor over and confirms to open that note in-viewer.
//!
//! Pure (no I/O): the controller builds the vault index, runs the reverse lookup, and passes the
//! finished [`BacklinkItem`]s in. This module only holds the result and drives the cursor.

use std::path::PathBuf;

/// One note that links to the current one, as shown in the backlinks overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklinkItem {
    /// The linking note's human label — its vault-relative path with the `.md` extension dropped
    /// (from [`crate::vault_index::NoteEntry::display`]), the same row text the quick-switcher uses.
    pub display: String,
    /// The linking note's **absolute** path, so confirming a row opens it in-viewer.
    pub path: PathBuf,
}

/// Live state of the backlinks overlay while it is open.
///
/// Created by [`crate::controller::Controller::open_backlinks`] when the user presses `G` on a
/// markdown note inside an Obsidian vault that has at least one backlink, and destroyed when they
/// open a backlink or cancel.
pub struct BacklinksState {
    /// The linking notes, in the vault index's walk order (stable per build).
    items: Vec<BacklinkItem>,
    /// Cursor position within `items`. Driven by the run loop; clamped to the list.
    cursor: usize,
}

impl BacklinksState {
    /// Build a new `BacklinksState` over the given linking-note items, cursor at 0.
    pub fn new(items: Vec<BacklinkItem>) -> Self {
        Self { items, cursor: 0 }
    }

    /// The linking-note items, in walk order. Exposed for the Presenter projection and tests.
    pub fn items(&self) -> &[BacklinkItem] {
        &self.items
    }

    /// The cursor position within the item list.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether there are no backlinks at all (the controller does not open the overlay in that
    /// case, but the predicate keeps the state total).
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
    pub fn selected(&self) -> Option<&BacklinkItem> {
        self.items.get(self.cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(display: &str, path: &str) -> BacklinkItem {
        BacklinkItem {
            display: display.to_string(),
            path: PathBuf::from(path),
        }
    }

    fn state() -> BacklinksState {
        BacklinksState::new(vec![
            item("A", "/vault/A.md"),
            item("sub/B", "/vault/sub/B.md"),
            item("C", "/vault/C.md"),
        ])
    }

    #[test]
    fn new_starts_at_cursor_zero_and_selects_first() {
        let s = state();
        assert_eq!(s.cursor(), 0);
        assert_eq!(s.selected().unwrap().display, "A");
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
        assert_eq!(s.selected().unwrap().display, "C");
        s.move_selection(-10); // past the top → first row
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn set_cursor_clamps_to_the_list() {
        let mut s = state();
        s.set_cursor(1);
        assert_eq!(s.cursor(), 1);
        assert_eq!(s.selected().unwrap().display, "sub/B");
        assert_eq!(s.selected().unwrap().path, PathBuf::from("/vault/sub/B.md"));
        s.set_cursor(99);
        assert_eq!(s.cursor(), 2, "clamped to the last row");
    }

    #[test]
    fn empty_state_is_inert() {
        let mut s = BacklinksState::new(Vec::new());
        assert!(s.is_empty());
        assert_eq!(s.selected(), None);
        s.move_selection(1);
        assert_eq!(s.cursor(), 0);
        s.set_cursor(3);
        assert_eq!(s.cursor(), 0);
    }
}
