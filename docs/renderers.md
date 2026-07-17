# External renderers (optional)

Rendering is **delegated** to best-in-class external CLIs. These are *runtime, install-time*
dependencies (not Cargo dependencies) and each is **optional**:

| View | Renderer | Install |
| --- | --- | --- |
| Rendered markdown | [`glow`](https://github.com/charmbracelet/glow) | `brew install glow` / package manager |
| Diffs | [`delta`](https://github.com/dandavison/delta) | `brew install git-delta` / `cargo install git-delta` |
| Syntax-highlighted content | [`bat`](https://github.com/sharkdp/bat) | `brew install bat` / package manager |
| Inline image/video preview | [`chafa`](https://hpjansson.org/chafa/) (recommended), or `kitten` / `timg` / `viu` | `brew install chafa` / package manager |

Or install them all at once with the bundled helper (best-effort; detects brew/apt/dnf/pacman
and falls back to `cargo install` for `delta` and `bat`; `glow` is written in Go, so the helper
prints its manual install link instead of attempting a cargo install), run from the plugin dir
(`herdr plugin list` shows its path):

```bash
./scripts/install-renderers.sh
```

**If a renderer is not installed, the viewer falls back to plain text** and shows a short
notice in the content pane naming the missing capability (e.g. *“Markdown renderer
unavailable (glow: …); showing plain text.”*). The viewer never crashes or shows an empty
pane when a renderer is absent. It degrades gracefully. So the renderers are recommended for
the best experience but not required to use the viewer.

Untrusted file content is always fed to a renderer on **stdin** (never as a command argument),
and the renderer's output is re-sanitized before display, so a hostile file name or file
content cannot inject a command or drive the terminal.

## Inline image & video preview

Image and video files get a **capability-gated** preview. The content pane always shows a clean
placeholder for a media file — its type, dimensions (for images, parsed cheaply from the header),
and size — so you learn what it is without leaving the terminal. When two conditions are met, you
can also preview the media **inline**:

1. **The terminal supports an inline-graphics protocol** — the kitty graphics protocol (Ghostty,
   kitty, WezTerm, Konsole), iTerm2's inline images, or sixel. This is detected from the
   environment; an unknown terminal is treated as incapable (so no escape sequences are ever sent
   to a terminal that can't render them).
2. **A backend CLI is installed** — the first of [`kitten`](https://sw.kovidgoyal.net/kitty/kittens/icat/)
   (kitty), [`chafa`](https://hpjansson.org/chafa/), [`timg`](https://github.com/hzeller/timg), or
   [`viu`](https://github.com/atanunq/viu) on `PATH` (in that priority). **`chafa` is recommended**
   — it auto-detects the protocol and degrades to Unicode symbols on its own.

When both hold, the placeholder invites you to press **`Enter`** to view; the image (or, for a
video, a poster frame extracted with [`ffmpeg`](https://ffmpeg.org/)/`ffmpegthumbnailer` at ~10% of
the duration) is painted over the terminal, and any key returns you to the viewer. When either
condition is missing — including **no backend installed** — the viewer simply shows the file-info
placeholder and never crashes or emits raw escape bytes. Detection is cheap and cached. Turn the
whole feature off with `media_preview = false` in [config](configuration.md).

### Bundled markdown palette

The viewer ships a small bundled markdown style palette (`assets/markdown-style.json`) that
`glow` is pointed at when it is present, so rendered markdown uses a consistent set of named
ANSI colors (headings, code blocks, links, etc.) rather than glow's built-in `dark` style.
When the palette file is absent, glow falls back to its built-in `dark` style. Markdown still
renders, just with glow's default colors. The palette is a trusted glow argument (located only
inside the plugin's own dirs), never derived from untrusted input.
