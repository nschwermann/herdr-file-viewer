//! Wikilink parsing — pull Obsidian links out of a markdown **source** string.
//!
//! A pure lexer (no I/O): given the raw markdown text it returns every followable link, in
//! source order, with the byte span it occupies so a caller can rank/annotate them. It parses
//! the Obsidian variants — `[[Note]]`, `[[Note|alias]]`, `[[Note#Heading]]`, `[[Note#^blockid]]`,
//! the embed forms `![[…]]` — plus standard inline `[text](relative.md)` links. Fenced code
//! blocks (``` / ~~~) and inline code spans (`` `…` ``) are skipped so a link written inside a
//! code sample is never treated as followable. Resolution of a [`Link::target`] to an actual note
//! is [`crate::obsidian::resolve_target`]'s job; this module only finds and structures the links.

use std::ops::Range;

/// Which markup produced a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// `[[Note]]` / `[[Note|alias]]` / `[[Note#Heading]]`.
    Wiki,
    /// `![[Note]]` — an embed/transclusion. Followed like a wikilink (we open the target rather
    /// than inlining it, which the read-only viewer does not do).
    Embed,
    /// A standard inline `[text](target)` markdown link with a relative (non-URL) target.
    Markdown,
}

/// A link's optional in-note anchor: a heading or a block id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anchor {
    /// `#Heading` — scroll to that heading.
    Heading(String),
    /// `#^blockid` — scroll to the block carrying that `^id` marker.
    Block(String),
}

/// One followable link found in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The byte range the whole link markup occupies in the source (for annotation/ranking).
    pub span: Range<usize>,
    /// Which markup produced it.
    pub kind: LinkKind,
    /// The path/name part to resolve (anchor and alias removed). May be empty for a pure
    /// same-note anchor link like `[[#Heading]]`.
    pub target: String,
    /// The in-note anchor (`#Heading` / `#^block`), if any.
    pub anchor: Option<Anchor>,
    /// The display alias (`[[Note|alias]]` or the `[text]` of a markdown link), if any.
    pub alias: Option<String>,
}

impl Link {
    /// A short human label for the link navigator / notices: the alias if present, else the
    /// target (with its anchor appended), else the bare anchor for a same-note link.
    pub fn display(&self) -> String {
        if let Some(alias) = &self.alias {
            return alias.clone();
        }
        let mut s = self.target.clone();
        match &self.anchor {
            Some(Anchor::Heading(h)) => {
                if s.is_empty() {
                    s = format!("#{h}");
                } else {
                    s.push('#');
                    s.push_str(h);
                }
            }
            Some(Anchor::Block(b)) => {
                if s.is_empty() {
                    s = format!("#^{b}");
                } else {
                    s.push_str("#^");
                    s.push_str(b);
                }
            }
            None => {}
        }
        s
    }
}

/// Split a wikilink's inner text (`Note#Heading|alias`) into `(target, anchor, alias)`.
/// The alias is everything after the first `|`; the anchor is everything after the first `#`
/// in the pre-alias part (`^id` → block, else heading). Whitespace is trimmed.
fn split_wiki_inner(inner: &str) -> (String, Option<Anchor>, Option<String>) {
    let (path_part, alias) = match inner.split_once('|') {
        Some((p, a)) => (p, Some(a.trim().to_string())),
        None => (inner, None),
    };
    let (target, anchor) = split_anchor(path_part);
    (target, anchor, alias.filter(|a| !a.is_empty()))
}

/// Split a `target#anchor` string into `(target, anchor)`. A `#^id` anchor is a block id; any
/// other `#…` is a heading. No `#` yields `(target, None)`.
fn split_anchor(s: &str) -> (String, Option<Anchor>) {
    match s.split_once('#') {
        Some((target, anchor)) => {
            let anchor = anchor.trim();
            let anchor = if let Some(block) = anchor.strip_prefix('^') {
                Some(Anchor::Block(block.trim().to_string()))
            } else if anchor.is_empty() {
                None
            } else {
                Some(Anchor::Heading(anchor.to_string()))
            };
            (target.trim().to_string(), anchor)
        }
        None => (s.trim().to_string(), None),
    }
}

/// Whether a markdown-link target is an external URL / non-followable scheme (so we leave it to
/// the OS, not inline navigation). Matches `scheme://`, `mailto:`, `tel:`, and protocol-relative
/// `//host`.
fn is_external_target(t: &str) -> bool {
    let t = t.trim();
    t.starts_with("//") || t.starts_with("mailto:") || t.starts_with("tel:") || t.contains("://")
}

/// Parse every followable link out of `source`, in source order. Pure; skips fenced code blocks
/// and inline code spans so links written inside code are ignored.
pub fn parse_links(source: &str) -> Vec<Link> {
    let bytes = source.as_bytes();
    let mut links = Vec::new();
    let mut i = 0;
    let mut at_line_start = true; // for fence detection
    let mut in_fence: Option<&[u8]> = None; // the fence marker (``` or ~~~) currently open

    while i < bytes.len() {
        // Fenced code block handling: a line beginning (after optional spaces) with ``` or ~~~.
        if at_line_start {
            let line_start = i;
            let mut j = i;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            let fence = if bytes[j..].starts_with(b"```") {
                Some(&b"```"[..])
            } else if bytes[j..].starts_with(b"~~~") {
                Some(&b"~~~"[..])
            } else {
                None
            };
            if let Some(marker) = fence {
                match in_fence {
                    // An open fence closes on a matching marker line.
                    Some(open) if open == marker => in_fence = None,
                    None => in_fence = Some(marker),
                    _ => {}
                }
                // Skip the rest of this line.
                i = line_start;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue; // stays at_line_start after we consume the '\n' below
            }
        }

        let b = bytes[i];
        if b == b'\n' {
            at_line_start = true;
            i += 1;
            continue;
        }
        at_line_start = false;

        if in_fence.is_some() {
            i += 1;
            continue;
        }

        // Inline code span: skip from an opening run of backticks to the matching run.
        if b == b'`' {
            let mut ticks = 0;
            while i + ticks < bytes.len() && bytes[i + ticks] == b'`' {
                ticks += 1;
            }
            let mut k = i + ticks;
            while k < bytes.len() {
                if bytes[k] == b'`' {
                    let mut run = 0;
                    while k + run < bytes.len() && bytes[k + run] == b'`' {
                        run += 1;
                    }
                    if run == ticks {
                        k += run;
                        break;
                    }
                    k += run;
                } else if bytes[k] == b'\n' {
                    // An unterminated inline span does not cross a blank line; bail conservatively.
                    break;
                } else {
                    k += 1;
                }
            }
            i = k.max(i + ticks);
            continue;
        }

        // Embed / wikilink: `![[ … ]]` or `[[ … ]]`.
        let is_embed = b == b'!' && bytes[i + 1..].starts_with(b"[[");
        if is_embed || bytes[i..].starts_with(b"[[") {
            let open = i;
            let inner_start = if is_embed { i + 3 } else { i + 2 };
            if let Some(rel_close) = find_sub(&bytes[inner_start..], b"]]") {
                let inner_end = inner_start + rel_close;
                let inner = &source[inner_start..inner_end];
                // A wikilink's inner text never spans a newline.
                if !inner.contains('\n') {
                    let (target, anchor, alias) = split_wiki_inner(inner);
                    links.push(Link {
                        span: open..inner_end + 2,
                        kind: if is_embed {
                            LinkKind::Embed
                        } else {
                            LinkKind::Wiki
                        },
                        target,
                        anchor,
                        alias,
                    });
                    i = inner_end + 2;
                    continue;
                }
            }
            i += 1;
            continue;
        }

        // Standard markdown link: `[text](target)`.
        if b == b'['
            && let Some((link, next)) = parse_markdown_link(source, bytes, i)
        {
            if let Some(link) = link {
                links.push(link);
            }
            i = next;
            continue;
        }

        i += 1;
    }
    links
}

/// Parse a `[text](target)` starting at `open` (`bytes[open] == b'['`). Returns
/// `Some((maybe_link, next_index))` when the `[...](...)` shape is present (the link is `None`
/// when its target is external and thus skipped), or `None` when the shape does not match so the
/// caller advances by one byte.
fn parse_markdown_link(source: &str, bytes: &[u8], open: usize) -> Option<(Option<Link>, usize)> {
    // `[text]` — text ends at the matching `]` on the same line (no nested brackets handled).
    let text_start = open + 1;
    let mut j = text_start;
    while j < bytes.len() && bytes[j] != b']' && bytes[j] != b'\n' {
        j += 1;
    }
    if j >= bytes.len() || bytes[j] != b']' {
        return None;
    }
    let text = &source[text_start..j];
    // Immediately followed by `(`.
    if bytes.get(j + 1) != Some(&b'(') {
        return None;
    }
    let target_start = j + 2;
    let mut k = target_start;
    while k < bytes.len() && bytes[k] != b')' && bytes[k] != b'\n' {
        k += 1;
    }
    if k >= bytes.len() || bytes[k] != b')' {
        return None;
    }
    let mut target = source[target_start..k].trim();
    // Allow `<url with spaces>`.
    if target.starts_with('<') && target.ends_with('>') && target.len() >= 2 {
        target = &target[1..target.len() - 1];
    }
    let next = k + 1;
    if target.is_empty() || is_external_target(target) {
        return Some((None, next));
    }
    let (path, anchor) = split_anchor(target);
    let alias = (!text.trim().is_empty()).then(|| text.trim().to_string());
    Some((
        Some(Link {
            span: open..next,
            kind: LinkKind::Markdown,
            target: path,
            anchor,
            alias,
        }),
        next,
    ))
}

/// Byte index of the first occurrence of `needle` in `hay`, or `None`.
fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets(src: &str) -> Vec<String> {
        parse_links(src).into_iter().map(|l| l.target).collect()
    }

    #[test]
    fn plain_wikilink() {
        let links = parse_links("see [[Some Note]] here");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].kind, LinkKind::Wiki);
        assert_eq!(links[0].target, "Some Note");
        assert_eq!(links[0].anchor, None);
        assert_eq!(links[0].alias, None);
        // The span covers exactly the `[[…]]`.
        assert_eq!(
            &"see [[Some Note]] here"[links[0].span.clone()],
            "[[Some Note]]"
        );
    }

    #[test]
    fn wikilink_with_alias() {
        let links = parse_links("[[Target Note|the display text]]");
        assert_eq!(links[0].target, "Target Note");
        assert_eq!(links[0].alias.as_deref(), Some("the display text"));
        assert_eq!(links[0].display(), "the display text");
    }

    #[test]
    fn wikilink_with_heading_anchor() {
        let links = parse_links("[[Note#Some Heading]]");
        assert_eq!(links[0].target, "Note");
        assert_eq!(
            links[0].anchor,
            Some(Anchor::Heading("Some Heading".to_string()))
        );
        assert_eq!(links[0].display(), "Note#Some Heading");
    }

    #[test]
    fn wikilink_with_block_anchor() {
        let links = parse_links("[[Note#^abc123]]");
        assert_eq!(links[0].target, "Note");
        assert_eq!(links[0].anchor, Some(Anchor::Block("abc123".to_string())));
        assert_eq!(links[0].display(), "Note#^abc123");
    }

    #[test]
    fn wikilink_with_alias_and_anchor() {
        let links = parse_links("[[Note#Heading|Alias]]");
        assert_eq!(links[0].target, "Note");
        assert_eq!(
            links[0].anchor,
            Some(Anchor::Heading("Heading".to_string()))
        );
        assert_eq!(links[0].alias.as_deref(), Some("Alias"));
    }

    #[test]
    fn embed_link() {
        let links = parse_links("![[Diagram]]");
        assert_eq!(links[0].kind, LinkKind::Embed);
        assert_eq!(links[0].target, "Diagram");
        assert_eq!(&"![[Diagram]]"[links[0].span.clone()], "![[Diagram]]");
    }

    #[test]
    fn same_note_anchor_only() {
        let links = parse_links("[[#Heading]]");
        assert_eq!(links[0].target, "");
        assert_eq!(
            links[0].anchor,
            Some(Anchor::Heading("Heading".to_string()))
        );
        assert_eq!(links[0].display(), "#Heading");
    }

    #[test]
    fn unicode_and_spaces() {
        let links = parse_links("[[Café Résumé|Réüsumé ✨]]");
        assert_eq!(links[0].target, "Café Résumé");
        assert_eq!(links[0].alias.as_deref(), Some("Réüsumé ✨"));
    }

    #[test]
    fn standard_markdown_relative_link() {
        let links = parse_links("[a link](notes/Other.md)");
        assert_eq!(links[0].kind, LinkKind::Markdown);
        assert_eq!(links[0].target, "notes/Other.md");
        assert_eq!(links[0].alias.as_deref(), Some("a link"));
    }

    #[test]
    fn markdown_link_with_anchor() {
        let links = parse_links("[x](Other.md#Heading)");
        assert_eq!(links[0].target, "Other.md");
        assert_eq!(
            links[0].anchor,
            Some(Anchor::Heading("Heading".to_string()))
        );
    }

    #[test]
    fn external_markdown_links_are_skipped() {
        assert!(parse_links("[site](https://example.com)").is_empty());
        assert!(parse_links("[mail](mailto:a@b.com)").is_empty());
        assert!(parse_links("[proto](//cdn.example.com/x)").is_empty());
    }

    #[test]
    fn multiple_links_in_order() {
        let src = "[[A]] then [[B|b]] and [c](d.md)";
        assert_eq!(targets(src), vec!["A", "B", "d.md"]);
    }

    #[test]
    fn links_inside_fenced_code_are_ignored() {
        let src = "real [[A]]\n```\ncode [[B]] not a link\n```\nafter [[C]]";
        assert_eq!(targets(src), vec!["A", "C"]);
    }

    #[test]
    fn links_inside_inline_code_are_ignored() {
        let src = "use `[[Literal]]` verbatim but [[Real]] links";
        assert_eq!(targets(src), vec!["Real"]);
    }

    #[test]
    fn tilde_fence_is_also_skipped() {
        let src = "~~~\n[[Nope]]\n~~~\n[[Yes]]";
        assert_eq!(targets(src), vec!["Yes"]);
    }

    #[test]
    fn unterminated_wikilink_is_not_a_link() {
        assert!(parse_links("[[oops no close").is_empty());
        assert!(parse_links("a [ not a link ] b").is_empty());
    }
}
