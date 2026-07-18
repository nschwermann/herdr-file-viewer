//! Backlinks panel (`G`) — open a list of every other note that links *to* the current
//! markdown-in-vault note, and follow the selected backlink to that note in-viewer. Part of the
//! Session Controller (mirrors `controller/linknav.rs`, the outbound `g` navigator: `g` lists the
//! links *out* of this note, `G` lists the notes that link *in*).
//!
//! The overlay is a keyboard-only modal ([`Modal::Backlinks`]) modelled after the wikilink
//! navigator and heading outline: a flat list of the linking notes with a cursor. Backlinks are a
//! cheap reverse lookup over the cached [`crate::vault_index::VaultIndex`]'s resolved outgoing
//! links — so this reuses the same `self.vault_index` the quick-switcher builds. Confirming a
//! backlink reuses [`Controller::navigate_to_note`], so a follow feeds the same `[`/`]`
//! back/forward history the wikilink navigator does. Everything is read-only: it re-reveals notes
//! and moves the in-pane selection, never touching files or git.

use super::*;

impl Controller {
    /// The owned backlinks draw model for the Presenter, or `None` when the overlay is closed.
    /// Projects each stored [`BacklinkItem`] into its borrow-free display row, mirroring
    /// [`link_nav_view`](Self::link_nav_view) / [`outline_view`](Self::outline_view).
    pub(super) fn backlinks_view(&self) -> Option<BacklinksView> {
        let s = self.modal.backlinks()?;
        Some(BacklinksView {
            rows: s.items().iter().map(|item| item.display.clone()).collect(),
            cursor: s.cursor(),
        })
    }

    /// Whether the backlinks overlay is currently open.
    pub fn backlinks_open(&self) -> bool {
        self.modal.backlinks().is_some()
    }

    /// The note the backlinks panel is built for: the displayed content (`content_path`, what the
    /// user is reading) first, else the selected tree node when it is a file (before any render has
    /// landed). Mirrors [`outline_note_path`](Self::outline_note_path).
    fn backlinks_note_path(&self) -> Option<PathBuf> {
        self.content_path.clone().or_else(|| {
            self.tree
                .selected()
                .filter(|n| n.kind == NodeKind::File)
                .map(|n| n.path)
        })
    }

    /// Open the backlinks panel (`G`). Gated to a markdown file inside an Obsidian vault (the same
    /// gate as the `g` wikilink navigator): the current file is the displayed content
    /// (`content_path`), falling back to the selected tree node. When that is not a markdown note in
    /// a vault, set the guidance notice and open nothing. Otherwise build or reuse the cached vault
    /// index, compute the backlinks as a reverse lookup over its resolved outgoing links, and open
    /// the overlay — or set a "no backlinks" notice when nothing links here.
    pub(super) fn open_backlinks(&mut self) -> Effects {
        let notice = "No backlinks: open a markdown note in an Obsidian vault";
        let Some(current) = self.backlinks_note_path() else {
            self.action_notice = Some(notice.into());
            return Effects::redraw();
        };
        if !crate::obsidian::is_markdown(&current) {
            self.action_notice = Some(notice.into());
            return Effects::redraw();
        }
        let Some(vault) = crate::obsidian::find_vault(&current) else {
            self.action_notice = Some(notice.into());
            return Effects::redraw();
        };
        // The current note's vault-relative path is the key the reverse lookup matches against; it
        // is `Some` since the vault is an ancestor of `current`. On the off chance it is not, treat
        // it as a note with no backlinks (the guidance notice).
        let Some(source_rel) = vault.relative(&current).map(Path::to_path_buf) else {
            self.action_notice = Some(notice.into());
            return Effects::redraw();
        };
        // Build or reuse the cached vault index (shared with the quick-switcher), then scan it for
        // the notes whose resolved outgoing links include this note. Each becomes an owned row so
        // the index borrow is dropped before we mutate `self.modal`.
        self.ensure_vault_index(&vault.root);
        let index = self
            .vault_index
            .as_ref()
            .expect("ensure_vault_index sets it");
        let items: Vec<BacklinkItem> = index
            .backlinks(&source_rel)
            .into_iter()
            .map(|note| BacklinkItem {
                display: note.display(),
                path: vault.root.join(&note.rel),
            })
            .collect();
        if items.is_empty() {
            self.action_notice = Some("No backlinks to this note".into());
            return Effects::redraw();
        }
        self.modal = Modal::Backlinks(BacklinksState::new(items));
        Effects::redraw()
    }

    /// Route a key while the backlinks panel is open: `j`/`Down` and `k`/`Up` move the cursor,
    /// `Enter` opens the selected backlink, `Esc`/`q` closes. Mirrors `handle_link_nav_key`. A
    /// no-op (defensive) when the panel is not open.
    pub fn handle_backlinks_key(&mut self, key: KeyEvent) -> Effects {
        let Some(state) = self.modal.backlinks_mut() else {
            return Effects::noop();
        };
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                state.move_selection(1);
                Effects::redraw()
            }
            KeyCode::Char('k') | KeyCode::Up => {
                state.move_selection(-1);
                Effects::redraw()
            }
            KeyCode::Enter => self.follow_backlink(),
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = Modal::None;
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }

    /// Follow the selected backlink: close the panel and navigate to that note in-viewer, recording
    /// the jump in the `[`/`]` history (via [`navigate_to_note`](Self::navigate_to_note), the shared
    /// reveal + render primitive the quick-switcher and wikilink navigator use). A no-op (the panel
    /// stays open) when there is somehow no selection.
    fn follow_backlink(&mut self) -> Effects {
        let Some(abs) = self
            .modal
            .backlinks()
            .and_then(|s| s.selected())
            .map(|item| item.path.clone())
        else {
            return Effects::noop();
        };
        self.modal = Modal::None;
        self.navigate_to_note(&abs, None)
    }
}
