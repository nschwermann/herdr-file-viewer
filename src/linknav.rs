//! Link-navigator state — the ephemeral state of the inline wikilink overlay (the `g` key).
//!
//! [`LinkNavState`] holds the note's followable links (already parsed and resolved by the
//! controller) as a flat list plus a cursor, closely mirroring [`crate::finder::FinderState`] but
//! much simpler: there is no query, no fuzzy matching, and no horizontal scroll — just a list the
//! user moves a cursor over and confirms. Construction takes the pre-computed items; the cursor
//! starts at 0 and is driven by the run loop (`j`/`k`/arrows and confirm).
//!
//! Pure (no I/O): the controller does the file reads (parse the source, build the vault index,
//! resolve each target) and passes the finished [`LinkItem`]s in. Resolution of a wikilink target
//! to a note is [`crate::obsidian::resolve_target`]'s job; this module only holds the result.

use crate::wikilink::Anchor;
use std::path::PathBuf;

/// One followable link in the note, as shown in the navigator overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkItem {
    /// The human label for the row — the link's alias, else its target (+ anchor). From
    /// [`crate::wikilink::Link::display`].
    pub display: String,
    /// The resolved **absolute** path of the target note, or `None` when the target resolves to no
    /// note in the vault (an unresolved link — shown as such, never followed).
    pub resolved: Option<PathBuf>,
    /// The link's in-note anchor (`#Heading` / `#^block`), carried so following the link can scroll
    /// the freshly-opened note to it (best-effort).
    pub anchor: Option<Anchor>,
}

/// Live state of the wikilink navigator overlay while it is open.
///
/// Created by [`crate::controller::Controller::open_link_nav`] when the user presses `g` on a
/// markdown note inside an Obsidian vault, and destroyed when they follow a link or cancel.
pub struct LinkNavState {
    /// The note's links, in source order.
    items: Vec<LinkItem>,
    /// Cursor position within `items`. Driven by the run loop; clamped to the list.
    cursor: usize,
}

impl LinkNavState {
    /// Build a new `LinkNavState` over the given (source-ordered) link items, cursor at 0.
    pub fn new(items: Vec<LinkItem>) -> Self {
        Self { items, cursor: 0 }
    }

    /// The link items, in source order. Exposed for the Presenter projection and tests.
    pub fn items(&self) -> &[LinkItem] {
        &self.items
    }

    /// The cursor position within the item list.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether there are no links at all (the controller does not open the overlay in that case,
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
    pub fn selected(&self) -> Option<&LinkItem> {
        self.items.get(self.cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(display: &str, resolved: Option<&str>) -> LinkItem {
        LinkItem {
            display: display.to_string(),
            resolved: resolved.map(PathBuf::from),
            anchor: None,
        }
    }

    fn state() -> LinkNavState {
        LinkNavState::new(vec![
            item("A", Some("/vault/A.md")),
            item("B", None),
            item("C", Some("/vault/sub/C.md")),
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
        assert_eq!(s.selected().unwrap().display, "B");
        assert!(s.selected().unwrap().resolved.is_none(), "B is unresolved");
        s.set_cursor(99);
        assert_eq!(s.cursor(), 2, "clamped to the last row");
    }

    #[test]
    fn empty_state_is_inert() {
        let mut s = LinkNavState::new(Vec::new());
        assert!(s.is_empty());
        assert_eq!(s.selected(), None);
        s.move_selection(1);
        assert_eq!(s.cursor(), 0);
        s.set_cursor(3);
        assert_eq!(s.cursor(), 0);
    }
}
