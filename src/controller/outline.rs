//! Heading outline (`o`) — open a jump-through list of the current markdown note's headings and
//! scroll the content pane to the one the user picks. Part of the Session Controller (mirrors
//! `controller/linknav.rs`, minus resolution and the back/forward history).
//!
//! The overlay is a keyboard-only modal ([`Modal::Outline`]) modelled after the wikilink navigator:
//! a flat list of the note's headings (indented by level) with a cursor. Confirming a heading
//! reuses the exact `overrides`/`pending_goto` mechanism go-to-line's auto-switch uses — it switches
//! the note to the source-mapped (line-numbered) view and queues a scroll to the heading's source
//! line, since rendered markdown has no per-line source map to jump within directly. Everything is
//! read-only: it re-renders the current note and moves the scroll, never touching files or git.

use super::*;

/// The largest note we read for heading parsing. A generous bound so a pathological file can never
/// stall the input thread or blow memory; a real note is far smaller. (Mirrors the wikilink
/// navigator's own bounded reader.)
const MAX_NOTE_BYTES: u64 = 1024 * 1024;

/// Read up to [`MAX_NOTE_BYTES`] of `path`, lossily decoded as UTF-8. Returns `None` on any I/O
/// error (never panics), so the caller degrades gracefully to "no outline".
fn read_note_bounded(path: &Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_NOTE_BYTES).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

impl Controller {
    /// The owned outline draw model for the Presenter, or `None` when the overlay is closed.
    /// Projects each stored [`OutlineItem`] into a borrow-free row (text + level), mirroring
    /// [`link_nav_view`](Self::link_nav_view).
    pub(super) fn outline_view(&self) -> Option<OutlineView> {
        let s = self.modal.outline()?;
        Some(OutlineView {
            rows: s
                .items()
                .iter()
                .map(|item| OutlineRowView {
                    text: item.text.clone(),
                    level: item.level,
                })
                .collect(),
            cursor: s.cursor(),
        })
    }

    /// Whether the heading-outline overlay is currently open.
    pub fn outline_open(&self) -> bool {
        self.modal.outline().is_some()
    }

    /// The note the outline is built from / jumps within: the displayed content (`content_path`,
    /// what the user is reading) first, else the selected tree node when it is a file (before any
    /// render has landed).
    fn outline_note_path(&self) -> Option<PathBuf> {
        self.content_path.clone().or_else(|| {
            self.tree
                .selected()
                .filter(|n| n.kind == NodeKind::File)
                .map(|n| n.path)
        })
    }

    /// Open the heading outline (`o`). Gated to a markdown file: the current file is the displayed
    /// content (`content_path`), falling back to the selected tree node. When that is not a markdown
    /// note — or it has no headings — set a one-line notice and open nothing. Otherwise read the note
    /// (bounded), parse its ATX headings, and open the overlay.
    pub(super) fn open_outline(&mut self) -> Effects {
        let Some(current) = self.outline_note_path() else {
            self.action_notice = Some("No outline: open a markdown file".into());
            return Effects::redraw();
        };
        if !crate::obsidian::is_markdown(&current) {
            self.action_notice = Some("No outline: open a markdown file".into());
            return Effects::redraw();
        }
        let Some(source) = read_note_bounded(&current) else {
            self.action_notice = Some("No outline: open a markdown file".into());
            return Effects::redraw();
        };
        let headings = crate::mdnote::headings(&source);
        if headings.is_empty() {
            self.action_notice = Some("No headings in this note".into());
            return Effects::redraw();
        }
        let items: Vec<OutlineItem> = headings
            .into_iter()
            .map(|h| OutlineItem {
                level: h.level,
                text: h.text,
                line: h.line,
            })
            .collect();
        self.modal = Modal::Outline(OutlineState::new(items));
        Effects::redraw()
    }

    /// Route a key while the outline is open: `j`/`Down` and `k`/`Up` move the cursor, `Enter` jumps
    /// to the selected heading, `Esc`/`q` closes. Mirrors `handle_link_nav_key`. A no-op (defensive)
    /// when the outline is not open.
    pub fn handle_outline_key(&mut self, key: KeyEvent) -> Effects {
        let Some(state) = self.modal.outline_mut() else {
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
            KeyCode::Enter => self.jump_to_heading(),
            KeyCode::Esc | KeyCode::Char('q') => {
                self.modal = Modal::None;
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }

    /// Jump the content pane to the selected heading. Closes the overlay, then switches the displayed
    /// note to the source-mapped (line-numbered) view and queues a scroll to the heading's 1-based
    /// source line — the exact `overrides`/`pending_goto` mechanism go-to-line's auto-switch and the
    /// wikilink anchor use (rendered markdown has no per-line source map to jump within, so this
    /// source-mapped view is the consistent target). Best-effort and never panics: if the displayed
    /// path is somehow gone, it just closes.
    fn jump_to_heading(&mut self) -> Effects {
        // Extract the selected heading's source line before dropping the borrow of `self.modal`.
        let Some(line) = self
            .modal
            .outline()
            .and_then(|s| s.selected())
            .map(|item| item.line)
        else {
            return Effects::noop();
        };
        self.modal = Modal::None;
        let Some(path) = self.outline_note_path() else {
            return Effects::redraw();
        };
        // Switch the note to the source-mapped view and queue the jump for when that render lands
        // (poll applies it via scroll_to_line). `pending_goto` is set AFTER `dispatch_render`, which
        // clears any stale pending jump, so this one survives.
        self.overrides.insert(path, ViewMode::SyntaxContent);
        self.dispatch_render();
        self.pending_goto = Some((self.latest_seq, line));
        Effects::redraw()
    }
}
