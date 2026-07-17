//! Obsidian markdown note transforms — a pure preprocessing layer for the rendered (`v`) view.
//!
//! The content pane delegates markdown styling to `glow`, which knows nothing about Obsidian's
//! extensions. Rather than reinvent a renderer (constitution: delegate rendering), this module
//! **rewrites the markdown source** before it is piped to glow so glow renders the result:
//!
//! - **Frontmatter → a Properties panel** ([`split_frontmatter`] + [`properties_markdown`]): the
//!   leading YAML block, otherwise hidden, becomes a clean key/value table (tags/aliases as
//!   chips) prepended above the body. Toggleable, like Obsidian.
//! - **Callouts** ([`transform_callouts`]): `> [!note] Title` blocks become titled, bordered
//!   blockquotes instead of literal `[!note]` text, honouring a custom title and the foldable
//!   `+`/`-` marker.
//! - **Task checkboxes** ([`transform_tasks`]): `- [ ]` / `- [x]` render as `☐` / `☑` glyphs so
//!   they read as checkboxes even in the plain-text fallback. (Rendering only — toggling would
//!   write the file, which the read-only constitution forbids, so it is intentionally omitted.)
//! - **Heading outline** ([`headings`]): the note's ATX headings, with 1-based source line
//!   numbers, feed the jump-through outline overlay.
//!
//! Everything is pure and fence-aware (a `#`, `>`, or `- [ ]` inside a fenced code block is left
//! alone) — no I/O, no allocation of the file beyond the transformed copy.

/// A parsed frontmatter property value: a scalar or a list (tags/aliases).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropValue {
    Scalar(String),
    List(Vec<String>),
}

/// Parsed YAML frontmatter — ordered key/value entries (insertion order preserved for a stable
/// Properties table). A deliberately small YAML subset: `key: scalar`, inline `key: [a, b]`, and
/// block lists (`key:` then indented `- item` lines). Nested maps are not modelled (rare in a
/// note's frontmatter) and are captured as scalars.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Frontmatter {
    pub entries: Vec<(String, PropValue)>,
}

impl Frontmatter {
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Split a markdown source into `(frontmatter, body)`. Frontmatter is a leading `---` line, YAML
/// lines, and a closing `---`/`...` line at the very top of the file. Returns `(None, source)`
/// when there is no well-formed frontmatter block, so a plain note is untouched.
pub fn split_frontmatter(source: &str) -> (Option<Frontmatter>, &str) {
    // The block must start on the very first line (allow a leading BOM).
    let s = source.strip_prefix('\u{feff}').unwrap_or(source);
    let first_line_end = s.find('\n');
    let first = match first_line_end {
        Some(e) => &s[..e],
        None => s,
    };
    if first.trim_end() != "---" {
        return (None, source);
    }
    // Find the closing fence line (`---` or `...`).
    let after_first = first_line_end.map(|e| e + 1).unwrap_or(s.len());
    let mut idx = after_first;
    let mut yaml = String::new();
    let mut closed_at = None;
    // Iterate line by line from after the opening fence.
    for line in s[after_first..].split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" || trimmed == "..." {
            closed_at = Some(idx + line.len());
            break;
        }
        yaml.push_str(line);
        idx += line.len();
    }
    match closed_at {
        Some(body_start) => {
            let fm = parse_frontmatter(&yaml);
            // Skip a single blank line right after the closing fence for tidy spacing.
            let body = &s[body_start..];
            let body = body.strip_prefix('\n').unwrap_or(body);
            (Some(fm), body)
        }
        None => (None, source), // unterminated → not frontmatter
    }
}

/// Parse the YAML text between the `---` fences into ordered [`Frontmatter`] entries.
pub fn parse_frontmatter(yaml: &str) -> Frontmatter {
    let mut entries: Vec<(String, PropValue)> = Vec::new();
    let mut lines = yaml.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        // Only treat top-level (unindented) `key:` lines as properties.
        if line.starts_with([' ', '\t']) {
            continue;
        }
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_string();
        if key.is_empty() {
            continue;
        }
        let rest = rest.trim();
        if rest.is_empty() {
            // A block list may follow on indented `- item` lines.
            let mut items = Vec::new();
            while let Some(next) = lines.peek() {
                let t = next.trim_start();
                if next.starts_with([' ', '\t']) && t.starts_with('-') {
                    let item = t[1..].trim();
                    if !item.is_empty() {
                        items.push(unquote(item).to_string());
                    }
                    lines.next();
                } else if next.trim().is_empty() {
                    lines.next();
                } else {
                    break;
                }
            }
            if items.is_empty() {
                entries.push((key, PropValue::Scalar(String::new())));
            } else {
                entries.push((key, PropValue::List(items)));
            }
        } else if let Some(inner) = rest.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            // Inline list `[a, b, c]`.
            let items: Vec<String> = inner
                .split(',')
                .map(|s| unquote(s.trim()).to_string())
                .filter(|s| !s.is_empty())
                .collect();
            entries.push((key, PropValue::List(items)));
        } else {
            entries.push((key, PropValue::Scalar(unquote(rest).to_string())));
        }
    }
    Frontmatter { entries }
}

/// Strip matching surrounding single/double quotes from a YAML scalar.
fn unquote(s: &str) -> &str {
    let s = s.trim();
    for q in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Keys whose values Obsidian shows as chips (each item a distinct pill). `tags` also gets a
/// leading `#` per chip.
fn is_chip_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "tags" | "tag" | "aliases" | "alias" | "cssclasses" | "cssclass"
    )
}

/// Render frontmatter as a markdown **Properties** block to prepend above the body: a titled
/// table with one row per property. List values render as chips (inline code), and a `tags` list
/// gets a leading `#` per chip, mirroring Obsidian's properties view. Returns an empty string for
/// empty frontmatter.
pub fn properties_markdown(fm: &Frontmatter) -> String {
    if fm.is_empty() {
        return String::new();
    }
    let mut out = String::from("### Properties\n\n| Property | Value |\n| --- | --- |\n");
    for (key, val) in &fm.entries {
        let rendered = match val {
            PropValue::Scalar(s) => format_scalar(key, s),
            PropValue::List(items) => {
                let tag_prefix =
                    key.eq_ignore_ascii_case("tags") || key.eq_ignore_ascii_case("tag");
                items
                    .iter()
                    .map(|it| {
                        let it = it.trim_start_matches('#');
                        if tag_prefix {
                            format!("`#{it}`")
                        } else if is_chip_key(key) {
                            format!("`{it}`")
                        } else {
                            it.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            }
        };
        // Escape a pipe in a value so it can't break the markdown table.
        let rendered = rendered.replace('|', "\\|");
        out.push_str(&format!("| {} | {} |\n", key.replace('|', "\\|"), rendered));
    }
    out.push_str("\n---\n\n");
    out
}

/// Format a scalar property value: a bare `[[wikilink]]` or `#tag` stays as-is (glow/our wikilink
/// layer style it), a URL becomes a markdown link, everything else is plain text.
fn format_scalar(key: &str, value: &str) -> String {
    let v = value.trim();
    if v.is_empty() {
        return String::new();
    }
    if is_chip_key(key) {
        let v = v.trim_start_matches('#');
        return if key.eq_ignore_ascii_case("tags") {
            format!("`#{v}`")
        } else {
            format!("`{v}`")
        };
    }
    if v.starts_with("http://") || v.starts_with("https://") {
        return format!("[{v}]({v})");
    }
    v.to_string()
}

/// Icon + uppercase label for a callout type (lower-cased). Unknown types fall back to a generic
/// note marker. All markers are width-1 BMP glyphs (no emoji) so glow's box layout stays aligned.
fn callout_style(kind: &str) -> (&'static str, String) {
    let (icon, label) = match kind {
        "note" => ("▸", "NOTE"),
        "abstract" | "summary" | "tldr" => ("▤", "SUMMARY"),
        "info" => ("ℹ", "INFO"),
        "todo" => ("☐", "TODO"),
        "tip" | "hint" | "important" => ("★", "TIP"),
        "success" | "check" | "done" => ("✓", "SUCCESS"),
        "question" | "help" | "faq" => ("?", "QUESTION"),
        "warning" | "caution" | "attention" => ("⚠", "WARNING"),
        "failure" | "fail" | "missing" => ("✗", "FAILURE"),
        "danger" | "error" => ("‼", "DANGER"),
        "bug" => ("✷", "BUG"),
        "example" => ("▤", "EXAMPLE"),
        "quote" | "cite" => ("❝", "QUOTE"),
        other => ("▸", &*other.to_uppercase()),
    };
    (icon, label.to_string())
}

/// Rewrite Obsidian callouts into titled blockquotes glow renders as bordered boxes.
///
/// A callout header `> [!type]<fold> <title>` becomes `> **icon LABEL — title**`; the foldable
/// marker (`+`/`-`) is shown as a `▾`/`▸` indicator rather than left literal. Body lines (further
/// `>` lines) are untouched, so glow keeps them inside the same blockquote border. Fence-aware:
/// a `>` line inside a code fence is not treated as a callout.
pub fn transform_callouts(body: &str) -> String {
    let mut out = String::with_capacity(body.len() + 32);
    let mut in_fence: Option<&str> = None;
    for line in body.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        if let Some(marker) = fence_marker(content) {
            in_fence = match in_fence {
                Some(open) if open == marker => None,
                None => Some(marker),
                other => other,
            };
            out.push_str(line);
            continue;
        }
        if in_fence.is_none()
            && let Some(rewritten) = rewrite_callout_header(content)
        {
            out.push_str(&rewritten);
            if line.ends_with('\n') {
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
    }
    out
}

/// If `line` is a callout header (`> [!type]...`), return the rewritten header line; else `None`.
fn rewrite_callout_header(line: &str) -> Option<String> {
    let indent_len = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_len);
    let rest = rest.strip_prefix('>')?;
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    let inner = rest.strip_prefix("[!")?;
    let close = inner.find(']')?;
    let kind = inner[..close].trim().to_lowercase();
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    let after = &inner[close + 1..];
    // Optional foldable marker directly after `]`.
    let (fold, title) = match after.strip_prefix('+') {
        Some(t) => (" ▾", t.trim()),
        None => match after.strip_prefix('-') {
            Some(t) => (" ▸", t.trim()),
            None => ("", after.trim()),
        },
    };
    let (icon, label) = callout_style(&kind);
    let heading = if title.is_empty() {
        format!("{icon} {label}{fold}")
    } else {
        format!("{icon} {label}{fold} — {title}")
    };
    Some(format!("{indent}> **{heading}**"))
}

/// Rewrite GitHub/Obsidian task list items into checkbox glyphs (`☐`/`☑`). Rendering only — the
/// read-only viewer never writes the toggled state back. Fence-aware.
pub fn transform_tasks(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut in_fence: Option<&str> = None;
    for line in body.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        if let Some(marker) = fence_marker(content) {
            in_fence = match in_fence {
                Some(open) if open == marker => None,
                None => Some(marker),
                other => other,
            };
            out.push_str(line);
            continue;
        }
        if in_fence.is_none()
            && let Some(rewritten) = rewrite_task_line(content)
        {
            out.push_str(&rewritten);
            if line.ends_with('\n') {
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
    }
    out
}

/// If `line` is a task list item (`- [ ]`, `* [x]`, `+ [X]`, optionally indented), rewrite the
/// `[ ]`/`[x]` marker to `☐`/`☑`; else `None`.
fn rewrite_task_line(line: &str) -> Option<String> {
    let indent_len = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_len);
    let bullet = rest.chars().next()?;
    if !matches!(bullet, '-' | '*' | '+') {
        return None;
    }
    let rest = &rest[1..];
    let rest = rest.strip_prefix(' ')?;
    let mark = rest.strip_prefix("[ ]").map(|r| ('☐', r)).or_else(|| {
        rest.strip_prefix("[x]")
            .or_else(|| rest.strip_prefix("[X]"))
            .map(|r| ('☑', r))
    })?;
    let (glyph, after) = mark;
    let after = after.strip_prefix(' ').unwrap_or(after);
    Some(format!("{indent}{bullet} {glyph} {after}"))
}

/// A heading in the note's outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// ATX level 1..=6.
    pub level: u8,
    /// The heading text (markers and trailing `#`s stripped, trimmed).
    pub text: String,
    /// 1-based source line number, for the jump-to-heading scroll.
    pub line: usize,
}

/// Parse the note's ATX headings (`# …` … `###### …`) for the outline overlay, with 1-based
/// source line numbers. Fence-aware and frontmatter-aware (a `#` inside a code fence or in the
/// YAML block is not a heading). Setext headings are not modelled (rare in notes).
pub fn headings(source: &str) -> Vec<Heading> {
    // Skip the frontmatter block so a `#comment` in YAML never counts.
    let (fm, _) = split_frontmatter(source);
    let skip_lines = if fm.is_some() {
        frontmatter_line_count(source)
    } else {
        0
    };

    let mut out = Vec::new();
    let mut in_fence: Option<&str> = None;
    for (i, line) in source.lines().enumerate() {
        let lineno = i + 1;
        if lineno <= skip_lines {
            continue;
        }
        if let Some(marker) = fence_marker(line) {
            in_fence = match in_fence {
                Some(open) if open == marker => None,
                None => Some(marker),
                other => other,
            };
            continue;
        }
        if in_fence.is_some() {
            continue;
        }
        let trimmed = line.trim_start();
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) {
            let after = &trimmed[hashes..];
            // An ATX heading requires a space after the `#`s (or an empty heading).
            if after.is_empty() || after.starts_with([' ', '\t']) {
                let text = after.trim().trim_end_matches('#').trim().to_string();
                out.push(Heading {
                    level: hashes as u8,
                    text,
                    line: lineno,
                });
            }
        }
    }
    out
}

/// How many leading source lines the frontmatter block occupies (opening fence, YAML, closing
/// fence), for skipping when scanning headings. 0 when there is no frontmatter.
fn frontmatter_line_count(source: &str) -> usize {
    let s = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut lines = s.lines();
    if lines.next().map(|l| l.trim_end()) != Some("---") {
        return 0;
    }
    let mut n = 1;
    for line in lines {
        n += 1;
        let t = line.trim_end();
        if t == "---" || t == "..." {
            return n;
        }
    }
    0
}

/// The fenced-code-block marker a line opens/closes with (```` ``` ```` or `~~~`), or `None`.
fn fence_marker(line: &str) -> Option<&'static str> {
    let t = line.trim_start();
    if t.starts_with("```") {
        Some("```")
    } else if t.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

/// The full markdown preprocessing applied before glow renders the `v` view: optionally prepend
/// the Properties panel (else drop the frontmatter, matching the hidden default), then transform
/// callouts and task checkboxes on the body. Pure.
pub fn preprocess(source: &str, show_properties: bool) -> String {
    let (fm, body) = split_frontmatter(source);
    let body = transform_tasks(&transform_callouts(body));
    match (show_properties, fm) {
        (true, Some(fm)) if !fm.is_empty() => {
            let mut out = properties_markdown(&fm);
            out.push_str(&body);
            out
        }
        // No properties shown, or none present: just the (transformed) body — frontmatter hidden.
        _ => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_frontmatter_extracts_block_and_body() {
        let src = "---\ntitle: Hello\ntags: [a, b]\n---\n\n# Body\ntext\n";
        let (fm, body) = split_frontmatter(src);
        let fm = fm.expect("frontmatter present");
        assert_eq!(
            fm.entries[0],
            ("title".to_string(), PropValue::Scalar("Hello".to_string()))
        );
        assert_eq!(
            fm.entries[1],
            (
                "tags".to_string(),
                PropValue::List(vec!["a".to_string(), "b".to_string()])
            )
        );
        assert_eq!(body, "# Body\ntext\n");
    }

    #[test]
    fn split_frontmatter_block_list() {
        let src = "---\naliases:\n  - Foo\n  - Bar\ndate: 2026-07-17\n---\nbody";
        let (fm, body) = split_frontmatter(src);
        let fm = fm.unwrap();
        assert_eq!(
            fm.entries[0],
            (
                "aliases".to_string(),
                PropValue::List(vec!["Foo".to_string(), "Bar".to_string()])
            )
        );
        assert_eq!(
            fm.entries[1],
            (
                "date".to_string(),
                PropValue::Scalar("2026-07-17".to_string())
            )
        );
        assert_eq!(body, "body");
    }

    #[test]
    fn no_frontmatter_is_untouched() {
        let src = "# Just a heading\n\nsome --- text";
        let (fm, body) = split_frontmatter(src);
        assert!(fm.is_none());
        assert_eq!(body, src);
    }

    #[test]
    fn unterminated_frontmatter_is_not_parsed() {
        let src = "---\ntitle: x\nno closing fence\n";
        let (fm, body) = split_frontmatter(src);
        assert!(fm.is_none());
        assert_eq!(body, src);
    }

    #[test]
    fn properties_markdown_renders_table_with_chips() {
        let fm = Frontmatter {
            entries: vec![
                ("title".to_string(), PropValue::Scalar("Hi".to_string())),
                (
                    "tags".to_string(),
                    PropValue::List(vec!["project".to_string(), "wip".to_string()]),
                ),
            ],
        };
        let md = properties_markdown(&fm);
        assert!(md.contains("### Properties"));
        assert!(md.contains("| title | Hi |"));
        assert!(md.contains("`#project`"));
        assert!(md.contains("`#wip`"));
        assert!(md.trim_end().ends_with("---"));
    }

    #[test]
    fn callout_becomes_titled_blockquote() {
        let out = transform_callouts("> [!warning] Watch out\n> body\n");
        let first = out.lines().next().unwrap();
        assert!(first.starts_with("> **"), "callout header bolded: {first}");
        assert!(first.contains("WARNING"));
        assert!(first.contains("Watch out"));
        // Body line untouched (still in the blockquote).
        assert!(out.contains("> body"));
    }

    #[test]
    fn callout_without_title_and_foldable_marker() {
        let out = transform_callouts("> [!note]-\n> hidden by default\n");
        let first = out.lines().next().unwrap();
        assert!(first.contains("NOTE"));
        assert!(
            first.contains('▸'),
            "collapsed foldable marker shown: {first}"
        );
    }

    #[test]
    fn callout_custom_type_uppercased() {
        let out = transform_callouts("> [!custom] Title\n");
        assert!(out.contains("CUSTOM"));
        assert!(out.contains("Title"));
    }

    #[test]
    fn callout_inside_fence_is_untouched() {
        let src = "```\n> [!note] not a callout\n```\n";
        assert_eq!(transform_callouts(src), src);
    }

    #[test]
    fn tasks_become_checkbox_glyphs() {
        let out = transform_tasks("- [ ] todo\n- [x] done\n  - [X] nested done\n");
        assert!(out.contains("- ☐ todo"));
        assert!(out.contains("- ☑ done"));
        assert!(out.contains("  - ☑ nested done"));
    }

    #[test]
    fn tasks_inside_fence_untouched() {
        let src = "```\n- [ ] literal\n```\n";
        assert_eq!(transform_tasks(src), src);
    }

    #[test]
    fn non_task_bullets_untouched() {
        let src = "- plain bullet\n* another\n";
        assert_eq!(transform_tasks(src), src);
    }

    #[test]
    fn headings_parsed_with_line_numbers() {
        let src = "---\ntitle: x\n---\n# One\ntext\n## Two\n### Three\n";
        let hs = headings(src);
        assert_eq!(hs.len(), 3);
        assert_eq!(
            hs[0],
            Heading {
                level: 1,
                text: "One".into(),
                line: 4
            }
        );
        assert_eq!(
            hs[1],
            Heading {
                level: 2,
                text: "Two".into(),
                line: 6
            }
        );
        assert_eq!(
            hs[2],
            Heading {
                level: 3,
                text: "Three".into(),
                line: 7
            }
        );
    }

    #[test]
    fn headings_skip_fenced_code_and_yaml() {
        let src = "# Real\n```\n# fake in code\n```\n## Also real\n";
        let hs = headings(src);
        assert_eq!(
            hs.iter().map(|h| h.text.as_str()).collect::<Vec<_>>(),
            vec!["Real", "Also real"]
        );
    }

    #[test]
    fn headings_require_space_after_hashes() {
        // `#tag` is not a heading; `# Heading` is.
        let hs = headings("#notheading\n# Heading\n");
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].text, "Heading");
    }

    #[test]
    fn preprocess_prepends_properties_when_enabled() {
        let src = "---\ntitle: Note\n---\n# Body\n- [ ] task\n";
        let with = preprocess(src, true);
        assert!(with.contains("### Properties"));
        assert!(with.contains("# Body"));
        assert!(with.contains("- ☐ task"));

        let without = preprocess(src, false);
        assert!(
            !without.contains("### Properties"),
            "properties hidden when toggled off"
        );
        assert!(without.contains("# Body"));
        assert!(without.contains("- ☐ task"));
    }

    #[test]
    fn preprocess_plain_note_is_body_only() {
        let src = "# Title\n\ntext\n";
        assert_eq!(preprocess(src, true), src);
    }
}
