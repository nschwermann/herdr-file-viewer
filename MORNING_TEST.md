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

---

## A — rendering glowup (callout boxes + highlighted links)

**What changed:** two upgrades to the **rendered markdown view** (`v`) so it reads more like Obsidian.
Both are a new **post-render styling pass** (`src/mdstyle.rs`) that runs over `glow`'s output — we
keep delegating the actual markdown rendering to `glow`, and only re-color the spans it produced.

1. **Highlighted links.** `[[wikilinks]]`, aliased `[[target|alias]]`, note `![[embeds]]`, and
   standard `[text](target)` links now render in a distinct **underlined purple** link colour
   (`rgb(183,148,244)`) instead of `glow`'s plain body colour, so they pop out of the text.
2. **Callout boxes.** Obsidian callouts (`> [!note]`, `> [!tip]`, `> [!warning]`, `> [!danger]`, …)
   now render as a **titled box with a per-type accent colour and a faint filled background tint**:
   an accent-coloured bold title + an accent-coloured left bar, over a subtle dark tint that fills
   the whole block. Accents: note/info/todo = **blue**, tip/summary = **cyan**, success = **green**,
   warning/question = **yellow**, danger/failure/bug = **red**, example = **purple**, quote = gray.
   The type icon, custom title, and foldable `▸`/`▾` marker from before are all kept.

**How to test (Ghostty):**
1. Refresh the plugin:
   ```
   cd ~/Workspace/herdr-file-viewer && cargo build --release
   # then in herdr: close the viewer pane (q) and reopen it (Ctrl+Space f)
   ```
2. Open a note that has both callouts and links — a good one has, e.g.:
   ```markdown
   Body text with a [[Some Note]] and an aliased [[Target Note|nice name]] link,
   a note embed ![[Another Note]], and a [markdown link](Other.md).

   > [!note] Heads up
   > This is a note callout body.

   > [!tip] Pro tip
   > Stay hydrated.

   > [!warning] Careful
   > Watch out for this.

   > [!danger] Do not
   > This is dangerous.

   > A plain blockquote (NOT a callout) — should stay un-tinted.
   ```
   (If you don't have one handy, drop that into a note inside your vault and open it.)
3. In the rendered view (`v`), confirm:
   - The four wikilink/embed/markdown-link forms are **purple + underlined**.
   - Each callout is a **filled tinted box** with an accent title/left-bar in its type colour
     (note=blue, tip=cyan, warning=yellow, danger=red).
   - The **plain blockquote** at the bottom is a normal blockquote — **no tint** (proves we don't
     colour every blockquote, only real callouts).
4. Cycle to the **source view** (`v` again): it should show the raw, **un-styled** markup — the
   glowup is rendered-view only, never the source or diffs.

**Decisions / trade-offs:**
- **Delegate-rendering preserved.** `glow` still does the markdown; `mdstyle` only patches ratatui
  `Style`s onto the already-ingested spans (same span-resegmentation trick as `src/highlight.rs`).
  No new crates, nothing writes your files.
- **Callout detection = glow's blockquote border + our icon/LABEL header.** `mdnote` already rewrites
  `> [!type] title` into `> **<icon> LABEL — title**` before glow; glow renders every blockquote line
  with a `│ ` left border. So `mdstyle` finds a callout by: a `│` line whose content starts with one
  of our icons + an ALL-CAPS label, then tints that line and the following `│` lines until a
  non-blockquote line. This is why a **plain blockquote isn't tinted** and consecutive callouts each
  get their own accent. A drift-guard test renders every Obsidian callout type through `mdnote` and
  asserts `mdstyle` still detects it, so the two can't silently diverge. Custom callout types (glow
  shows them with the `▸` note icon) fall back to the **blue** note accent.
- **Links: wikilinks are matched by literal markup; markdown links by glow's underline.** glow passes
  `[[…]]`/`![[…]]` through verbatim, so we find that markup in the rendered line's plain text (using
  `wikilink::parse_links` on the *pre-glow* source to know which links are real, so markup inside
  code blocks is skipped) and re-color the run. Standard `[text](target)` links are rewritten by glow
  (it splits the text from the URL and underlines the URL) — underline is glow's **only** link-URL
  signal in its dark theme, so we re-color underlined spans to the link colour. Net effect: the URL
  of a markdown link becomes purple+underlined; its link *text* keeps glow's own emphasis.
- **Known small imperfections (documented, not blockers):**
  - We restyle the wikilink markup in place — the `[[ ]]` brackets stay visible (Obsidian hides them
    and shows just the alias). Making them disappear would mean *rewriting* the source, not restyling,
    which the recommended approach avoids.
  - If the *identical* `[[markup]]` appears both as a real link and inside a code block in the same
    note, both get colored (we can't tell prose from code once glow has flattened it). Rare.
  - A wikilink that glow *wraps* across two display lines won't be colored as a whole (same per-line
    limitation as the click-to-follow feature). Wikilinks rarely wrap.
  - The callout background tint is tuned for a **dark** theme (the same one `glow` renders with,
    `-s dark`). On a light terminal the tint would look off — but the viewer already assumes glow's
    dark palette.
- **Colours** live as constants at the top of `src/mdstyle.rs` (`LINK_FG`, the `Accent` fg/bg pairs)
  if you want to tweak the palette later.

---

## B — clickable tags filter the file explorer

**What changed:** in a rendered markdown note inside an Obsidian vault, **left-clicking a `#tag`** now
**filters the left-hand file tree** to every vault note carrying that tag — Obsidian's `tag:` search,
but as a click. It works on both tag surfaces:
- the **`#tag` chips** in the frontmatter **Properties** panel (toggle it with `p`), and
- an **inline `#tag`** in the note body.

While the filter is active the tree shows only the matching notes (with their parent folders,
auto-expanded), and the tree's **top border title changes to `▽ #tag`** so you always see what's
filtering it. Selecting a match renders it normally. **Press `Esc` to clear** the filter and get the
full tree back. Built on the same reusable click hit-test as FIX 3 (`content_target_at` now returns a
`ContentTarget::Tag` alongside `Link`), plus a new vault tag index (`src/tagindex.rs`) and a tree
filter modelled on the existing changed-only filter.

**How to test:**
1. `cd ~/Workspace/herdr-file-viewer && cargo build --release`, then close/reopen the viewer pane.
2. Open a note in your Brain vault that has `tags:` in its frontmatter — e.g. a note tagged
   **`#ryoshi-games`** (or pick any tag you use). Press `p` if the Properties panel isn't showing.
3. In the rendered `v` view, **click the `#ryoshi-games` chip** in the Properties table. The file tree
   on the left should collapse to just the notes carrying that tag; the tree title shows `▽ #ryoshi-games`.
   A one-line notice (`Filtering tree by #ryoshi-games — N notes · Esc to clear`) confirms the count.
4. Click a matching note in the filtered tree — it opens/renders normally.
5. **Press `Esc`** — the full tree comes back and the title reverts to the folder name.
6. Also try an **inline** `#tag` in a note body (not just the Properties chip) — clicking it filters
   the same way.
7. Nested tags: if you have `#project/ryoshi`, clicking the parent `#project` reveals notes tagged
   with any child (`#project/ryoshi`, `#project/foo`) too — Obsidian's behaviour.
8. Edge check: click a tag whose only notes live *outside* the tree root, or a tag with no notes —
   you get a notice and the tree is left unchanged (no empty tree).

**Decisions / limitations:**
- **Tree-root scope (the main limitation).** The tree is rooted at the viewer's root (the worktree /
  cwd). The filter can only *show* notes **under that root** — a note elsewhere in the vault is found
  by the index but not displayed (the notice still counts total matches). If you launch the viewer at
  your **vault root**, every vault note is reachable and this is a non-issue; if you launch it in a
  sub-folder, the filter is scoped to that sub-folder. This is inherent to a root-bounded tree, not a
  bug.
- **Nested tags.** A note tagged `#a/b/c` is indexed under the full tag **and** every ancestor prefix
  (`a`, `a/b`, `a/b/c`), so clicking a parent tag matches its children — matching Obsidian's `tag:`.
- **What counts as a tag.** Frontmatter `tags:` / `tag:` (YAML list or a space/comma-separated
  scalar), plus inline body `#tag`s (`#` + letters/digits/`_`/`-`/`/`). A markdown heading (`# ` with
  a space) and a purely numeric `#123` are **not** tags; inline tags inside fenced code blocks are
  ignored. Tags match **case-insensitively**.
- **Clear gesture.** `Esc` clears it. It's layered into the existing `Esc`/`q` back-out order:
  text-selection → committed in-file search → **tag filter** → un-zoom → quit. So a zoomed, tag-filtered
  pane takes two `Esc`s (one for the filter, one for the zoom). I deliberately did **not** add a new
  remappable key/intent for this — it's a mouse-apply + `Esc`-clear feature — so there's no `[keys]`
  entry and no `?`-overlay row for it. Jumping to an unrelated note (the `f` finder, a followed link)
  also lifts the filter so the target stays reachable (same relax rule the changed-only filter uses).
- **Standalone vs shared index.** The tag index (`src/tagindex.rs`) is **standalone to this feature**
  and cached on the controller, rebuilt on a re-root/refresh. A separate `vault_index` may land later
  (for a quick-switcher); I kept `tagindex` cohesive and small so it can either stay independent or be
  folded into that later without entangling them now.
- **Bonus fix.** While extending the shared click hit-test I found and fixed a pre-existing off-by-one
  in `content_target_at` (it indexed the content lines with a 1-based row), so click-to-follow-links
  (FIX 3) now reads the exact clicked line too. Covered by a new end-to-end click test.
- **Tests:** unit tests for the tag parsing (`src/tagindex.rs`) and the tag-under-caret detection
  (`src/controller/mouse.rs`), a tree-filter test (`tests/tree_filters.rs`), and an end-to-end
  click→filter→`Esc` test (`tests/controller.rs`). Full suite green except the one known-flaky e2e
  search test (fails on the base commit too).
