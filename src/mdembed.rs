//! Image-embed detection + sentinel plumbing for the rendered (`v`) markdown view.
//!
//! Obsidian embeds an image with `![[image.png|width]]` (a wikilink embed, resolved vault-wide) or
//! the standard `![alt](path.png)` (a relative markdown image). glow knows neither, so the rendered
//! view would otherwise show the literal markup. This module **finds** those image embeds in the
//! (already preprocessed) markdown source — reusing the [`crate::wikilink`] lexer, so fenced code
//! and inline-code spans are skipped — then, for the embeds the render worker resolves to real
//! files, [`inject`]s a unique one-line **sentinel** in place of the markup. After glow renders the
//! source, [`reserve_bands`] locates each sentinel line and turns it into a band of blank rows.
//! The actual image is painted inline over that band by `app::MediaPane` (the same graphics path as
//! the standalone preview). Pure: no I/O — resolving a target to a file and reading its dimensions
//! is the render worker's job.

use crate::media::{self, MediaKind};
use crate::wikilink::{self, LinkKind};
use ratatui::text::{Line, Text};
use std::ops::Range;
use std::path::Path;

/// One image embed found in a markdown note, before resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageEmbed {
    /// The byte span the whole embed markup occupies in the source (including the leading `!`).
    pub span: Range<usize>,
    /// The `target` to resolve to an image file: a vault-wide name/relative-path for a wiki embed
    /// (`![[Pic.png]]`), or a note-relative path for a standard image (`![](sub/pic.png)`).
    pub target: String,
    /// The `|width` hint in pixels (`![[Pic.png|481]]`), when it is a bare number. `None` for no
    /// hint or a non-numeric one (`|left`, `|300x200`) — the image then scales to fit.
    pub width: Option<u32>,
    /// `true` for a `![[…]]` wiki embed (resolve vault-wide), `false` for a `![](…)` markdown image
    /// (resolve relative to the note).
    pub wiki: bool,
}

/// Find every **image** embed in `source`, in source order. Built on [`wikilink::parse_links`], so
/// it inherits the code-fence / inline-code skipping. A `![[…]]` embed counts when its target has a
/// recognized image extension; a `[text](path)` link counts only when it is immediately preceded by
/// `!` (a markdown image) and its target is an image.
pub fn image_embeds(source: &str) -> Vec<ImageEmbed> {
    let bytes = source.as_bytes();
    wikilink::parse_links(source)
        .into_iter()
        .filter_map(|link| {
            if !is_image_target(&link.target) {
                return None;
            }
            match link.kind {
                // `![[Note]]` is an Embed; only image targets are image embeds. `![[Pic.png|481]]`
                // parses the width into the alias (a bare number after `|`).
                LinkKind::Embed => Some(ImageEmbed {
                    span: link.span,
                    target: link.target,
                    width: link
                        .alias
                        .as_deref()
                        .and_then(|a| a.trim().parse::<u32>().ok()),
                    wiki: true,
                }),
                // `[alt](pic.png)` is a markdown link; it is an image only with a leading `!`, which
                // the parser leaves just before the span.
                LinkKind::Markdown => {
                    let has_bang = link.span.start > 0 && bytes[link.span.start - 1] == b'!';
                    has_bang.then(|| ImageEmbed {
                        span: (link.span.start - 1)..link.span.end,
                        target: link.target,
                        width: None,
                        wiki: false,
                    })
                }
                LinkKind::Wiki => None,
            }
        })
        .collect()
}

/// Whether a link/embed target names an image file (by extension).
fn is_image_target(target: &str) -> bool {
    // Strip any anchor the parser may have left on an odd target, then classify by extension.
    let path = target.split('#').next().unwrap_or(target).trim();
    !path.is_empty() && media::classify(Path::new(path)) == Some(MediaKind::Image)
}

/// The sentinel marker. Deliberately plain ASCII on its own paragraph, so glow renders it verbatim
/// as a single line we can find in the output, and unlikely enough to never collide with real note
/// text. The id is the embed's index; `reserve_bands` parses it back out.
const MARK: &str = "HFVxEMBEDx";

/// The sentinel token for embed `id`, wrapped in blank lines so glow renders it as its own
/// one-line paragraph (never merged into an adjacent paragraph).
fn sentinel(id: usize) -> String {
    format!("\n\n{MARK}{id}x{MARK}\n\n")
}

/// Parse the embed id out of a rendered line's text, if it carries a sentinel. Matches
/// `{MARK}{digits}x{MARK}` anywhere in the line (glow may pad it with spaces).
fn sentinel_id(line_text: &str) -> Option<usize> {
    let start = line_text.find(MARK)? + MARK.len();
    let rest = &line_text[start..];
    let end = rest.find(MARK)?;
    rest[..end].trim_end_matches('x').parse::<usize>().ok()
}

/// Replace each `(span, id)` in `source` with that embed's sentinel, leaving every other byte
/// untouched. `replacements` must be in ascending, non-overlapping span order (as
/// [`image_embeds`] returns them, filtered to the resolved ones). Only the resolved embeds are
/// replaced; an unresolved embed keeps its literal markup so the reader sees it.
pub fn inject(source: &str, replacements: &[(Range<usize>, usize)]) -> String {
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0;
    for (span, id) in replacements {
        if span.start < cursor || span.end > source.len() {
            continue; // defensive: skip an out-of-order / out-of-range span rather than panic
        }
        out.push_str(&source[cursor..span.start]);
        out.push_str(&sentinel(*id));
        cursor = span.end;
    }
    out.push_str(&source[cursor..]);
    out
}

/// Turn each sentinel line in glow's rendered output into a band of `rows` blank lines (looked up
/// by embed id via `rows_for`). Returns the rebuilt text plus, for every band found, its embed id
/// and the 0-based line index of the band's top row — what the app overlays the image onto. A
/// sentinel whose id has no `rows_for` entry is dropped to a single blank line (its image did not
/// resolve to a size).
pub fn reserve_bands(
    text: Text<'static>,
    rows_for: impl Fn(usize) -> Option<u16>,
) -> (Text<'static>, Vec<(usize, usize)>) {
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(text.lines.len());
    let mut bands: Vec<(usize, usize)> = Vec::new();
    for line in text.lines {
        let plain: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        if let Some(id) = sentinel_id(&plain) {
            let top = lines.len();
            let rows = rows_for(id).unwrap_or(1).max(1);
            for _ in 0..rows {
                lines.push(Line::default());
            }
            if rows_for(id).is_some() {
                bands.push((id, top));
            }
        } else {
            lines.push(line);
        }
    }
    (Text::from(lines), bands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_image_embed_with_width() {
        let e = image_embeds("intro\n![[Schwiz_Character_Sheet.png|481]]\nmore");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].target, "Schwiz_Character_Sheet.png");
        assert_eq!(e[0].width, Some(481));
        assert!(e[0].wiki);
        assert_eq!(
            &"intro\n![[Schwiz_Character_Sheet.png|481]]\nmore"[e[0].span.clone()],
            "![[Schwiz_Character_Sheet.png|481]]"
        );
    }

    #[test]
    fn wiki_image_embed_without_width() {
        let e = image_embeds("![[diagram.jpg]]");
        assert_eq!(e[0].target, "diagram.jpg");
        assert_eq!(e[0].width, None);
        assert!(e[0].wiki);
    }

    #[test]
    fn standard_markdown_image_span_includes_bang() {
        let src = "![a caption](assets/pic.png)";
        let e = image_embeds(src);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].target, "assets/pic.png");
        assert!(!e[0].wiki);
        assert_eq!(&src[e[0].span.clone()], "![a caption](assets/pic.png)");
    }

    #[test]
    fn non_bang_markdown_link_is_not_an_image() {
        assert!(image_embeds("[see the pic](pic.png)").is_empty());
    }

    #[test]
    fn non_image_embeds_are_ignored() {
        assert!(image_embeds("![[Some Note]] and [[Other]]").is_empty());
        assert!(image_embeds("![[report.pdf]]").is_empty());
    }

    #[test]
    fn embeds_in_code_are_skipped() {
        let src = "real ![[a.png]]\n```\n![[b.png]] in code\n```\ninline `![[c.png]]` too";
        let targets: Vec<_> = image_embeds(src).into_iter().map(|e| e.target).collect();
        assert_eq!(targets, vec!["a.png"]);
    }

    #[test]
    fn multiple_embeds_in_order() {
        let e = image_embeds("![[one.png]]\ntext\n![two](two.jpg)\n![[three.webp|200]]");
        let targets: Vec<_> = e.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(targets, vec!["one.png", "two.jpg", "three.webp"]);
        assert_eq!(e[2].width, Some(200));
    }

    #[test]
    fn inject_replaces_only_resolved_spans() {
        let src = "a ![[keep.png]] b ![[drop.png]] c";
        let embeds = image_embeds(src);
        // Resolve only the first embed.
        let out = inject(src, &[(embeds[0].span.clone(), 0)]);
        assert!(out.contains("HFVxEMBEDx0x"), "sentinel injected: {out}");
        assert!(
            out.contains("![[drop.png]]"),
            "unresolved markup kept: {out}"
        );
        assert!(
            !out.contains("![[keep.png]]"),
            "resolved markup removed: {out}"
        );
    }

    #[test]
    fn reserve_bands_finds_sentinels_and_reserves_rows() {
        // Simulate glow output: a heading line, a sentinel line, a trailing line.
        let text = Text::from(vec![
            Line::from("# Title"),
            Line::from("HFVxEMBEDx0xHFVxEMBEDx"),
            Line::from("after"),
        ]);
        let (out, bands) = reserve_bands(text, |id| (id == 0).then_some(3));
        assert_eq!(bands, vec![(0, 1)], "band 0 tops at line index 1");
        // 1 heading + 3 reserved + 1 trailing = 5 lines; the reserved rows are blank.
        assert_eq!(out.lines.len(), 5);
        let plain =
            |l: &Line| -> String { l.spans.iter().map(|s| s.content.to_string()).collect() };
        assert_eq!(plain(&out.lines[0]), "# Title");
        assert_eq!(plain(&out.lines[1]), "");
        assert_eq!(plain(&out.lines[4]), "after");
    }

    #[test]
    fn sentinel_id_round_trips() {
        let s = sentinel(7);
        // The middle line carries the marker (glow would render it padded; contains still matches).
        assert_eq!(sentinel_id(&format!("   {}   ", s.trim())), Some(7));
        assert_eq!(sentinel_id("no marker here"), None);
    }
}
