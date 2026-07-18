//! Post-render styling pass for the rendered (`v`) markdown view — the "Obsidian glowup".
//!
//! The content pane delegates markdown styling to `glow` (constitution: delegate rendering), but
//! glow knows nothing about Obsidian's link/callout conventions:
//!
//! - It renders `[[wikilink]]` / `[[wikilink|alias]]` / `![[embed]]` markup as **plain
//!   body-coloured text** (it is not standard markdown), so links do not stand out.
//! - Its callouts are plain bordered blockquotes — no per-type accent, no filled tint.
//!
//! Rather than reinvent a renderer, this module runs a **pure post-render pass** over the ratatui
//! [`Text`] glow produced (already ingested via `ansi-to-tui`), patching styles onto the spans:
//!
//! - **Links** ([`style_links`]): wikilink/embed markup runs get a distinct link colour + underline.
//!   Standard `[text](target)` links are handled by recolouring the URL span glow already
//!   underlines (underline is glow's only link-URL signal in its dark theme) to the same colour.
//! - **Callouts** ([`tint_callout`]): a glow blockquote whose first line carries our icon + LABEL
//!   header (see [`crate::mdnote`]) becomes a titled box — a faint per-type background tint across
//!   every line of the block, an accent-coloured left bar, and an accent-coloured bold title.
//!
//! The wikilink markup to restyle comes from [`crate::wikilink::parse_links`] on the **pre-glow**
//! source, so markup inside fenced/inline code is not collected (glow renders it verbatim too, but
//! we never search for it). The one imperfection: if the *identical* wikilink markup appears both as
//! a real link and inside a code block, both are restyled — see `docs/usage.md`.
//!
//! Pure: allocates a new `Text`; never mutates the input. Zero new Cargo deps (ratatui + std).

use std::borrow::Cow;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

use crate::wikilink::{self, LinkKind};

// ── link styling ────────────────────────────────────────────────────────────

/// Obsidian-like link colour: a soft purple, readable on a dark theme.
const LINK_FG: Color = Color::Rgb(183, 148, 244);

/// The style patched onto a link run: the link colour plus an underline cue.
const LINK_STYLE: Style = Style::new().fg(LINK_FG).add_modifier(Modifier::UNDERLINED);

// ── callout accents ─────────────────────────────────────────────────────────

/// The callout header icons [`crate::mdnote::transform_callouts`] emits. A rendered blockquote line
/// is treated as a callout header only when (after the `│` border) it starts with one of these,
/// followed by a space and an ALL-CAPS label — a strong signal that avoids restyling ordinary
/// blockquote prose. Kept in lockstep with `mdnote`'s icons by
/// `known_callout_types_are_detected` below.
const CALLOUT_ICONS: &[char] = &['▸', '▤', 'ℹ', '☐', '★', '✓', '?', '⚠', '✗', '‼', '✷', '❝'];

/// The glow blockquote border glyph. Every line of a blockquote (callout or plain) begins with it.
const BORDER: char = '│';

/// A per-type callout accent. Maps to a bright foreground (title / left bar) and a faint background
/// tint (the filled box). Chosen to read on a dark theme.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Accent {
    Blue,
    Cyan,
    Green,
    Yellow,
    Red,
    Purple,
    Gray,
}

impl Accent {
    /// The bright accent foreground for the title, icon, and left bar.
    fn fg(self) -> Color {
        match self {
            Accent::Blue => Color::Rgb(120, 170, 255),
            Accent::Cyan => Color::Rgb(80, 200, 210),
            Accent::Green => Color::Rgb(120, 200, 130),
            Accent::Yellow => Color::Rgb(230, 200, 100),
            Accent::Red => Color::Rgb(240, 120, 120),
            Accent::Purple => Color::Rgb(190, 150, 240),
            Accent::Gray => Color::Rgb(170, 170, 180),
        }
    }

    /// The faint background tint filling the callout box (a dark, low-saturation shade).
    fn bg(self) -> Color {
        match self {
            Accent::Blue => Color::Rgb(30, 40, 58),
            Accent::Cyan => Color::Rgb(26, 46, 50),
            Accent::Green => Color::Rgb(28, 46, 34),
            Accent::Yellow => Color::Rgb(52, 46, 26),
            Accent::Red => Color::Rgb(54, 32, 34),
            Accent::Purple => Color::Rgb(44, 34, 56),
            Accent::Gray => Color::Rgb(42, 42, 48),
        }
    }
}

/// The accent for a rendered callout LABEL (the uppercase word `mdnote` emits after the icon).
/// Unknown labels (custom callout types, which `mdnote` renders with the `▸` note icon) fall back
/// to the note accent, matching `mdnote`'s own fallback.
fn accent_for_label(label: &str) -> Accent {
    match label {
        "NOTE" | "INFO" | "TODO" => Accent::Blue,
        "SUMMARY" | "TIP" => Accent::Cyan,
        "SUCCESS" => Accent::Green,
        "QUESTION" | "WARNING" => Accent::Yellow,
        "FAILURE" | "DANGER" | "BUG" => Accent::Red,
        "EXAMPLE" => Accent::Purple,
        "QUOTE" => Accent::Gray,
        _ => Accent::Blue,
    }
}

/// How a rendered line relates to a callout block.
enum CalloutLine {
    /// A callout header (`│ <icon> <LABEL> …`) opening a block with this accent.
    Header(Accent),
    /// A blockquote continuation line (`│ …`) — a callout body line when a block is open, or an
    /// ordinary blockquote line otherwise.
    Body,
    /// Any other line — ends an open callout block.
    Other,
}

// ── public API ──────────────────────────────────────────────────────────────

/// Run the Obsidian styling pass over glow's rendered markdown `text`, using `markdown_source`
/// (the pre-glow markdown fed to glow) to know which wikilinks are real (not inside code).
///
/// Returns a new `Text`; the input is consumed and never mutated. Callout tinting is a small state
/// machine over the lines (a header opens a block; blockquote continuation lines stay in it; any
/// other line closes it), so a plain blockquote — one with no icon+LABEL header — is never tinted.
pub fn style_rendered_markdown(text: Text<'static>, markdown_source: &str) -> Text<'static> {
    let markups = wiki_markups(markdown_source);
    let mut out: Vec<Line<'static>> = Vec::with_capacity(text.lines.len());
    let mut open: Option<Accent> = None; // the accent of the callout currently open, if any

    for line in text.lines {
        let plain = plain_text(&line);
        let line = match classify_callout_line(&plain) {
            CalloutLine::Header(accent) => {
                open = Some(accent);
                tint_callout(line, accent, true)
            }
            CalloutLine::Body => match open {
                Some(accent) => tint_callout(line, accent, false),
                None => line, // a plain blockquote line — leave it be
            },
            CalloutLine::Other => {
                open = None;
                line
            }
        };
        out.push(style_links(line, &markups));
    }
    Text::from(out)
}

// ── callout detection + tinting ───────────────────────────────────────────────

/// The concatenated plain text of a line's spans.
fn plain_text(line: &Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Classify a rendered line by its plain text: is it a callout header, a blockquote continuation,
/// or something else? A header is a blockquote line whose content (after the `│` border) starts
/// with one of our [`CALLOUT_ICONS`], a space, and an ALL-CAPS label token.
fn classify_callout_line(plain: &str) -> CalloutLine {
    let content = plain.trim_start();
    let Some(after_border) = content.strip_prefix(BORDER) else {
        return CalloutLine::Other; // not a blockquote line at all
    };
    let inner = after_border.trim_start();
    let mut chars = inner.chars();
    let Some(icon) = chars.next() else {
        return CalloutLine::Body;
    };
    if !CALLOUT_ICONS.contains(&icon) {
        return CalloutLine::Body;
    }
    // The icon must be followed by a space, then an uppercase label token.
    let Some(rest) = chars.as_str().strip_prefix(' ') else {
        return CalloutLine::Body;
    };
    let token: String = rest
        .chars()
        .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    if token.len() >= 2 && token.chars().any(|c| c.is_ascii_uppercase()) {
        CalloutLine::Header(accent_for_label(&token))
    } else {
        CalloutLine::Body
    }
}

/// Patch a callout's per-type styling onto one rendered line: a faint background tint across every
/// span (a full-width filled box, since glow pads each line to the pane width), an accent-coloured
/// left bar on the `│` border, and — for the header line — an accent-coloured bold title.
fn tint_callout(line: Line<'static>, accent: Accent, is_header: bool) -> Line<'static> {
    let bg = accent.bg();
    let fg = accent.fg();
    let spans = line
        .spans
        .into_iter()
        .map(|s| {
            let blank = s.content.trim().is_empty();
            let is_border = s.content.contains(BORDER);
            let mut style = s.style.bg(bg);
            if is_header && !blank {
                // The whole header line (icon, label, title, border) takes the accent, bold.
                style = style.fg(fg).add_modifier(Modifier::BOLD);
            } else if is_border {
                // Body lines: only the left bar is accented; body text keeps glow's styling.
                style = style.fg(fg);
            }
            Span {
                content: s.content,
                style,
            }
        })
        .collect();
    Line {
        spans,
        style: line.style.bg(bg),
        alignment: line.alignment,
    }
}

// ── link detection + restyling ────────────────────────────────────────────────

/// The literal wikilink/embed markup strings to restyle, taken from the pre-glow source so code
/// spans are skipped. Standard `[text](target)` links are NOT collected here — glow rewrites them,
/// so they are restyled via their underlined URL span in [`style_links`]. Deduplicated for fewer
/// per-line searches.
fn wiki_markups(source: &str) -> Vec<String> {
    let mut markups: Vec<String> = wikilink::parse_links(source)
        .into_iter()
        .filter_map(|link| match link.kind {
            LinkKind::Wiki | LinkKind::Embed => source.get(link.span).map(str::to_owned),
            LinkKind::Markdown => None,
        })
        .collect();
    markups.sort_unstable();
    markups.dedup();
    markups
}

/// Restyle links on one line: (1) recolour glow's underlined markdown-link URL spans to the link
/// colour, then (2) patch the link style onto every wikilink/embed markup run found in the line's
/// plain text.
fn style_links(line: Line<'static>, markups: &[String]) -> Line<'static> {
    // (1) glow underlines only link URLs in its dark theme — recolour them to the unified link fg.
    let spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|s| {
            if s.style.add_modifier.contains(Modifier::UNDERLINED) {
                let style = s.style.fg(LINK_FG);
                Span {
                    content: s.content,
                    style,
                }
            } else {
                s
            }
        })
        .collect();

    // (2) restyle the wikilink/embed markup runs glow passed through verbatim.
    let plain: String = spans.iter().map(|s| s.content.as_ref()).collect();
    let ranges: Vec<(usize, usize)> = if plain.contains("[[") {
        markups.iter().flat_map(|m| find_all(&plain, m)).collect()
    } else {
        Vec::new()
    };

    let spans = if ranges.is_empty() {
        spans
    } else {
        restyle_ranges(&spans, &ranges, LINK_STYLE)
    };

    Line {
        spans,
        style: line.style,
        alignment: line.alignment,
    }
}

/// Byte ranges of every occurrence of `needle` in `haystack` (non-overlapping, left to right).
fn find_all(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let s = start + pos;
        let e = s + needle.len();
        out.push((s, e));
        start = e;
    }
    out
}

/// Re-segment `spans` at the `ranges` boundaries and patch `style` onto the sub-spans that fall
/// inside any range. Mirrors [`crate::highlight`]'s resegmentation: `ranges` are byte offsets into
/// the line's concatenated plain text and align with the spans' byte layout.
fn restyle_ranges(
    spans: &[Span<'static>],
    ranges: &[(usize, usize)],
    style: Style,
) -> Vec<Span<'static>> {
    let mut boundaries: Vec<usize> = ranges.iter().flat_map(|&(s, e)| [s, e]).collect();
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut out: Vec<Span<'static>> = Vec::new();
    let mut cursor = 0usize; // byte offset into the plain text
    for span in spans {
        let text = span.content.as_ref();
        let lo = cursor;
        let hi = cursor + text.len();
        let cuts: Vec<usize> = boundaries
            .iter()
            .copied()
            .filter(|&b| b > lo && b < hi)
            .collect();
        if cuts.is_empty() {
            out.push(Span {
                content: span.content.clone(),
                style: patch_if_covered(lo, hi, span.style, ranges, style),
            });
        } else {
            let mut pos = lo;
            for &cut in &cuts {
                if cut > pos {
                    let sub = text[(pos - lo)..(cut - lo)].to_owned();
                    out.push(Span {
                        content: Cow::Owned(sub),
                        style: patch_if_covered(pos, cut, span.style, ranges, style),
                    });
                }
                pos = cut;
            }
            if pos < hi {
                let sub = text[(pos - lo)..].to_owned();
                out.push(Span {
                    content: Cow::Owned(sub),
                    style: patch_if_covered(pos, hi, span.style, ranges, style),
                });
            }
        }
        cursor = hi;
    }
    out
}

/// Patch `style` onto `original` iff the sub-segment `[lo, hi)` is fully inside some range. A
/// sub-segment produced by [`restyle_ranges`] never straddles a boundary, so it is wholly inside or
/// wholly outside every range.
fn patch_if_covered(
    lo: usize,
    hi: usize,
    original: Style,
    ranges: &[(usize, usize)],
    style: Style,
) -> Style {
    if ranges.iter().any(|&(s, e)| lo >= s && hi <= e) {
        original.patch(style)
    } else {
        original
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a line from `(text, style)` span pairs.
    fn line(spans: &[(&str, Style)]) -> Line<'static> {
        Line::from(
            spans
                .iter()
                .map(|(t, st)| Span {
                    content: Cow::Owned((*t).to_owned()),
                    style: *st,
                })
                .collect::<Vec<_>>(),
        )
    }

    /// The concatenated (text, style) of a line's spans, for assertions.
    fn spans_of(l: &Line<'static>) -> Vec<(String, Style)> {
        l.spans
            .iter()
            .map(|s| (s.content.to_string(), s.style))
            .collect()
    }

    // ── link styling ──────────────────────────────────────────────────────────

    #[test]
    fn wikilink_markup_gets_link_style_across_split_spans() {
        // glow splits `[[` into separate spans; the run must still be restyled as one.
        let l = line(&[
            ("see ", Style::default()),
            ("[", Style::default()),
            ("[", Style::default()),
            ("Some Note]] end", Style::default()),
        ]);
        let styled = style_links(l, &["[[Some Note]]".to_string()]);
        // Every character of `[[Some Note]]` must carry the link fg + underline.
        let mut covered = String::new();
        for (text, st) in spans_of(&styled) {
            if st.fg == Some(LINK_FG) {
                assert!(st.add_modifier.contains(Modifier::UNDERLINED));
                covered.push_str(&text);
            }
        }
        assert_eq!(covered, "[[Some Note]]");
    }

    #[test]
    fn embed_and_alias_markup_are_styled() {
        let l = line(&[("x ![[Note]] y [[T|a]] z", Style::default())]);
        let styled = style_links(l, &["![[Note]]".to_string(), "[[T|a]]".to_string()]);
        let linked: String = spans_of(&styled)
            .into_iter()
            .filter(|(_, st)| st.fg == Some(LINK_FG))
            .map(|(t, _)| t)
            .collect();
        assert_eq!(linked, "![[Note]][[T|a]]");
    }

    #[test]
    fn markdown_link_url_underline_is_recoloured() {
        // glow underlines only the link URL; recolour it to the link fg, keeping the underline.
        let url = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::UNDERLINED);
        let l = line(&[
            ("text ", Style::default().fg(Color::Green)),
            ("/Other.md", url),
        ]);
        let styled = style_links(l, &[]);
        let spans = spans_of(&styled);
        assert_eq!(spans[0].1.fg, Some(Color::Green), "link text left alone");
        assert_eq!(spans[1].1.fg, Some(LINK_FG), "URL recoloured to link fg");
        assert!(spans[1].1.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn unknown_markup_and_plain_text_untouched() {
        let l = line(&[("just plain body text", Style::default())]);
        let styled = style_links(l, &["[[NotHere]]".to_string()]);
        assert!(spans_of(&styled).iter().all(|(_, st)| st.fg.is_none()));
    }

    #[test]
    fn find_all_finds_repeated_occurrences() {
        assert_eq!(find_all("a[[x]]b[[x]]", "[[x]]"), vec![(1, 6), (7, 12)]);
        assert_eq!(find_all("nope", "[[x]]"), Vec::<(usize, usize)>::new());
    }

    #[test]
    fn wiki_markups_skips_code_and_markdown_links() {
        // `[[Real]]` counts; the code-spanned one and the `[md](x)` link do not.
        let src = "[[Real]] and `[[Code]]` and [md](x.md) and ![[Emb]]";
        let mut got = wiki_markups(src);
        got.sort();
        assert_eq!(got, vec!["![[Emb]]".to_string(), "[[Real]]".to_string()]);
    }

    // ── callouts ────────────────────────────────────────────────────────────

    /// A glow-rendered blockquote line: 2-space margin, `│ ` border, then content, then padding.
    fn glow_blockquote(content: &str) -> Line<'static> {
        line(&[
            ("  ", Style::default()),
            ("│ ", Style::default().fg(Color::Rgb(55, 55, 55))),
            (content, Style::default()),
            ("    ", Style::default()),
        ])
    }

    #[test]
    fn callout_header_is_detected_and_accented() {
        match classify_callout_line("  │ ⚠ WARNING — Careful") {
            CalloutLine::Header(a) => assert_eq!(a, Accent::Yellow),
            _ => panic!("warning header not detected"),
        }
        match classify_callout_line("  │ ▸ NOTE") {
            CalloutLine::Header(a) => assert_eq!(a, Accent::Blue),
            _ => panic!("note header not detected"),
        }
    }

    #[test]
    fn plain_blockquote_and_prose_are_not_headers() {
        assert!(matches!(
            classify_callout_line("  │ Just a quote."),
            CalloutLine::Body
        ));
        // A question-mark icon must not turn ordinary prose into a callout header.
        assert!(matches!(
            classify_callout_line("  │ ? what now"),
            CalloutLine::Body
        ));
        assert!(matches!(
            classify_callout_line("  Regular paragraph."),
            CalloutLine::Other
        ));
    }

    #[test]
    fn callout_block_is_tinted_and_plain_blockquote_is_not() {
        let text = Text::from(vec![
            glow_blockquote("▸ NOTE — Title"),
            glow_blockquote("body line one"),
            line(&[("", Style::default())]), // blank line ends the block
            glow_blockquote("a plain quote"),
        ]);
        let out = style_rendered_markdown(text, "");
        let bg = Accent::Blue.bg();

        // Header + body line carry the tint on every span.
        for i in [0usize, 1] {
            assert!(
                out.lines[i].spans.iter().all(|s| s.style.bg == Some(bg)),
                "callout line {i} fully tinted"
            );
        }
        // Header title is accent fg + bold.
        let header_title = out.lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains("NOTE"))
            .unwrap();
        assert_eq!(header_title.style.fg, Some(Accent::Blue.fg()));
        assert!(header_title.style.add_modifier.contains(Modifier::BOLD));

        // The trailing plain blockquote (index 3) is NOT tinted.
        assert!(
            out.lines[3].spans.iter().all(|s| s.style.bg != Some(bg)),
            "plain blockquote untinted"
        );
    }

    #[test]
    fn body_line_left_bar_is_accented_but_body_text_is_not() {
        let text = Text::from(vec![
            glow_blockquote("★ TIP"),
            glow_blockquote("stay hydrated"),
        ]);
        let out = style_rendered_markdown(text, "");
        let body = &out.lines[1];
        let border = body
            .spans
            .iter()
            .find(|s| s.content.contains(BORDER))
            .unwrap();
        assert_eq!(
            border.style.fg,
            Some(Accent::Cyan.fg()),
            "tip left bar accented"
        );
        let body_text = body
            .spans
            .iter()
            .find(|s| s.content.contains("hydrated"))
            .unwrap();
        assert_eq!(body_text.style.fg, None, "body text keeps its own colour");
    }

    /// Drift guard: every Obsidian callout type `mdnote` renders must be detected here (icon in
    /// [`CALLOUT_ICONS`], label mapped to an accent). Ties this module's tables to `mdnote`'s
    /// `callout_style` via the public transform.
    #[test]
    fn known_callout_types_are_detected() {
        let kinds = [
            "note",
            "abstract",
            "summary",
            "tldr",
            "info",
            "todo",
            "tip",
            "hint",
            "important",
            "success",
            "check",
            "done",
            "question",
            "help",
            "faq",
            "warning",
            "caution",
            "attention",
            "failure",
            "fail",
            "missing",
            "danger",
            "error",
            "bug",
            "example",
            "quote",
            "cite",
        ];
        for kind in kinds {
            let rendered = crate::mdnote::transform_callouts(&format!("> [!{kind}] Title\n"));
            let header = rendered.lines().next().unwrap();
            // `> **▸ NOTE — Title**` → `▸ NOTE — Title`
            let inner = header
                .trim_start()
                .trim_start_matches("> **")
                .trim_end_matches("**");
            let glow_like = format!("  │ {inner}   ");
            assert!(
                matches!(classify_callout_line(&glow_like), CalloutLine::Header(_)),
                "callout kind `{kind}` not detected from `{glow_like}`"
            );
        }
    }

    #[test]
    fn custom_callout_type_falls_back_to_note_accent() {
        match classify_callout_line("  │ ▸ CUSTOM — Title") {
            CalloutLine::Header(a) => assert_eq!(a, Accent::Blue),
            _ => panic!("custom callout not detected"),
        }
    }
}
