//! Quick-switcher (`F`) — fuzzy-find any note in the containing Obsidian vault by name or alias and
//! open it in-viewer (Obsidian's ⌘O equivalent). Part of the Session Controller (mirrors
//! `controller/finder.rs`, but over the vault's notes rather than the repo's files).
//!
//! Also home to the two shared vault helpers the vault-navigation features reuse:
//! [`Controller::current_vault`] (detect the vault the current selection lives in) and
//! [`Controller::ensure_vault_index`] (build-or-reuse the cached [`crate::vault_index::VaultIndex`]).
//!
//! The overlay is the keyboard-only finder pattern: a query line plus a fuzzy-ranked list. Confirming
//! reuses [`Controller::navigate_to_note`], so a quick-switch feeds the same `[`/`]` back/forward
//! history the wikilink navigator does. Read-only: it only moves the in-pane selection.

use super::*;
use crate::obsidian::{Vault, find_vault};
use crate::switcher::SwitcherState;
use crate::vault_index::VaultIndex;

impl Controller {
    /// Detect the Obsidian vault the current selection lives in, if any: try the displayed note
    /// (`content_path`, what the user is reading) first, then the selected tree node, then the tree
    /// root — so the vault resolves whether a note, another file, or a directory is selected. `None`
    /// when nothing in that chain has a `.obsidian/` ancestor (not in a vault).
    pub(super) fn current_vault(&self) -> Option<Vault> {
        [
            self.content_path.clone(),
            self.tree.selected().map(|n| n.path),
            Some(self.root.clone()),
        ]
        .into_iter()
        .flatten()
        .find_map(|p| find_vault(&p))
    }

    /// Build-or-reuse the cached vault index for `vault_root`. Rebuilds when the cache is empty or was
    /// built for a different vault; otherwise reuses it (the index is invalidated wholesale on a
    /// re-root and on the `r` refresh, so a stale index only survives an out-of-band edit until the
    /// next refresh — see [`crate::vault_index`]). After this returns, `self.vault_index` is `Some`
    /// and built for `vault_root`.
    pub(super) fn ensure_vault_index(&mut self, vault_root: &Path) {
        let fresh = self
            .vault_index
            .as_ref()
            .is_some_and(|i| i.root() == vault_root);
        if !fresh {
            self.vault_index = Some(VaultIndex::build(vault_root));
        }
    }

    /// The owned quick-switcher draw model for the Presenter, or `None` when the overlay is closed.
    /// Resolves the ranked match indices into owned label strings (mirrors [`finder_view`]).
    ///
    /// [`finder_view`]: Self::finder_view
    pub(super) fn quick_switcher_view(&self) -> Option<crate::presenter::QuickSwitcherView> {
        let s = self.modal.quick_switcher()?;
        Some(crate::presenter::QuickSwitcherView {
            query: s.query().to_string(),
            rows: s
                .matches()
                .iter()
                .map(|&i| s.label(i).to_string())
                .collect(),
            cursor: s.cursor(),
        })
    }

    /// Whether the quick-switcher overlay is currently open.
    pub fn quick_switcher_open(&self) -> bool {
        self.modal.quick_switcher().is_some()
    }

    /// Open the quick-switcher (`F`). Gated to being inside an Obsidian vault: detect the vault, build
    /// or reuse its cached index, and build the candidate list — one row per note (its vault-relative
    /// name) plus one row per frontmatter alias (labelled `alias  ·  note`), each pointing at the
    /// note's absolute path. A notice (and no overlay) when not in a vault or the vault has no notes.
    pub(super) fn open_quick_switcher(&mut self) -> Effects {
        let Some(vault) = self.current_vault() else {
            self.action_notice = Some("Not in an Obsidian vault".into());
            return Effects::redraw();
        };
        self.ensure_vault_index(&vault.root);
        let index = self
            .vault_index
            .as_ref()
            .expect("ensure_vault_index sets it");
        let mut candidates: Vec<String> = Vec::new();
        let mut targets: Vec<PathBuf> = Vec::new();
        for note in index.notes() {
            let display = note.display();
            let abs = vault.root.join(&note.rel);
            candidates.push(display.clone());
            targets.push(abs.clone());
            for alias in &note.aliases {
                candidates.push(format!("{alias}  ·  {display}"));
                targets.push(abs.clone());
            }
        }
        if candidates.is_empty() {
            self.action_notice = Some("No notes in this vault".into());
            return Effects::redraw();
        }
        self.modal = Modal::QuickSwitcher(SwitcherState::new(candidates, targets));
        Effects::redraw()
    }

    /// Route a key while the quick-switcher is open: a printable char (no non-Shift modifier) edits
    /// the query and re-ranks, `Backspace` deletes, `Up`/`Down` move the selection, `Enter` opens the
    /// selected note, `Esc` closes. Mirrors `handle_finder_key`. A no-op (defensive) when closed.
    pub fn handle_quick_switcher_key(&mut self, key: KeyEvent) -> Effects {
        let Some(state) = self.modal.quick_switcher_mut() else {
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
            KeyCode::Enter => self.confirm_quick_switcher(),
            KeyCode::Esc => {
                self.modal = Modal::None;
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }

    /// Confirm the quick-switcher selection: take the highlighted note's absolute path, close the
    /// overlay, and navigate to it in-viewer (recording it in the `[`/`]` history). Zero matches →
    /// no-op, the overlay stays open (mirrors the finder confirm).
    fn confirm_quick_switcher(&mut self) -> Effects {
        let Some(target) = self
            .modal
            .quick_switcher()
            .and_then(|s| s.selected_target())
            .map(Path::to_path_buf)
        else {
            return Effects::noop();
        };
        self.modal = Modal::None;
        self.navigate_to_note(&target, None)
    }
}
