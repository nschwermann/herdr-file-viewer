# External renderers (optional)

Rendering is **delegated** to best-in-class external CLIs. These are *runtime, install-time*
dependencies (not Cargo dependencies) and each is **optional**:

| View | Renderer | Install |
| --- | --- | --- |
| Rendered markdown | [`glow`](https://github.com/charmbracelet/glow) | `brew install glow` / package manager |
| Diffs | [`delta`](https://github.com/dandavison/delta) | `brew install git-delta` / `cargo install git-delta` |
| Syntax-highlighted content | [`bat`](https://github.com/sharkdp/bat) | `brew install bat` / package manager |
| Inline **video** poster frame | [`ffmpeg`](https://ffmpeg.org/) (or `ffmpegthumbnailer`) | `brew install ffmpeg` / package manager |

Inline **image** preview needs no external CLI — images are decoded and encoded in-process (via
the bundled [`ratatui-image`](https://crates.io/crates/ratatui-image) library); only a
graphics-capable terminal is required. `ffmpeg` is only for extracting a **video's** poster frame.

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

Image and video files render **inline in the content pane**, automatically — as soon as the file is
highlighted in the tree, with **no keypress**. The image is drawn scaled-to-fit right in the pane
(not over a suspended terminal), with the file's metadata (type, dimensions, size) shown above it.
Press **`Enter`** (or `z`) to zoom the pane for a larger view, like any other file.

This works when the terminal supports an **inline-graphics protocol** — the kitty graphics protocol
(Ghostty, kitty, WezTerm, Konsole), iTerm2's inline images, or sixel — detected from the
environment. The pixels are decoded and encoded **in-process** by the bundled
[`ratatui-image`](https://crates.io/crates/ratatui-image) library, so **no external image CLI**
(chafa/kitten/timg/viu) is needed. For a **video**, a poster frame is extracted with
[`ffmpeg`](https://ffmpeg.org/) (or `ffmpegthumbnailer`) at ~10% of the duration, then rendered
through the same inline image path.

On a terminal with no graphics protocol — or a video with no `ffmpeg` — the pane simply shows the
file-info placeholder (type/dimensions/size); it never crashes or emits raw escape bytes to a
terminal that can't render them. Detection is cheap and cached. Turn the whole feature off with
`media_preview = false` in [config](configuration.md).

### Running inside herdr

Because the viewer runs as a pane inside herdr (a terminal multiplexer), **herdr** must forward the
graphics to the outer terminal. herdr can, but it is **off by default** — enable it once in
`~/.config/herdr/config.toml`:

```toml
[experimental]
kitty_graphics = true   # render inline kitty graphics from panes to the outer terminal
```

Then apply it: `herdr server reload-config`, and **detach + reattach** (`prefix+q`, then `herdr`)
so the change engages for the client (graphics are initialized per attached client). The outer
terminal must itself be graphics-capable (Ghostty, kitty, WezTerm, …). Without this, the pane shows
the metadata placeholder but no image. Run the viewer standalone (outside herdr) and the image
renders with no extra setup.

### Bundled markdown palette

The viewer ships a small bundled markdown style palette (`assets/markdown-style.json`) that
`glow` is pointed at when it is present, so rendered markdown uses a consistent set of named
ANSI colors (headings, code blocks, links, etc.) rather than glow's built-in `dark` style.
When the palette file is absent, glow falls back to its built-in `dark` style. Markdown still
renders, just with glow's default colors. The palette is a trusted glow argument (located only
inside the plugin's own dirs), never derived from untrusted input.
