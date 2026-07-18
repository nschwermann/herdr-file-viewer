//! Quick-Switcher state — the ephemeral state of the vault note quick-switcher overlay (`F`).
//!
//! [`SwitcherState`] is the go-to-file [`crate::finder::FinderState`] pattern applied to the
//! containing Obsidian vault's **notes** instead of the repo's files: a query buffer, a candidate
//! **label** list (each note's vault-relative name plus one row per frontmatter alias), the current
//! fuzzy-ranked match indices, and a cursor. It carries a `targets` vector parallel to the labels so
//! confirming a match resolves to the note's absolute path to open in-viewer (Obsidian's ⌘O). The
//! candidates come from the cached [`crate::vault_index::VaultIndex`]; matching is
//! [`crate::fuzzy::match_and_rank`], the same scorer the finder uses.
//!
//! Pure (no I/O): the controller builds the labels/targets from the vault index and drives the
//! query/cursor via the run loop.

use crate::fuzzy;
use crate::prompt::PromptInput;
use std::path::{Path, PathBuf};

/// Live state of the quick-switcher overlay while it is open.
///
/// Created by [`crate::controller::Controller::open_quick_switcher`] when the user presses `F` inside
/// an Obsidian vault, and destroyed when they confirm (open a note) or cancel.
pub struct SwitcherState {
    /// The current query the user has typed.
    prompt: PromptInput,
    /// The candidate row labels — one per note (its vault-relative name) plus one per alias — that
    /// the fuzzy matcher ranks. Parallel to [`targets`](Self::targets): `candidates[i]` opens
    /// `targets[i]`. Populated once at open time; not refreshed mid-session.
    candidates: Vec<String>,
    /// The absolute note path each candidate label opens, parallel to `candidates`. Several labels
    /// (a note's name plus its aliases) can point at the same note.
    targets: Vec<PathBuf>,
    /// Indices into `candidates` that match the current query, ranked best-first. Empty when the
    /// query is empty (no matches until the user types). Driven by the run loop.
    matches: Vec<usize>,
    /// Cursor position within `matches`. Driven by the run loop.
    cursor: usize,
}

impl SwitcherState {
    /// Build a new `SwitcherState` with an empty prompt over the given parallel `candidates`/`targets`.
    /// The two must be the same length (one target per candidate label).
    pub fn new(candidates: Vec<String>, targets: Vec<PathBuf>) -> Self {
        debug_assert_eq!(candidates.len(), targets.len());
        Self {
            prompt: PromptInput::default(),
            candidates,
            targets,
            matches: Vec::new(),
            cursor: 0,
        }
    }

    /// The current query string.
    pub fn query(&self) -> &str {
        self.prompt.query()
    }

    /// Push a printable character onto the query and re-run the fuzzy match; the selection resets to
    /// the top so a stale cursor from the previous match list is never surfaced.
    pub fn push(&mut self, c: char) {
        self.prompt.push(c);
        self.recompute();
    }

    /// Remove the last character from the query and re-run the fuzzy match.
    pub fn backspace(&mut self) {
        self.prompt.backspace();
        self.recompute();
    }

    /// Re-run [`fuzzy::match_and_rank`] against the current query and reset the cursor to 0.
    fn recompute(&mut self) {
        self.matches = fuzzy::match_and_rank(self.prompt.query(), &self.candidates);
        self.cursor = 0;
    }

    /// Move the cursor within the match list by `delta` rows, clamped to `[0, matches.len()-1]`. A
    /// no-op (cursor stays 0) when the list is empty.
    pub fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            self.cursor = 0;
            return;
        }
        let max = self.matches.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
    }

    /// The ranked match indices (into `candidates`). Exposed for the Presenter projection and tests.
    pub fn matches(&self) -> &[usize] {
        &self.matches
    }

    /// The label of the `i`-th candidate. Exposed for the Presenter projection.
    pub fn label(&self, candidate_index: usize) -> &str {
        &self.candidates[candidate_index]
    }

    /// The cursor position within the match list.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The absolute note path at the current cursor position, or `None` when the match list is empty
    /// (zero matches → nothing to open).
    pub fn selected_target(&self) -> Option<&Path> {
        let cand = *self.matches.get(self.cursor)?;
        self.targets.get(cand).map(PathBuf::as_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> SwitcherState {
        SwitcherState::new(
            vec![
                "Alpha".to_string(),
                "sub/Beta".to_string(),
                "nickname  ·  sub/Beta".to_string(),
            ],
            vec![
                PathBuf::from("/v/Alpha.md"),
                PathBuf::from("/v/sub/Beta.md"),
                PathBuf::from("/v/sub/Beta.md"),
            ],
        )
    }

    #[test]
    fn empty_query_has_no_matches() {
        let s = state();
        assert!(s.matches().is_empty());
        assert_eq!(s.selected_target(), None);
    }

    #[test]
    fn typing_fuzzy_matches_and_selects() {
        let mut s = state();
        s.push('B');
        s.push('e');
        assert!(!s.matches().is_empty(), "Beta rows match 'Be'");
        // The top match resolves to Beta's path.
        assert_eq!(s.selected_target(), Some(Path::new("/v/sub/Beta.md")));
    }

    #[test]
    fn alias_label_matches_by_alias_and_targets_the_note() {
        let mut s = state();
        for c in "nick".chars() {
            s.push(c);
        }
        // The alias row ("nickname · sub/Beta") matches and points at Beta.
        assert_eq!(s.selected_target(), Some(Path::new("/v/sub/Beta.md")));
    }

    #[test]
    fn backspace_recomputes() {
        let mut s = state();
        s.push('z'); // matches nothing
        assert!(s.matches().is_empty());
        s.backspace();
        // Empty query → no matches (matches only appear once the user types).
        assert!(s.matches().is_empty());
    }

    #[test]
    fn move_selection_clamps() {
        let mut s = state();
        s.push('B'); // matches the two Beta rows
        s.move_selection(-1);
        assert_eq!(s.cursor(), 0);
        s.move_selection(10);
        assert_eq!(s.cursor(), s.matches().len() - 1);
    }
}
