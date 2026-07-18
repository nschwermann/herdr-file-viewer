//! Global content search state — the ephemeral state of the vault content-search overlay (`S`).
//!
//! Unlike the quick-switcher (which fuzzy-matches note *names* live as you type), a content search
//! runs an external grep over note *bodies*, so it is deliberately **two-phase**: you type a query,
//! press `Enter` to **run** the search, then navigate the resulting hits and `Enter` again to **open**
//! one. [`GlobalSearchState`] tracks the query buffer, the last run's [`crate::vsearch::SearchHit`]s
//! and cursor, and a `dirty` flag — set on every edit, cleared when a search runs — so the controller
//! knows whether the next `Enter` should re-run the search (query changed) or open the selected hit.
//!
//! Pure (no I/O): the controller runs the search (through the injected searcher) and feeds the hits
//! in via [`set_results`](GlobalSearchState::set_results); this module only holds them.

use crate::prompt::PromptInput;
use crate::vsearch::SearchHit;

/// Live state of the global content-search overlay while it is open.
///
/// Created by [`crate::controller::Controller::open_global_search`] when the user presses `S` inside
/// an Obsidian vault, and destroyed when they open a hit or cancel.
pub struct GlobalSearchState {
    /// The current query the user has typed.
    prompt: PromptInput,
    /// The hits from the most recent run (empty until the first `Enter`, or when a run found none).
    results: Vec<SearchHit>,
    /// Cursor position within `results`. Driven by the run loop; clamped to the list.
    cursor: usize,
    /// Whether a search has been run at least once (distinguishes "type and press Enter" from "no
    /// matches" in the overlay).
    searched: bool,
    /// Whether the query has been edited since the last run — so the next `Enter` re-runs the search
    /// rather than opening the highlighted hit. Set by `push`/`backspace`, cleared by `set_results`.
    dirty: bool,
}

impl Default for GlobalSearchState {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalSearchState {
    /// A fresh overlay: empty query, no results, nothing run yet.
    pub fn new() -> Self {
        Self {
            prompt: PromptInput::default(),
            results: Vec::new(),
            cursor: 0,
            searched: false,
            dirty: false,
        }
    }

    /// The current query string.
    pub fn query(&self) -> &str {
        self.prompt.query()
    }

    /// Push a printable character onto the query. Marks the query dirty (the results no longer
    /// reflect it) but does NOT run the search — that waits for `Enter`.
    pub fn push(&mut self, c: char) {
        self.prompt.push(c);
        self.dirty = true;
    }

    /// Remove the last character from the query. Marks the query dirty; does not run the search.
    pub fn backspace(&mut self) {
        self.prompt.backspace();
        self.dirty = true;
    }

    /// Install the hits from a completed run: reset the cursor to the top, mark a search as having
    /// run, and clear the dirty flag (the results now reflect the current query).
    pub fn set_results(&mut self, results: Vec<SearchHit>) {
        self.results = results;
        self.cursor = 0;
        self.searched = true;
        self.dirty = false;
    }

    /// Whether the next `Enter` should **run** the search rather than open a hit: the query has been
    /// edited since the last run, or no search has run yet.
    pub fn needs_search(&self) -> bool {
        self.dirty || !self.searched
    }

    /// Whether at least one search has run (for the "no matches" vs "type and press Enter" copy).
    pub fn searched(&self) -> bool {
        self.searched
    }

    /// The hits from the most recent run, in order.
    pub fn results(&self) -> &[SearchHit] {
        &self.results
    }

    /// The cursor position within the results list.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Move the cursor by `delta` rows, clamped to `[0, results.len()-1]`. A no-op when empty.
    pub fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            self.cursor = 0;
            return;
        }
        let max = self.results.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
    }

    /// The hit at the current cursor position, or `None` when there are no results.
    pub fn selected(&self) -> Option<&SearchHit> {
        self.results.get(self.cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn hit(rel: &str, line: usize) -> SearchHit {
        SearchHit {
            rel: PathBuf::from(rel),
            abs: PathBuf::from("/v").join(rel),
            line,
            preview: format!("preview {line}"),
        }
    }

    #[test]
    fn fresh_state_needs_a_search_and_has_no_results() {
        let s = GlobalSearchState::new();
        assert!(s.needs_search(), "nothing run yet → Enter runs the search");
        assert!(!s.searched());
        assert!(s.selected().is_none());
    }

    #[test]
    fn typing_marks_dirty_but_does_not_search() {
        let mut s = GlobalSearchState::new();
        s.set_results(vec![hit("A.md", 1)]);
        assert!(!s.needs_search(), "after a run, results reflect the query");
        s.push('x');
        assert!(
            s.needs_search(),
            "an edit means Enter should re-run, not open"
        );
    }

    #[test]
    fn set_results_resets_cursor_and_clears_dirty() {
        let mut s = GlobalSearchState::new();
        s.push('q');
        s.set_results(vec![hit("A.md", 2), hit("B.md", 9)]);
        assert!(!s.needs_search());
        assert!(s.searched());
        assert_eq!(s.cursor(), 0);
        s.move_selection(1);
        assert_eq!(s.selected().unwrap().rel, PathBuf::from("B.md"));
    }

    #[test]
    fn move_selection_clamps() {
        let mut s = GlobalSearchState::new();
        s.set_results(vec![hit("A.md", 1), hit("B.md", 2)]);
        s.move_selection(-5);
        assert_eq!(s.cursor(), 0);
        s.move_selection(5);
        assert_eq!(s.cursor(), 1);
    }
}
