//! Inline wikilink navigation (`g` + `[`/`]`) — open the link navigator for the current
//! markdown-in-vault note, follow a link to another note in-viewer, and walk a back/forward
//! history. Part of the Session Controller (mirrors `controller/finder.rs`).
//!
//! The overlay itself is a keyboard-only modal ([`Modal::LinkNav`]) modelled after the go-to-file
//! finder: a flat list of the note's links with a cursor. Following a resolved link reuses the
//! finder's "reveal + render" navigation primitive; a link with an anchor additionally queues an
//! anchor scroll via the same `overrides`/`pending_goto` mechanism go-to-line uses. Everything is
//! read-only: it re-reveals notes and moves the in-pane selection, never touching files or git.

use super::*;
use crate::wikilink::Anchor;

/// The largest note we read for link parsing / anchor resolution. A generous bound so a
/// pathological file can never stall the input thread or blow memory; a real note is far smaller.
const MAX_NOTE_BYTES: u64 = 1024 * 1024;

/// Read up to [`MAX_NOTE_BYTES`] of `path`, lossily decoded as UTF-8. Returns `None` on any I/O
/// error (never panics), so every caller degrades gracefully to "no links" / "no anchor". Shared
/// with the status bar's link-count cache ([`Controller::recompute_link_count`]).
pub(super) fn read_note_bounded(path: &Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_NOTE_BYTES).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

impl Controller {
    /// The owned link-navigator draw model for the Presenter, or `None` when the overlay is closed.
    /// Projects each stored [`LinkItem`] into a borrow-free row (display text + resolved flag),
    /// mirroring [`finder_view`](Self::finder_view).
    pub(super) fn link_nav_view(&self) -> Option<LinkNavView> {
        let s = self.modal.link_nav()?;
        Some(LinkNavView {
            rows: s
                .items()
                .iter()
                .map(|item| LinkNavRowView {
                    display: item.display.clone(),
                    resolved: item.resolved.is_some(),
                })
                .collect(),
            cursor: s.cursor(),
        })
    }

    /// Whether the wikilink navigator overlay is currently open.
    pub fn link_nav_open(&self) -> bool {
        self.modal.link_nav().is_some()
    }

    /// Open the wikilink navigator (`g`). Gated to a markdown file inside an Obsidian vault: the
    /// current file is the displayed content (`content_path`), falling back to the selected tree
    /// node. When that is not a markdown note in a vault — or it has no links — set a one-line
    /// notice and open nothing. Otherwise read the note (bounded), parse its links, build the vault
    /// index, resolve each target to an absolute path (keeping its anchor), and open the overlay.
    pub(super) fn open_link_nav(&mut self) -> Effects {
        // The current note: the displayed file first (what the user is reading), else the selected
        // tree node when it is a file (before any render has landed).
        let current = self.content_path.clone().or_else(|| {
            self.tree
                .selected()
                .filter(|n| n.kind == NodeKind::File)
                .map(|n| n.path)
        });
        let Some(current) = current else {
            self.action_notice =
                Some("No wikilinks: open a markdown note in an Obsidian vault".into());
            return Effects::redraw();
        };
        // Gate: markdown + inside a vault. Either miss shows the same guidance notice.
        if !crate::obsidian::is_markdown(&current) {
            self.action_notice =
                Some("No wikilinks: open a markdown note in an Obsidian vault".into());
            return Effects::redraw();
        }
        let Some(vault) = crate::obsidian::find_vault(&current) else {
            self.action_notice =
                Some("No wikilinks: open a markdown note in an Obsidian vault".into());
            return Effects::redraw();
        };
        let Some(source) = read_note_bounded(&current) else {
            self.action_notice =
                Some("No wikilinks: open a markdown note in an Obsidian vault".into());
            return Effects::redraw();
        };
        let links = crate::wikilink::parse_links(&source);
        if links.is_empty() {
            self.action_notice = Some("No wikilinks in this note".into());
            return Effects::redraw();
        }
        // Resolve each target the Obsidian way (shortest-unique-path over the whole vault), keeping
        // the anchor. `source_rel` is `current` relative to the vault root (Some, since the vault is
        // an ancestor of `current`); on the off chance it is not, every link is simply unresolved.
        let index = crate::obsidian::markdown_index(&vault.root, 20_000);
        let source_rel = vault.relative(&current);
        let items: Vec<LinkItem> = links
            .into_iter()
            .map(|link| {
                let resolved = source_rel
                    .and_then(|sr| crate::obsidian::resolve_target(&link.target, sr, &index))
                    .map(|rel| vault.root.join(rel));
                LinkItem {
                    display: link.display(),
                    resolved,
                    anchor: link.anchor,
                }
            })
            .collect();
        self.modal = Modal::LinkNav(LinkNavState::new(items));
        Effects::redraw()
    }

    /// Route a key while the link navigator is open: `j`/`Down` and `k`/`Up` move the cursor,
    /// `Enter` follows the selected link, `Esc`/`q` closes. Mirrors `handle_finder_key`. A no-op
    /// (defensive) when the navigator is not open.
    pub fn handle_link_nav_key(&mut self, key: KeyEvent) -> Effects {
        let Some(state) = self.modal.link_nav_mut() else {
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
            KeyCode::Enter => self.follow_link(),
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = Modal::None;
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }

    /// Follow the selected link. An unresolved link closes the navigator with a notice and does not
    /// navigate. A resolved link closes the navigator, pushes the current note onto the back-stack,
    /// clears the forward-stack (a new branch abandons the old forward history), and navigates
    /// (reveal + render) to the target — queuing an anchor scroll when the link carries one.
    fn follow_link(&mut self) -> Effects {
        // Extract owned copies so the immutable borrow of `self.modal` is dropped before we mutate.
        let Some((display, resolved, anchor)) = self
            .modal
            .link_nav()
            .and_then(|s| s.selected())
            .map(|item| {
                (
                    item.display.clone(),
                    item.resolved.clone(),
                    item.anchor.clone(),
                )
            })
        else {
            return Effects::noop();
        };
        self.modal = Modal::None;
        let Some(abs) = resolved else {
            self.action_notice = Some(format!("Unresolved link: {display}"));
            return Effects::redraw();
        };
        if !self.tree.reveal(&abs) {
            self.action_notice = Some(format!("Could not open {}", self.nav_display_path(&abs)));
            return Effects::redraw();
        }
        // Push the note we're leaving onto the back-stack and start a fresh forward history.
        if let Some(cur) = self.content_path.clone() {
            self.nav_back.push(cur);
        }
        self.nav_forward.clear();
        self.after_nav_reveal(&abs, anchor.as_ref());
        Effects::redraw()
    }

    /// Follow a link parsed from a **mouse click** on the displayed content (FIX 3): resolve its
    /// target vault-wide (the same shortest-unique-path resolution the `g` navigator uses) and
    /// navigate to that note in-viewer, keeping the back/forward history. An unresolved target shows
    /// a notice. Shared by the content-click hit-test (`content_target_at`).
    pub(super) fn follow_content_link(&mut self, link: &crate::wikilink::Link) -> Effects {
        let current = self.content_path.clone().or_else(|| {
            self.tree
                .selected()
                .filter(|n| n.kind == NodeKind::File)
                .map(|n| n.path)
        });
        let Some(current) = current else {
            return Effects::noop();
        };
        let Some(abs) = self.resolve_note_target(&current, &link.target) else {
            self.action_notice = Some(format!("Unresolved link: {}", link.display()));
            return Effects::redraw();
        };
        if !self.tree.reveal(&abs) {
            self.action_notice = Some(format!("Could not open {}", self.nav_display_path(&abs)));
            return Effects::redraw();
        }
        // Mirror `follow_link`: push the note we're leaving, start a fresh forward history, navigate.
        if let Some(cur) = self.content_path.clone() {
            self.nav_back.push(cur);
        }
        self.nav_forward.clear();
        self.after_nav_reveal(&abs, link.anchor.as_ref());
        Effects::redraw()
    }

    /// Resolve a wikilink/markdown-link `target` to an absolute **note** path within `current`'s
    /// vault, the Obsidian way (shortest-unique-path). `None` when `current` is not a markdown note
    /// in a vault, or the target does not resolve. Read-only (a bounded vault walk).
    fn resolve_note_target(&self, current: &Path, target: &str) -> Option<PathBuf> {
        if !crate::obsidian::is_markdown(current) {
            return None;
        }
        let vault = crate::obsidian::find_vault(current)?;
        let source_rel = vault.relative(current)?;
        let index = crate::obsidian::markdown_index(&vault.root, 20_000);
        let rel = crate::obsidian::resolve_target(target, source_rel, &index)?;
        Some(vault.root.join(rel))
    }

    /// Go back to the previously-viewed note (`[`). Peeks the back-stack, re-reveals that note, and
    /// on success pops it and pushes the note being left onto the forward-stack. An empty stack
    /// shows a notice and does nothing but redraw.
    pub(super) fn nav_back(&mut self) -> Effects {
        let Some(prev) = self.nav_back.last().cloned() else {
            self.action_notice = Some("No previous note".into());
            return Effects::redraw();
        };
        if !self.tree.reveal(&prev) {
            self.action_notice = Some(format!("Could not open {}", self.nav_display_path(&prev)));
            return Effects::redraw();
        }
        self.nav_back.pop();
        if let Some(cur) = self.content_path.clone() {
            self.nav_forward.push(cur);
        }
        self.after_nav_reveal(&prev, None);
        Effects::redraw()
    }

    /// Go forward in the link-navigation history (`]`) — the mirror of [`nav_back`](Self::nav_back).
    pub(super) fn nav_forward(&mut self) -> Effects {
        let Some(next) = self.nav_forward.last().cloned() else {
            self.action_notice = Some("No next note".into());
            return Effects::redraw();
        };
        if !self.tree.reveal(&next) {
            self.action_notice = Some(format!("Could not open {}", self.nav_display_path(&next)));
            return Effects::redraw();
        }
        self.nav_forward.pop();
        if let Some(cur) = self.content_path.clone() {
            self.nav_back.push(cur);
        }
        self.after_nav_reveal(&next, None);
        Effects::redraw()
    }

    /// Shared post-reveal wiring for every link/back/forward navigation, mirroring the finder's
    /// confirm: re-sync the filter mirrors `reveal` may have relaxed, zoom the file when the content
    /// pane isn't visible (so the user actually sees the note they jumped to), then render. When
    /// `anchor` is `Some` and its target line is found, open the note in the source-mapped view and
    /// queue a `pending_goto` scroll to that line (the exact mechanism go-to-line uses); otherwise
    /// render normally at the top. Best-effort: a missing anchor just opens the note at the top.
    fn after_nav_reveal(&mut self, abs: &Path, anchor: Option<&Anchor>) {
        // Resolve the anchor to a 1-based source line (best-effort), then defer to the line-based
        // variant so the anchor path and the direct-line path (outline / quick-switcher / global
        // search hits) share one implementation.
        let line = anchor.and_then(|a| self.anchor_line(abs, a));
        self.after_nav_reveal_line(abs, line);
    }

    /// The line-parameterised form of [`after_nav_reveal`](Self::after_nav_reveal): the same
    /// post-reveal wiring, but taking an already-resolved 1-based source `line` instead of an anchor.
    /// When `line` is `Some`, open the note in the source-mapped view and queue a `pending_goto`
    /// scroll to it (the exact mechanism go-to-line and the heading outline use); otherwise render at
    /// the top. Shared by the wikilink navigator (anchor → line), the global content search (hit
    /// line), and the note-opening confirms that jump to no particular line.
    pub(super) fn after_nav_reveal_line(&mut self, abs: &Path, line: Option<usize>) {
        // reveal() may have relaxed the tree's changed_only/hide_hidden — re-sync the mirrors so a
        // later `c`/`.` toggle stays consistent (same as `confirm_finder`).
        self.changed_only = self.tree.changed_only();
        self.hide_hidden = self.tree.hide_hidden();
        // A link/back/forward jump to a note outside an active tag filter relaxes the tree's tag
        // filter — re-sync the mirror so the title indicator + Esc clear-gesture reset with it.
        self.sync_tag_filter_after_reveal();
        // If the content pane isn't visible (narrow, tree-only layout), open the note zoomed so the
        // jumped-to file is actually on screen — mirrors the finder confirm.
        if self.content_width == 0 {
            self.zoomed = true;
            self.focus = Focus::Content;
        }
        if let Some(line) = line {
            self.overrides
                .insert(abs.to_path_buf(), ViewMode::SyntaxContent);
            self.dispatch_render();
            self.pending_goto = Some((self.latest_seq, line));
        } else {
            self.dispatch_render();
        }
    }

    /// Navigate the viewer to note `abs` in-viewer, recording the jump in the browser-style history:
    /// reveal it in the tree, push the note being left onto the back-stack, clear the forward-stack (a
    /// new branch abandons the old forward history), then wire up the reveal (optionally scrolling to
    /// `line`). A vanished target sets a non-fatal notice and does not navigate. Shared by the
    /// quick-switcher, the global content search, and the backlinks panel so all three feed the same
    /// `[`/`]` history the wikilink navigator does. Read-only: it only moves the in-pane selection.
    pub(super) fn navigate_to_note(&mut self, abs: &Path, line: Option<usize>) -> Effects {
        if !self.tree.reveal(abs) {
            self.action_notice = Some(format!("Could not open {}", self.nav_display_path(abs)));
            return Effects::redraw();
        }
        if let Some(cur) = self.content_path.clone() {
            self.nav_back.push(cur);
        }
        self.nav_forward.clear();
        self.after_nav_reveal_line(abs, line);
        Effects::redraw()
    }

    /// Resolve a link anchor to a 1-based source line in the target note (best-effort; `None` when
    /// not found or the note can't be read). A heading anchor matches the first [`crate::mdnote`]
    /// heading whose text equals it (case-insensitive, trimmed); a block anchor matches the first
    /// source line ending in `^<id>`.
    fn anchor_line(&self, abs: &Path, anchor: &Anchor) -> Option<usize> {
        let source = read_note_bounded(abs)?;
        match anchor {
            Anchor::Heading(h) => {
                let want = h.trim().to_lowercase();
                crate::mdnote::headings(&source)
                    .into_iter()
                    .find(|hd| hd.text.trim().to_lowercase() == want)
                    .map(|hd| hd.line)
            }
            Anchor::Block(id) => {
                let marker = format!("^{}", id.trim());
                source
                    .lines()
                    .enumerate()
                    .find(|(_, line)| line.trim_end().ends_with(&marker))
                    .map(|(i, _)| i + 1)
            }
        }
    }

    /// A short, human display of a path for a navigation notice: relative to the tree root when it
    /// is inside it, else the absolute path.
    fn nav_display_path(&self, abs: &Path) -> String {
        self.rel(abs)
            .unwrap_or_else(|| abs.to_path_buf())
            .display()
            .to_string()
    }
}
