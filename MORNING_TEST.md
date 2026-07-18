# Morning test notes — overnight run

Nathan: this is the running log of what I shipped overnight on `feat/obsidian-media-suite` (pushed
to the `nschwermann` fork). Each section = one feature: **what changed**, **how to test it in
Ghostty**, and **decisions** I made. Work through the test steps at your leisure.

**Setup for every test below:** the plugin is `herdr plugin link`ed to this repo, so a
`cargo build --release` here + closing/reopening the viewer picks up the latest. To refresh:

```
cd ~/Workspace/herdr-file-viewer && cargo build --release
# then in herdr: close the viewer pane (q) and reopen it (Ctrl+Space f)
```

Inline images need `kitty_graphics = true` in `~/.config/herdr/config.toml` `[experimental]` (already
enabled) + a client re-attach if you restart herdr. The matrix shader (`matrix-hallway.glsl`) is
still commented out in your Ghostty config — uncomment line 77 + reload (`kill -USR2 $(pgrep -f Ghostty.app/Contents/MacOS/ghostty)`) to restore it.

Already confirmed working by you earlier: **FIX 1** (inline image/video preview), **FIX 2** (inline
image embeds in rendered markdown), **FIX 4** (fork version `2.0.0` so installs build from source).

---

## FIX 3 — click a link to follow it

**What changed:** in a markdown-in-vault note, left-clicking a `[[wikilink]]`, a `![[embed]]`, or a
standard `[text](note)` link now **follows it** — opens that note in the viewer, exactly like the
`g` link-navigator's select (same vault-wide resolution, and it pushes the `[` / `]` back/forward
history). A click on plain text still just focuses the pane; drag-to-select-text is unchanged.

**How to test:**
1. Open a note that links to another note, e.g. one with a `[[Some Other Note]]`.
2. **Click** directly on the `[[…]]` text (works in the rendered `v` view and the source view).
3. The linked note should open in the content pane. Press `[` to go back, `]` to go forward.
4. Click on ordinary prose (not a link) — it should just focus the pane, nothing else.
5. Click a link whose target doesn't exist — a "Unresolved link: …" notice appears, no navigation.

**Decisions:**
- Gated to markdown-in-vault content, so a bracket pair in a code file is never treated as a link.
- Only the collapsed (no-drag) click follows; a drag still selects/copies text.
- v1 limitation: a link that glow *wraps* across two display lines may not be clickable as a whole
  (the per-line scan sees only half). Use `g` for those. Wikilinks rarely wrap.
- Built the content hit-test (`content_target_at` → `ContentTarget`) as a reusable seam so the
  clickable-tags feature (B) and the interactive status bar (C) can extend it.
