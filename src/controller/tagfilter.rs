//! Clickable-tag filter — click a `#tag` chip (Properties panel) or an inline `#tag` in a rendered
//! note to restrict the file tree to every vault note carrying that tag (Obsidian's `tag:` search),
//! and clear it with `Esc`. Part of the Session Controller (mirrors `controller/linknav.rs`).
//!
//! The click hit-test lives in `controller/mouse.rs` (`content_target_at` → `ContentTarget::Tag`);
//! this module owns the controller side: build/cache the vault [`crate::tagindex::TagIndex`], apply
//! the match set to the [`crate::tree::TreeModel`], surface the active tag (the tree-title
//! indicator + the `Esc` clear-gesture), and tear it down. Everything is read-only: it reads notes
//! to build the index and re-shapes the in-memory tree, never touching files or git.

use super::*;
use std::collections::BTreeSet;

/// Cap on notes scanned when building the vault tag index (mirrors the `markdown_index` cap the
/// wikilink navigator uses). A generous bound; a real vault is far smaller.
const TAG_INDEX_MAX_FILES: usize = 20_000;

impl Controller {
    /// Apply the clickable-tag filter for `tag` (without its leading `#`): restrict the tree to
    /// every vault note carrying that tag (or a nested child) that lives under the current tree
    /// root, with ancestor dirs auto-revealed. Builds/reuses the cached vault tag index, sets the
    /// active-tag mirror (for the title indicator + the `Esc` clear-gesture), and re-renders for the
    /// possibly-changed selection. A non-vault note, or a tag with no matching notes under the root,
    /// sets a one-line notice and leaves the tree unchanged.
    pub(super) fn apply_tag_filter(&mut self, tag: &str) -> Effects {
        let Some(rel) = self.tag_matches(tag) else {
            self.action_notice = Some(format!(
                "#{tag}: open a note in an Obsidian vault to filter by tag"
            ));
            return Effects::redraw();
        };
        if rel.is_empty() {
            self.action_notice = Some(format!(
                "#{tag}: no notes with this tag under the current root"
            ));
            return Effects::redraw();
        }
        let n = rel.len();
        self.tree.set_tag_filter(true, &rel);
        self.active_tag = Some(tag.to_string());
        self.action_notice = Some(format!(
            "Filtering tree by #{tag} — {n} note{} · Esc to clear",
            if n == 1 { "" } else { "s" }
        ));
        // The selection may now sit on a different (or the only matching) note — re-render it.
        self.dispatch_render();
        Effects::redraw()
    }

    /// The tree-root-relative note paths matching `tag` in the current vault, or `None` when there
    /// is no current markdown-in-vault note to locate (hence build) the index from. Ensures the
    /// cached index is present/fresh for the vault as a side effect. Notes outside the tree root are
    /// dropped — a note above the root can't be shown in a root-bounded tree.
    pub(super) fn tag_matches(&mut self, tag: &str) -> Option<BTreeSet<PathBuf>> {
        // The current note: the displayed file first, else the selected tree node when it is a file.
        let current = self.content_path.clone().or_else(|| {
            self.tree
                .selected()
                .filter(|n| n.kind == NodeKind::File)
                .map(|n| n.path)
        })?;
        if !crate::obsidian::is_markdown(&current) {
            return None;
        }
        let vault = crate::obsidian::find_vault(&current)?;
        self.ensure_tag_index(&vault.root);
        let index = self.tag_index.as_ref()?;
        let rel: BTreeSet<PathBuf> = index
            .notes_for(tag)
            .iter()
            .filter_map(|p| p.strip_prefix(&self.root).ok().map(Path::to_path_buf))
            .collect();
        Some(rel)
    }

    /// Build the cached vault tag index for `vault_root` if absent, or rebuild it when the cached
    /// one was built for a different vault. Read-only (a bounded vault walk).
    fn ensure_tag_index(&mut self, vault_root: &Path) {
        let fresh = self
            .tag_index
            .as_ref()
            .is_some_and(|ix| ix.vault_root() == vault_root);
        if !fresh {
            self.tag_index = Some(crate::tagindex::TagIndex::build(
                vault_root,
                TAG_INDEX_MAX_FILES,
            ));
        }
    }

    /// Clear the active tag filter (the `Esc` clear-gesture): lift it from the tree, reset the
    /// mirror, and re-render for the restored selection. The caller gates on an active filter.
    pub(super) fn clear_tag_filter(&mut self) -> Effects {
        self.clear_tag_filter_state();
        self.dispatch_render();
        Effects::redraw()
    }

    /// Reset the tag-filter state (mirror + tree flag) WITHOUT a re-render — the shared teardown for
    /// the `Esc` clear and the refresh path that drops a now-empty filter.
    pub(super) fn clear_tag_filter_state(&mut self) {
        self.active_tag = None;
        self.tree.set_tag_filter(false, &BTreeSet::new());
    }

    /// Re-sync the tag-filter mirror after a `reveal` may have relaxed the tree's `tag_only` flag (a
    /// finder jump / link follow to a note outside the filter lifts it). Mirrors the
    /// `changed_only`/`hide_hidden` re-sync so the title indicator and `Esc` clear stay consistent.
    pub(super) fn sync_tag_filter_after_reveal(&mut self) {
        if !self.tree.tag_only() {
            self.active_tag = None;
        }
    }

    /// The tag currently filtering the tree (the clicked `#tag`, without its `#`), or `None`. For
    /// the Presenter's tree-title indicator and tests.
    pub fn active_tag(&self) -> Option<&str> {
        self.active_tag.as_deref()
    }
}
