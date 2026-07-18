//! Global content search (`S`) — grep the whole vault's note contents for a query, list the hits,
//! and open the chosen one in-viewer at the matching line. Part of the Session Controller.
//!
//! Two-phase (see [`crate::globalsearch`]): type a query, `Enter` **runs** the search (through the
//! injected [`crate::vsearch::ContentSearcher`], or the pure fallback when none is wired), then
//! `Enter` on a highlighted hit **opens** it, scrolling to the matched line via
//! [`Controller::navigate_to_note`]. Read-only: it only moves the in-pane selection.

use super::*;
use crate::globalsearch::GlobalSearchState;
use crate::presenter::{GlobalSearchRowView, GlobalSearchView};

/// The most hits a single search returns. A generous bound so a common term can't build an unbounded
/// result list; the overlay scrolls within it.
const SEARCH_HIT_LIMIT: usize = 500;

impl Controller {
    /// The owned global-search draw model for the Presenter, or `None` when the overlay is closed.
    /// Projects each hit into a borrow-free row (`note:line` location + preview).
    pub(super) fn global_search_view(&self) -> Option<GlobalSearchView> {
        let s = self.modal.global_search()?;
        Some(GlobalSearchView {
            query: s.query().to_string(),
            rows: s
                .results()
                .iter()
                .map(|hit| GlobalSearchRowView {
                    location: format!("{}:{}", hit_display(&hit.rel), hit.line),
                    preview: hit.preview.clone(),
                })
                .collect(),
            cursor: s.cursor(),
            searched: s.searched(),
        })
    }

    /// Whether the global content-search overlay is currently open.
    pub fn global_search_open(&self) -> bool {
        self.modal.global_search().is_some()
    }

    /// Open the global content search (`S`). Gated to being inside an Obsidian vault: a notice (and no
    /// overlay) when not in one. Opens with an empty query — the search runs on the first `Enter`.
    pub(super) fn open_global_search(&mut self) -> Effects {
        if self.current_vault().is_none() {
            self.action_notice = Some("Not in an Obsidian vault".into());
            return Effects::redraw();
        }
        self.modal = Modal::GlobalSearch(GlobalSearchState::new());
        Effects::redraw()
    }

    /// Route a key while the global search is open: a printable char (no non-Shift modifier) edits the
    /// query, `Backspace` deletes, `Up`/`Down` move the result cursor, `Enter` runs the search (when
    /// the query changed) or opens the highlighted hit, `Esc` closes. A no-op (defensive) when closed.
    pub fn handle_global_search_key(&mut self, key: KeyEvent) -> Effects {
        let Some(state) = self.modal.global_search_mut() else {
            return Effects::noop();
        };
        match key.code {
            KeyCode::Char(c) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                state.push(c);
                Effects::redraw()
            }
            KeyCode::Backspace => {
                state.backspace();
                Effects::redraw()
            }
            KeyCode::Up => {
                state.move_selection(-1);
                Effects::redraw()
            }
            KeyCode::Down => {
                state.move_selection(1);
                Effects::redraw()
            }
            KeyCode::Enter => {
                // Re-run the search when the query changed since the last run; otherwise open the
                // highlighted hit. `needs_search` reads the modal, so decide before mutating self.
                if state.needs_search() {
                    self.run_global_search()
                } else {
                    self.open_selected_hit()
                }
            }
            KeyCode::Esc => {
                self.modal = Modal::None;
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }

    /// Run the content search for the overlay's current query and install the hits. Re-detects the
    /// vault (it must still exist — the overlay only opened inside one); an empty query is a no-op that
    /// keeps the overlay open. Uses the injected searcher (ripgrep, in `app.rs`) when wired, else the
    /// pure in-Rust fallback — so a controller with no searcher still searches, hermetically.
    fn run_global_search(&mut self) -> Effects {
        let query = match self.modal.global_search() {
            Some(s) => s.query().to_string(),
            None => return Effects::noop(),
        };
        if query.trim().is_empty() {
            return Effects::redraw();
        }
        let Some(vault) = self.current_vault() else {
            self.modal = Modal::None;
            self.action_notice = Some("Not in an Obsidian vault".into());
            return Effects::redraw();
        };
        let hits = self.run_vault_search(&vault.root, &query, SEARCH_HIT_LIMIT);
        if let Some(state) = self.modal.global_search_mut() {
            state.set_results(hits);
        }
        Effects::redraw()
    }

    /// Run the search through the injected [`crate::vsearch::ContentSearcher`] when one is wired (the
    /// live ripgrep-backed searcher), else fall back to the pure in-Rust scan — so tests that never
    /// inject a searcher still get real, deterministic results without spawning a subprocess.
    fn run_vault_search(
        &self,
        vault_root: &Path,
        query: &str,
        limit: usize,
    ) -> Vec<crate::vsearch::SearchHit> {
        match &self.searcher {
            Some(searcher) => searcher.search(vault_root, query, limit),
            None => crate::vsearch::search_fallback(vault_root, query, limit),
        }
    }

    /// Open the highlighted hit: close the overlay and navigate to the note, scrolling to the matched
    /// line (via the same `pending_goto` path go-to-line uses). A no-op when there are no results.
    fn open_selected_hit(&mut self) -> Effects {
        let Some((abs, line)) = self
            .modal
            .global_search()
            .and_then(|s| s.selected())
            .map(|hit| (hit.abs.clone(), hit.line))
        else {
            return Effects::noop();
        };
        self.modal = Modal::None;
        self.navigate_to_note(&abs, Some(line))
    }
}

/// A vault-relative note path rendered for a hit's location label: forward-slashed, with the
/// `.md`/`.markdown` extension dropped (matching the quick-switcher's note labels).
fn hit_display(rel: &Path) -> String {
    let s = rel.to_string_lossy().replace('\\', "/");
    for ext in [".md", ".markdown"] {
        if s.len() >= ext.len() && s[s.len() - ext.len()..].eq_ignore_ascii_case(ext) {
            return s[..s.len() - ext.len()].to_string();
        }
    }
    s
}
