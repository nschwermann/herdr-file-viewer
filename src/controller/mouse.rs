//! Column/tree pointer handling — wheel scroll, press/drag on the divider and scrollbars
//! (click-to-scroll), click vs double-click, and the scrollbar track\u2192offset math. Routed here
//! from `handle_mouse` for the non-modal case. Part of the Session Controller (M6 split).

use super::*;

impl Controller {
    /// Map a mouse event to a state change. Mouse is additive to the keyboard-first design
    /// (AC-18). A `Shift`+mouse event is left untouched so the terminal's own selection/copy
    /// still works (herdr reserves Shift+mouse for exactly that). Selection/activation happen
    /// on button *release*, so a divider drag is never mistaken for a click.
    pub fn handle_mouse(&mut self, ev: MouseEvent) -> Effects {
        // Modal gate, exhaustive over `Modal` (so a new overlay variant forces a mouse-routing
        // decision here, mirroring the keyboard gate in `handle`):
        // - Picker / Prompt are keyboard-only — swallow the mouse entirely. Without this a
        //   click/wheel would reach the tree/content beneath and change the selection under the
        //   modal, so a later confirm would act on the WRONG file (or strand an override on a dir).
        // - LineSelect owns the mouse over the content pane: route to its own handler (BEFORE the
        //   column handler) so a click places the marker and never leaks to the columns or
        //   starts a divider/scrollbar drag while the mode is active (AC-8/AC-9/AC-12).
        // - Help / Finder ARE mouse-interactive (wheel scrolls, click selects/switches): route to
        //   their own handler, which consumes every event and never leaks to the columns (AC-21).
        // - None: no modal → the two-column mouse handler below.
        match self.modal {
            Modal::Picker(_)
            | Modal::Prompt(_)
            | Modal::Annotations(_)
            | Modal::AnnotationEditor(_)
            // The wikilink navigator and heading outline are keyboard-only (like the prompt):
            // swallow the mouse so a click/wheel never reaches the tree/content beneath and moves
            // the selection under it.
            | Modal::LinkNav(_)
            | Modal::Outline(_)
            // The vault quick-switcher and global content search are keyboard-only too — swallow it.
            | Modal::QuickSwitcher(_)
            | Modal::GlobalSearch(_)
            | Modal::DiscardConfirm(_) => Effects::noop(),
            Modal::LineSelect(_) => self.handle_line_select_mouse(ev),
            Modal::Help(_) => self.handle_help_mouse(ev),
            Modal::Finder(_) => self.handle_finder_mouse(ev),
            Modal::None => self.handle_column_mouse(ev),
        }
    }

    /// Mouse handling while line-select mode owns the pointer. A left **press** in the content pane
    /// drops the selection caret on the character under the cursor; **dragging** extends a
    /// character-granular selection (scrolling the pane when the drag runs past an edge); the
    /// **release** finalizes it, leaving the selection standing for `Enter` (the `path:line`
    /// reference) or `y` (the content) to confirm. A press with no drag collapses the selection to
    /// that character — click-then-`y` still copies the clicked line (the content path's
    /// collapsed-selection fallback) and click-then-`Enter` its reference. Works under wrap too:
    /// `char_at_content_col` maps the clicked row through the same break-position simulation the
    /// wrapped scroll math uses, so the caret lands on the character actually under the cursor.
    ///
    /// `Shift`+mouse is deliberately left untouched (returned inert) so the terminal's OWN native
    /// selection/copy still works — herdr reserves `Shift`+drag for exactly that, and we don't want
    /// to swallow it. Every non-left event (wheel, other buttons) is inert so nothing leaks to the
    /// columns beneath while the mode holds the mouse.
    fn handle_line_select_mouse(&mut self, ev: MouseEvent) -> Effects {
        // Leave Shift+mouse for the terminal's native selection — never swallow it.
        if ev.modifiers.contains(KeyModifiers::SHIFT) {
            return Effects::noop();
        }
        let (col, row) = (ev.column, ev.row);
        let last = self.content.lines.len().max(1);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // A press outside the content region is inert and drops any in-flight drag.
                if self.hit_test(col, row) != MouseRegion::Content {
                    self.drag = None;
                    return Effects::noop();
                }
                let (line, caret) = self.char_at_content_col(col, row);
                if let Some(state) = self.modal.line_select_mut() {
                    state.begin_char(line, caret, last);
                }
                self.drag = Some(Drag::ContentSelect); // subsequent drags extend the selection
                Effects::redraw()
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                // Only extend when this drag began as a text selection (a press inside content);
                // otherwise it's a stray drag we don't own.
                if !matches!(self.drag, Some(Drag::ContentSelect)) {
                    return Effects::noop();
                }
                let (line, caret) = self.char_at_content_col(col, row);
                if let Some(state) = self.modal.line_select_mut() {
                    state.drag_char(line, caret, last);
                }
                self.autoscroll_selection(row); // follow the cursor past a viewport edge
                Effects::redraw()
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // Finalize only a selection drag; a stray release is inert (so it can't be mistaken
                // for a click that changes state).
                if matches!(self.drag, Some(Drag::ContentSelect)) {
                    self.drag = None;
                    return Effects::redraw();
                }
                Effects::noop()
            }
            _ => Effects::noop(),
        }
    }

    /// While dragging out a selection, nudge the content pane one line when the cursor is above the
    /// top or below the bottom of the text viewport, so the selection can extend past what's on
    /// screen. Only fires at the edges; a drag inside the viewport leaves the scroll alone.
    fn autoscroll_selection(&mut self, row: u16) {
        let Some(c) = self.geom.content_inner else {
            return;
        };
        if row < c.y {
            self.scroll_content(-1);
        } else if row >= c.y + c.height {
            self.scroll_content(1);
        }
    }

    /// Handle a mouse event over the two columns, with no modal open (the [`handle_mouse`] gate
    /// routes here only for [`Modal::None`]). Shift+mouse is inert so the terminal can do its own
    /// text selection; otherwise the wheel scrolls the column under the pointer, a left press
    /// starts a divider or scrollbar drag (scrollbars also jump to the pressed position), and a
    /// left release is a click — unless it ended a drag, in which case it's consumed.
    fn handle_column_mouse(&mut self, ev: MouseEvent) -> Effects {
        if ev.modifiers.contains(KeyModifiers::SHIFT) {
            return Effects::noop();
        }
        let (col, row) = (ev.column, ev.row);
        match ev.kind {
            MouseEventKind::ScrollDown => self.scroll_at(col, row, self.wheel_step),
            MouseEventKind::ScrollUp => self.scroll_at(col, row, -self.wheel_step),
            MouseEventKind::ScrollRight => self.hscroll_at(col, row, HSCROLL_STEP as i32),
            MouseEventKind::ScrollLeft => self.hscroll_at(col, row, -(HSCROLL_STEP as i32)),
            MouseEventKind::Down(MouseButton::Left) => {
                // A press in the content pane begins an ambient character selection; on the divider a
                // resize drag; on a scrollbar a scroll drag AND a jump to the pressed position
                // (click-to-scroll). Anything else waits for the release (a click) and drops a standing
                // selection (click-away deselect). Always (re)set `drag` from the press — so a stale
                // drag from a release we never saw (e.g. swallowed by a modal) can't act on later moves.
                let region = self.hit_test(col, row);
                if region == MouseRegion::Content {
                    // Seed a fresh collapsed char selection at the pressed caret; the Drag arm
                    // extends it and Up finalizes (collapsed ⇒ a click; non-collapsed ⇒ copy).
                    // `char_at_content_col` is wrap-aware, so this works on wrapped prose too.
                    self.last_click = None;
                    self.focus = Focus::Content;
                    self.drag = None;
                    // But not over the "Rendering…" placeholder: a file *is* selected mid-render, so
                    // the is-file guard in `copy_content_selection` wouldn't stop a drag from copying
                    // the placeholder text. (The press still focuses the pane.)
                    if self.content_rendering {
                        return Effects::redraw();
                    }
                    let (line, caret) = self.char_at_content_col(col, row);
                    let last = self.content.lines.len().max(1);
                    let mut sel = LineSelectState::new(line);
                    sel.begin_char(line, caret, last);
                    self.content_selection = Some(sel);
                    self.drag = Some(Drag::ContentSelect);
                    return Effects::redraw();
                }
                // A press anywhere outside the content region drops a standing ambient selection.
                let had_selection = self.content_selection.take().is_some();
                self.drag = match region {
                    MouseRegion::Divider => Some(Drag::Divider),
                    MouseRegion::ContentVBar => Some(Drag::ContentV),
                    MouseRegion::ContentHBar => Some(Drag::ContentH),
                    MouseRegion::TreeVBar => Some(Drag::TreeV),
                    MouseRegion::TreeHBar => Some(Drag::TreeH),
                    _ => None,
                };
                let fx = match region {
                    MouseRegion::ContentVBar => self.scroll_content_to_row(row),
                    MouseRegion::ContentHBar => self.scroll_content_h_to_col(col),
                    MouseRegion::TreeVBar => self.scroll_tree_to_row(row),
                    MouseRegion::TreeHBar => self.scroll_tree_h_to_col(col),
                    _ => Effects::noop(),
                };
                // A cleared selection needs a repaint even when the press itself was inert.
                if had_selection { Effects::redraw() } else { fx }
            }
            MouseEventKind::Drag(MouseButton::Left) => match self.drag {
                Some(Drag::Divider) => self.resize_split_to_col(col),
                Some(Drag::ContentV) => self.scroll_content_to_row(row),
                Some(Drag::ContentH) => self.scroll_content_h_to_col(col),
                Some(Drag::TreeV) => self.scroll_tree_to_row(row),
                Some(Drag::TreeH) => self.scroll_tree_h_to_col(col),
                // The finder is modal: its scrollbar drag is handled in handle_finder_mouse and
                // never reaches this (non-finder) path. Covered here only for exhaustiveness.
                Some(Drag::FinderV) => Effects::noop(),
                // Extend the ambient selection to the dragged caret + autoscroll past a viewport edge
                // — the L-mode drag, but on the Modal-independent `content_selection`.
                Some(Drag::ContentSelect) => {
                    let (line, caret) = self.char_at_content_col(col, row);
                    let last = self.content.lines.len().max(1);
                    if let Some(sel) = self.content_selection.as_mut() {
                        sel.drag_char(line, caret, last);
                    }
                    self.autoscroll_selection(row);
                    Effects::redraw()
                }
                None => Effects::noop(),
            },
            MouseEventKind::Up(MouseButton::Left) => match self.drag.take() {
                // End of an ambient drag. Still collapsed ⇒ it was a plain click (no Drag events):
                // drop it and just focus, as a bare content click did. Non-collapsed ⇒ auto-copy,
                // keeping the highlight.
                Some(Drag::ContentSelect) => {
                    let collapsed = self
                        .content_selection
                        .as_ref()
                        .map(|s| {
                            let (a, b) = s.char_span();
                            a == b
                        })
                        .unwrap_or(true);
                    self.last_click = None;
                    if collapsed {
                        // A plain click (no drag): if it landed on a followable link in a markdown
                        // note, follow it (FIX 3); otherwise it's a bare click — drop the collapsed
                        // selection and just focus the pane, as before.
                        self.content_selection = None;
                        if let Some(target) = self.content_target_at(col, row) {
                            return self.follow_content_target(target);
                        }
                        self.focus = Focus::Content;
                        Effects::redraw()
                    } else {
                        self.copy_content_selection()
                    }
                }
                // End of a divider/scrollbar drag, not a click. Clear the pending-click so a
                // tree-row click made before the drag can't pair with a later one as a double-click
                // — the drag may have scrolled the viewport, so the same screen row now maps to a
                // different node.
                Some(_) => {
                    self.last_click = None;
                    Effects::noop()
                }
                None => self.handle_click(col, row),
            },
            _ => Effects::noop(),
        }
    }

    /// A completed left-click: select the tree row it landed on (or focus the content pane). A
    /// double-click [`activate`](Self::activate)s the row — a directory toggles expand/collapse,
    /// a file opens in zoom mode (the editor hand-off is the `e` key, not the mouse).
    fn handle_click(&mut self, col: u16, row: u16) -> Effects {
        // The content pane's bottom-border status bar rides OUTSIDE `content_inner` (it's on the
        // border row), so these clicks would otherwise fall through to the inert `Outside` arm.
        // Check the two interactive chips first — same effect as the `v` / `g` keys, keyboard
        // behavior unchanged. `last_click` is cleared so a chip click never pairs into a
        // double-click on a later tree-row click.
        let pos = Position { x: col, y: row };
        if self.geom.status_view_rect.is_some_and(|r| r.contains(pos)) {
            self.last_click = None;
            return self.cycle_view(); // like pressing `v`
        }
        if self.geom.status_links_rect.is_some_and(|r| r.contains(pos)) {
            self.last_click = None;
            return self.open_link_nav(); // like pressing `g`
        }
        let region = self.hit_test(col, row);
        let now = Instant::now();
        match region {
            MouseRegion::TreeRow(idx) => {
                if idx >= self.tree.visible_nodes().len() {
                    self.last_click = None; // empty area below the nodes — inert, and breaks any
                    return Effects::noop(); // pending double-click sequence
                }
                // A double-click is two clicks on the SAME tree row within the window. Because
                // every non-tree-row click clears `last_click` (below), AND the finder's
                // open/confirm/Esc paths also clear it, `last_click` only ever holds a prior
                // tree-row click — the column-agnostic same-row match in `is_double_click`
                // cannot be tripped by a click in a different context (another pane or the finder).
                let double = is_double_click(self.last_click, (col, row), now);
                self.last_click = Some((col, row, now));
                self.action_notice = None;
                self.focus = Focus::Tree;
                self.tree.set_cursor(idx);
                self.dispatch_render(); // selection changed → re-render the content pane
                if double {
                    return self.activate(); // folder → expand/collapse, file → zoom mode
                }
                Effects::redraw()
            }
            MouseRegion::Content => {
                self.last_click = None; // a non-tree click breaks any pending double-click
                self.focus = Focus::Content;
                Effects::redraw()
            }
            // Scrollbars are handled on press/drag (above), not as a click; reaching here is inert.
            MouseRegion::Divider
            | MouseRegion::ContentVBar
            | MouseRegion::ContentHBar
            | MouseRegion::TreeVBar
            | MouseRegion::TreeHBar
            | MouseRegion::Outside => {
                self.last_click = None;
                Effects::noop()
            }
        }
    }

    /// What followable markup, if any, sits under a content-pane click at screen `(col, row)`. The
    /// reusable content hit-test behind FIX 3 (link-following) and the clickable-tag filter: map the
    /// click to a content line + character caret (`char_at_content_col`, wrap/scroll-aware), read
    /// that display line's text, and scan it for markup covering the caret — a link first, then a
    /// `#tag` token. Gated to a markdown note, so a bracket pair or a `#` in code (or a non-note
    /// file) is never treated as a link/tag. Read-only.
    pub(super) fn content_target_at(&self, col: u16, row: u16) -> Option<ContentTarget> {
        let current = self.content_path.as_ref()?;
        if !crate::obsidian::is_markdown(current) {
            return None;
        }
        // `char_at_content_col` returns a 1-based display line (its line-select contract); index the
        // 0-based `content.lines` with `line - 1` so the scanned text is the line actually under the
        // cursor. (`line` is clamped to `[1, last]`, so `checked_sub` only guards the empty pane.)
        let (line, caret) = self.char_at_content_col(col, row);
        let text: String = self
            .content
            .lines
            .get(line.checked_sub(1)?)?
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        // A link wins over a tag when both could match (a link's anchor `[[Note#heading]]` embeds a
        // `#`); the caret is inside the link span, so the link check claims it first.
        if let Some(link) = link_at_char(&text, caret) {
            return Some(ContentTarget::Link(link));
        }
        tag_at_char(&text, caret).map(ContentTarget::Tag)
    }

    /// Act on a content-click [`ContentTarget`]: follow a link to its note, or apply a tag filter
    /// to the tree.
    pub(super) fn follow_content_target(&mut self, target: ContentTarget) -> Effects {
        match target {
            ContentTarget::Link(link) => self.follow_content_link(&link),
            ContentTarget::Tag(tag) => self.apply_tag_filter(&tag),
        }
    }

    /// Scroll the pane under the cursor: the content pane scrolls vertically; over the tree the
    /// wheel moves the selection (the tree then scrolls to keep it in view, #45).
    fn scroll_at(&mut self, col: u16, row: u16, delta: isize) -> Effects {
        match self.hit_test(col, row) {
            MouseRegion::Content => {
                self.scroll_content(delta);
                Effects::redraw()
            }
            MouseRegion::TreeRow(_) => {
                self.focus = Focus::Tree;
                self.tree.move_cursor(delta.signum());
                self.dispatch_render();
                Effects::redraw()
            }
            _ => Effects::noop(),
        }
    }

    /// Horizontal wheel / trackpad swipe scrolls sideways: the content pane (like the `←`/`→`
    /// keys, for unwrapped long lines) or the tree (like the `H`/`L` keys). Each clamps to
    /// `[0, widest − viewport]`, so it is inert when nothing overflows.
    fn hscroll_at(&mut self, col: u16, row: u16, delta: i32) -> Effects {
        match self.hit_test(col, row) {
            MouseRegion::Content => self.scroll_content_h(delta),
            MouseRegion::TreeRow(_) => self.scroll_tree_h(delta),
            _ => Effects::noop(),
        }
    }

    /// Scroll the tree horizontally by `delta` columns, clamped to `[0, widest − tree width]` from
    /// the last drawn frame, so a long / deeply-nested row can be read sideways without ever
    /// over-scrolling past the content.
    fn scroll_tree_h(&mut self, delta: i32) -> Effects {
        let max = self
            .geom
            .tree_inner
            .map_or(0, |t| self.geom.tree_content_width.saturating_sub(t.width));
        let next = (self.tree_hscroll as i32 + delta).clamp(0, max as i32);
        self.tree_hscroll = next as u16;
        Effects::redraw()
    }

    /// The keyboard path for tree horizontal scroll (AC-18): `H`/`L` move `tree_hscroll` by the
    /// same step the mouse wheel uses, clamped to the measured max — mirroring how the content
    /// pane's `←`/`→` scroll `content_hscroll`. Inert unless the tree is focused, so the keys
    /// never fight the content pane's own horizontal scroll when the content is focused.
    pub(super) fn scroll_tree_h_focus(&mut self, delta: i32) -> Effects {
        if self.focus != Focus::Tree {
            return Effects::noop();
        }
        self.scroll_tree_h(delta)
    }

    /// The fraction `[0,1]` of a press/drag along a scrollbar track of `len` cells starting at
    /// `start`, as a rounding numerator/denominator: returns `(rel, span)` so callers stay in
    /// integer math (`offset = round(rel/span * max)`). `span` is 0 for a degenerate 1-cell track.
    pub(super) fn track_fraction(pos: u16, start: u16, len: u16) -> (u32, u32) {
        let rel = pos.saturating_sub(start).min(len.saturating_sub(1)) as u32;
        (rel, len.saturating_sub(1) as u32)
    }

    /// Map a press/drag at `pos` on a scrollbar track `[start, start + len)` onto an offset in
    /// `[0, max]`, rounded to the nearest integer. `None` (the caller no-ops) when the mapping is
    /// degenerate — a 1-cell track (`span == 0`) or a zero range (`max == 0`). The single
    /// track→offset rounding rule every linear scrollbar drag shares: a vertical bar passes the
    /// row + track `y`/`height`, a horizontal bar the col + track `x`/`width`. (The finder maps a
    /// drag to a *match index* and intentionally differs at the degenerate boundary, so it keeps
    /// its own mapping — see `finder_scroll_to_row`.)
    fn track_to_offset(pos: u16, start: u16, len: u16, max: u32) -> Option<u32> {
        let (rel, span) = Self::track_fraction(pos, start, len);
        if span == 0 || max == 0 {
            return None;
        }
        Some((rel * max + span / 2) / span)
    }

    /// Map a vertical press/drag on the content scrollbar track to a content scroll offset. The
    /// track is the fed-back `content_vbar` rect; the fraction maps linearly onto
    /// `[0, max_content_scroll]`, rounded to the nearest line. No-op without overflow.
    fn scroll_content_to_row(&mut self, row: u16) -> Effects {
        let Some(track) = self.geom.content_vbar else {
            return Effects::noop();
        };
        let Some(off) =
            Self::track_to_offset(row, track.y, track.height, self.max_content_scroll() as u32)
        else {
            return Effects::noop();
        };
        self.content_scroll = off as u16;
        Effects::redraw()
    }

    /// Map a horizontal press/drag on the content horizontal scrollbar to a content h-scroll offset.
    fn scroll_content_h_to_col(&mut self, col: u16) -> Effects {
        let Some(track) = self.geom.content_hbar else {
            return Effects::noop();
        };
        let Some(off) =
            Self::track_to_offset(col, track.x, track.width, self.max_content_hscroll() as u32)
        else {
            return Effects::noop();
        };
        self.content_hscroll = off as u16;
        Effects::redraw()
    }

    /// Map a horizontal press/drag on the tree's horizontal scrollbar to a tree h-scroll offset.
    fn scroll_tree_h_to_col(&mut self, col: u16) -> Effects {
        let Some(track) = self.geom.tree_hbar else {
            return Effects::noop();
        };
        let max = self.geom.tree_content_width.saturating_sub(track.width);
        let Some(off) = Self::track_to_offset(col, track.x, track.width, max as u32) else {
            return Effects::noop();
        };
        self.tree_hscroll = off as u16;
        Effects::redraw()
    }

    /// Map a vertical press/drag on the tree's vertical scrollbar to a selection — scrubbing the
    /// cursor through the file list, which scrolls the tree to keep it in view (the tree has no
    /// independent vertical offset; its position follows the selection, #45).
    fn scroll_tree_to_row(&mut self, row: u16) -> Effects {
        let Some(track) = self.geom.tree_vbar else {
            return Effects::noop();
        };
        let len = self.tree.visible_nodes().len();
        // `max = len - 1` (the last index): a 1-cell track or a list of ≤ 1 node yields `None` here,
        // exactly the old `span == 0 || len <= 1` no-op.
        let Some(idx) =
            Self::track_to_offset(row, track.y, track.height, len.saturating_sub(1) as u32)
        else {
            return Effects::noop();
        };
        let idx = idx as usize;
        self.focus = Focus::Tree;
        // A drag fires many events on the same row; only re-select (and re-render the content, an
        // expensive job) when the target actually changes, so a held scrub doesn't re-render the
        // same file every tick.
        if idx == self.tree.cursor() {
            return Effects::redraw();
        }
        self.tree.set_cursor(idx);
        self.dispatch_render();
        Effects::redraw()
    }

    /// During a divider drag, set the split so the divider tracks the cursor column — clamped
    /// like the keyboard resize so neither column can collapse. The tree width is measured from
    /// whichever edge the tree hugs, so a drag toward the content pane always *grows* the tree
    /// regardless of the side: from the left edge when the tree is on the left, and from the right
    /// edge when it is on the right.
    fn resize_split_to_col(&mut self, col: u16) -> Effects {
        if self.geom.area_width == 0 {
            return Effects::noop();
        }
        // A drag is an explicit resize: lift the `tree_max_cols` cap (the drag below sets `split_pct`
        // straight from the cursor, so there is no jump — the divider is grabbed at its drawn,
        // possibly-capped, position).
        self.engage_manual_split();
        let tree_w = match self.tree_position {
            crate::config::TreePosition::Left => col.saturating_sub(self.geom.area_x) as i32,
            crate::config::TreePosition::Right => {
                let right_edge = self.geom.area_x as i32 + self.geom.area_width as i32;
                (right_edge - col as i32).max(0)
            }
        };
        let pct = (tree_w * 100 / self.geom.area_width as i32)
            .clamp(self.split_floor_pct() as i32, SPLIT_MAX as i32);
        self.split_pct = pct as u16;
        Effects::redraw()
    }

    /// Which region of the last-drawn frame a cell falls in. The divider is checked first (it
    /// sits between the columns); a tree click maps to a visible node index by its row.
    fn hit_test(&self, col: u16, row: u16) -> MouseRegion {
        if let Some(dx) = self.geom.divider_x
            && (col == dx || col + 1 == dx)
        {
            return MouseRegion::Divider;
        }
        // Scrollbars live INSIDE the panes (a reserved gutter), fed back as 1-cell track rects that
        // are present only when that bar is drawn — so a hit on a `Some` track is a real bar. Check
        // them before the text rects. The tree's vertical bar no longer shares the divider column.
        let pos = Position { x: col, y: row };
        if self.geom.content_vbar.is_some_and(|r| r.contains(pos)) {
            return MouseRegion::ContentVBar;
        }
        if self.geom.content_hbar.is_some_and(|r| r.contains(pos)) {
            return MouseRegion::ContentHBar;
        }
        if self.geom.tree_vbar.is_some_and(|r| r.contains(pos)) {
            return MouseRegion::TreeVBar;
        }
        if self.geom.tree_hbar.is_some_and(|r| r.contains(pos)) {
            return MouseRegion::TreeHBar;
        }
        if let Some(t) = self.geom.tree_inner
            && t.contains(pos)
        {
            // Map the screen row to the node actually drawn there: the on-screen offset plus the
            // tree's scroll offset (#45), the same value `draw_tree` scrolled by. The row index may
            // still exceed the node count (the empty area below the last node): the click handler
            // treats that as inert, while the wheel still scrolls the column.
            return MouseRegion::TreeRow((row - t.y) as usize + self.geom.tree_scroll as usize);
        }
        if let Some(c) = self.geom.content_inner
            && c.contains(Position { x: col, y: row })
        {
            return MouseRegion::Content;
        }
        MouseRegion::Outside
    }
}

/// A followable target discovered under a content-pane click — the reusable content hit-test
/// result behind FIX 3. A closed set so each clickable-markup kind forces a routing decision in
/// [`Controller::follow_content_target`].
pub(super) enum ContentTarget {
    /// A wikilink / markdown link / note embed to follow to another note.
    Link(crate::wikilink::Link),
    /// An Obsidian `#tag` (a Properties-panel chip or an inline body hashtag), without its leading
    /// `#` — clicking it filters the tree to every vault note carrying that tag.
    Tag(String),
}

/// The `#tag` token covering character index `caret` in one displayed line `text`, if any —
/// returned **without** its leading `#` (e.g. `project/ryoshi`). A token is a `#` at a word
/// boundary (not preceded by a tag char or another `#`) followed by tag characters
/// (`[A-Za-z0-9_/-]`), rejecting a purely numeric run (Obsidian's inline-tag rule). The caret must
/// fall on the `#` or any of the tag characters. This scans the *displayed* line text, so it finds
/// a `#tag` chip in the rendered Properties panel (glow renders it as inline code, so its plain
/// text is `#tag`) and an inline body hashtag alike. Pure — the same rules as
/// [`crate::tagindex`]'s inline scanner, applied to a single caret.
fn tag_at_char(text: &str, caret: usize) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let is_tag_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/');
    let mut i = 0;
    while i < n {
        if chars[i] == '#' {
            let boundary_ok = i == 0 || !(is_tag_char(chars[i - 1]) || chars[i - 1] == '#');
            let mut j = i + 1;
            while j < n && is_tag_char(chars[j]) {
                j += 1;
            }
            // A real tag (at least one char after `#`) at a boundary, with the caret on `#..end`.
            if boundary_ok && j > i + 1 && caret >= i && caret < j {
                let raw: String = chars[i + 1..j].iter().collect();
                // Reject a purely numeric run (`#123`) — not a tag, matching the index's rule.
                if !raw.chars().all(|c| c.is_ascii_digit() || c == '/')
                    && !raw.starts_with('/')
                    && !raw.ends_with('/')
                    && !raw.contains("//")
                {
                    return Some(raw);
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

/// The followable link whose span covers character index `caret` in one displayed line `text`, if
/// any. Parses the line as markdown so `[[wikilinks]]`, `![[embeds]]`, and `[text](target)` are all
/// recognized; returns the first whose byte span contains the caret's byte offset. Pure.
fn link_at_char(text: &str, caret: usize) -> Option<crate::wikilink::Link> {
    let byte = text
        .char_indices()
        .nth(caret)
        .map(|(b, _)| b)
        .unwrap_or(text.len());
    crate::wikilink::parse_links(text)
        .into_iter()
        .find(|l| l.span.contains(&byte))
}

/// Two left-clicks at the same cell within this window are a double-click (a folder toggles
/// expand/collapse; a file opens in zoom mode — the editor hand-off is the `e` key).
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Two left-clicks on the same **row** within [`DOUBLE_CLICK`] are a double-click. The column
/// is ignored on purpose: a tree row is a single node end-to-end, so a click anywhere along it
/// targets that node, and a touchpad double-tap commonly lands a column or two apart between
/// taps — requiring the exact cell would silently drop those. (The column still matters for
/// *which* node a click selects; that is the caller's hit-test, not this timing rule.) Pure over
/// its timestamps so the timing rule is unit-testable without sleeping.
pub(super) fn is_double_click(
    prev: Option<(u16, u16, Instant)>,
    pos: (u16, u16),
    now: Instant,
) -> bool {
    matches!(prev, Some((_px, py, t)) if py == pos.1 && now.saturating_duration_since(t) <= DOUBLE_CLICK)
}

#[cfg(test)]
mod tests {
    use super::{DOUBLE_CLICK, is_double_click, link_at_char, tag_at_char};
    use std::time::Instant;

    #[test]
    fn tag_at_char_finds_the_tag_under_the_caret() {
        // A rendered Properties chip line (glow strips the backticks → plain `#tag` text).
        let text = "tags   #ryoshi-games   #project/ryoshi";
        // Caret inside `#ryoshi-games` (on the `r`).
        assert_eq!(
            tag_at_char(text, 8),
            Some("ryoshi-games".to_string()),
            "caret inside the first chip"
        );
        // Caret on the leading `#` of the first chip.
        assert_eq!(tag_at_char(text, 7), Some("ryoshi-games".to_string()));
        // Caret inside the nested `#project/ryoshi`.
        assert_eq!(tag_at_char(text, 25), Some("project/ryoshi".to_string()));
        // Caret in plain prose (the `tags` label) → nothing.
        assert!(tag_at_char(text, 1).is_none());
    }

    #[test]
    fn tag_at_char_rejects_headings_numbers_and_midword_hashes() {
        // A markdown heading (`# ` with a space) is not a tag — the space ends the token.
        assert!(tag_at_char("# Heading", 0).is_none());
        assert!(tag_at_char("# Heading", 2).is_none());
        // A purely numeric hashtag is a number, not a tag.
        assert!(tag_at_char("see #123 here", 5).is_none());
        // A `#` glued to a preceding word char is not a tag boundary.
        assert!(tag_at_char("email a#b c", 7).is_none());
        // But a boundary `#tag` (after a space) IS a tag.
        assert_eq!(tag_at_char("do #wip now", 4), Some("wip".to_string()));
    }

    #[test]
    fn link_at_char_finds_the_link_under_the_caret() {
        let text = "see [[Some Note]] and [x](other.md) here";
        // Caret inside `[[Some Note]]`.
        assert_eq!(
            link_at_char(text, 6).map(|l| l.target),
            Some("Some Note".to_string())
        );
        // Caret inside the markdown link `[x](other.md)`.
        assert_eq!(
            link_at_char(text, 23).map(|l| l.target),
            Some("other.md".to_string())
        );
        // Caret in plain prose → nothing to follow.
        assert!(link_at_char(text, 1).is_none());
        assert!(link_at_char(text, 39).is_none());
    }

    #[test]
    fn link_at_char_handles_wiki_embed_and_alias() {
        // A note embed `![[Note]]` is followable; an alias link resolves to its target.
        assert_eq!(
            link_at_char("![[Diagram]]", 5).map(|l| l.target),
            Some("Diagram".to_string())
        );
        assert_eq!(
            link_at_char("[[Target|shown]]", 3).map(|l| l.target),
            Some("Target".to_string())
        );
    }

    #[test]
    fn is_double_click_requires_the_same_row_within_the_window() {
        let t0 = Instant::now();
        let within = t0 + DOUBLE_CLICK / 2;
        let after = t0 + DOUBLE_CLICK * 2;
        // Same cell, inside the window → double-click.
        assert!(is_double_click(Some((5, 5, t0)), (5, 5), within));
        // Same ROW, different column, inside the window → still a double-click. A tree row is
        // one node end-to-end, and a touchpad double-tap often lands a column or two apart, so
        // requiring the exact cell would drop legitimate double-taps.
        assert!(is_double_click(Some((5, 5, t0)), (40, 5), within));
        // Too slow → not a double-click.
        assert!(!is_double_click(Some((5, 5, t0)), (5, 5), after));
        // A different ROW → not a double-click (it would target a different node).
        assert!(!is_double_click(Some((5, 5, t0)), (5, 6), within));
        // No previous click → never a double-click.
        assert!(!is_double_click(None, (5, 5), within));
    }
}
