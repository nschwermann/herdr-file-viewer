//! Vault tag index — `tag -> set of note paths`, built by a bounded, read-only walk of the
//! containing Obsidian vault.
//!
//! Powers the **clickable-tag filter**: clicking a `#tag` chip in a note's Properties panel (or an
//! inline `#tag` in the body) restricts the file tree to every vault note carrying that tag,
//! mirroring Obsidian's `tag:` search. A note *carries* a tag when:
//!
//! - its YAML frontmatter lists it under `tags:` / `tag:` (parsed via [`crate::mdnote`]), OR
//! - its body contains an inline `#tag` hashtag (`#` then `[A-Za-z0-9_/-]+`) that is **not** inside
//!   a fenced code block and is **not** a markdown heading (`# ` with a space is a heading, never a
//!   tag — naturally excluded, since a space is not a tag character), and is not purely numeric
//!   (Obsidian requires at least one non-numeric character in an inline tag).
//!
//! Tags are normalized **case-insensitively** with the leading `#` stripped. Obsidian **nested
//! tags** use `/` (`#project/ryoshi`): a note is indexed under the full nested tag **and every
//! ancestor prefix** (`project`, `project/ryoshi`), so clicking a parent tag reveals notes carrying
//! any child tag — Obsidian's own `tag:project` behaviour.
//!
//! Everything here is **read-only** (constitution §1): it opens notes to parse them, never writes.
//! The pure parsing surface ([`note_tags`], [`inline_tags`], [`frontmatter_tags`],
//! [`normalize_tag`], [`tag_prefixes`]) is unit-tested; only [`TagIndex::build`] touches the
//! filesystem (a bounded, dot-folder-skipping walk mirroring [`crate::obsidian::markdown_index`]).

use crate::mdnote::{Frontmatter, PropValue, split_frontmatter};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

/// The largest note read for tag parsing. A generous bound so a pathological file can't stall the
/// walk or blow memory; a real note is far smaller. Mirrors `linknav::MAX_NOTE_BYTES`.
const MAX_NOTE_BYTES: u64 = 1024 * 1024;

/// A cached snapshot of a vault's tags: each **normalized** tag (lower-cased, `#`-stripped, and
/// including every ancestor prefix of a nested tag) mapped to the set of **absolute** note paths
/// carrying it. Built once per vault and reused until a re-root/refresh invalidates it.
#[derive(Debug, Clone)]
pub struct TagIndex {
    /// Normalized tag (full nested path AND every ancestor prefix) → absolute note paths.
    tags: BTreeMap<String, BTreeSet<PathBuf>>,
    /// The vault root this index was built from, so a re-root can tell a stale cache from a fresh
    /// one (rebuild when the vault differs).
    vault_root: PathBuf,
}

impl TagIndex {
    /// Build the tag index for `vault_root` by a bounded, dot-folder-skipping walk that reads every
    /// `.md` note and extracts its tags (frontmatter + inline). `max_files` caps how many notes are
    /// scanned so a pathological tree can't stall the caller. Read-only; an unreadable dir/note is
    /// simply skipped (never panics).
    pub fn build(vault_root: &Path, max_files: usize) -> TagIndex {
        let mut tags: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
        let mut stack = vec![vault_root.to_path_buf()];
        let mut seen = 0usize;
        while let Some(dir) = stack.pop() {
            if seen >= max_files {
                break;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                // Skip dot-directories/files the way Obsidian hides its config + trash.
                if name.starts_with('.') {
                    continue;
                }
                match entry.file_type() {
                    Ok(ft) if ft.is_dir() => stack.push(path),
                    Ok(ft) if ft.is_file() => {
                        if seen >= max_files {
                            break;
                        }
                        if !crate::obsidian::is_markdown(&path) {
                            continue;
                        }
                        seen += 1;
                        if let Some(source) = read_note_bounded(&path) {
                            for tag in note_tags(&source) {
                                for prefix in tag_prefixes(&tag) {
                                    tags.entry(prefix).or_default().insert(path.clone());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        TagIndex {
            tags,
            vault_root: vault_root.to_path_buf(),
        }
    }

    /// The absolute note paths carrying `tag` (or any nested child of it — prefixes are indexed at
    /// build time). `tag` is normalized on the way in, so a clicked `#Project` matches an indexed
    /// `project`. An unknown/invalid tag yields an empty set.
    pub fn notes_for(&self, tag: &str) -> BTreeSet<PathBuf> {
        match normalize_tag(tag) {
            Some(t) => self.tags.get(&t).cloned().unwrap_or_default(),
            None => BTreeSet::new(),
        }
    }

    /// The vault root this index was built from (for stale-cache detection on a re-root).
    pub fn vault_root(&self) -> &Path {
        &self.vault_root
    }

    /// How many distinct (prefix-expanded) tags are indexed. For tests/diagnostics.
    pub fn len(&self) -> usize {
        self.tags.len()
    }

    /// Whether the index holds no tags at all.
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }
}

/// Read up to [`MAX_NOTE_BYTES`] of `path`, lossily decoded as UTF-8. `None` on any I/O error
/// (never panics), so a note that can't be read simply contributes no tags.
fn read_note_bounded(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_NOTE_BYTES).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Whether `c` is a legal Obsidian tag character: ASCII alphanumeric, `_`, `-`, or the nested-tag
/// separator `/`.
fn is_tag_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/')
}

/// The fenced-code-block marker a line opens/closes with (```` ``` ```` or `~~~`), or `None`.
/// A local copy of [`crate::mdnote`]'s (private) fence detection so the inline scanner is fence-aware.
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

/// Normalize a raw tag string: strip a leading `#`, trim, reject anything with a non-tag character
/// or a malformed slash (leading/trailing `/` or an empty `//` segment), and lower-case. Returns
/// `None` for an invalid/empty tag. Used for both frontmatter tags and the clicked-tag lookup.
pub fn normalize_tag(raw: &str) -> Option<String> {
    let t = raw.trim();
    let t = t.strip_prefix('#').unwrap_or(t).trim();
    if t.is_empty() {
        return None;
    }
    if !t.chars().all(is_tag_char) {
        return None;
    }
    if t.starts_with('/') || t.ends_with('/') || t.contains("//") {
        return None;
    }
    Some(t.to_ascii_lowercase())
}

/// Like [`normalize_tag`], with the extra Obsidian **inline** rule that a hashtag must contain at
/// least one non-numeric character (`#1234` is a number, not a tag). Applied only to inline body
/// tags, not to explicit frontmatter tags.
fn normalize_inline_tag(raw: &str) -> Option<String> {
    let t = normalize_tag(raw)?;
    if t.chars().all(|c| c.is_ascii_digit() || c == '/') {
        return None; // purely numeric (segments) → not an inline tag
    }
    Some(t)
}

/// Expand a normalized nested tag into itself and every ancestor prefix: `a/b/c` →
/// `["a", "a/b", "a/b/c"]`. A flat tag (`a`) yields just `["a"]`. This is what lets clicking a
/// parent tag reveal notes carrying any child (Obsidian's `tag:` semantics).
pub fn tag_prefixes(tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut acc = String::new();
    for seg in tag.split('/') {
        if seg.is_empty() {
            continue;
        }
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(seg);
        out.push(acc.clone());
    }
    out
}

/// Every (full, normalized) tag a note carries: its frontmatter `tags:`/`tag:` entries plus its
/// inline body hashtags. The set is de-duplicated and does NOT include ancestor prefixes — the
/// index expands those at insertion time via [`tag_prefixes`]. Pure.
pub fn note_tags(source: &str) -> BTreeSet<String> {
    let (fm, body) = split_frontmatter(source);
    let mut out = BTreeSet::new();
    if let Some(fm) = fm {
        for t in frontmatter_tags(&fm) {
            out.insert(t);
        }
    }
    for t in inline_tags(body) {
        out.insert(t);
    }
    out
}

/// Normalized tags declared in a note's frontmatter under `tags:` / `tag:` (any case). A list value
/// contributes one tag per item; a scalar value is split on whitespace and commas (Obsidian accepts
/// `tags: a b` and `tags: a, b`). Malformed items are dropped. Pure.
pub fn frontmatter_tags(fm: &Frontmatter) -> Vec<String> {
    let mut out = Vec::new();
    for (key, val) in &fm.entries {
        if !key.eq_ignore_ascii_case("tags") && !key.eq_ignore_ascii_case("tag") {
            continue;
        }
        match val {
            PropValue::List(items) => {
                for it in items {
                    if let Some(t) = normalize_tag(it) {
                        out.push(t);
                    }
                }
            }
            PropValue::Scalar(s) => {
                for piece in s.split([',', ' ', '\t']) {
                    if let Some(t) = normalize_tag(piece) {
                        out.push(t);
                    }
                }
            }
        }
    }
    out
}

/// Normalized inline `#tag` hashtags in a note **body** (frontmatter already stripped by the
/// caller). Fence-aware: a `#tag` inside a ```` ``` ````/`~~~` fenced code block is ignored. A
/// heading (`# ` with a space) is naturally excluded — a space is not a tag character. Pure.
pub fn inline_tags(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_fence: Option<&str> = None;
    for line in body.lines() {
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
        scan_line_tags(line, &mut out);
    }
    out
}

/// Scan one line for inline hashtags, pushing each normalized tag onto `out`. A hashtag starts at a
/// `#` that is not preceded by a tag character or another `#` (so it sits at a word boundary), then
/// runs over tag characters; a purely numeric run is rejected ([`normalize_inline_tag`]).
fn scan_line_tags(line: &str, out: &mut Vec<String>) {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        if chars[i] == '#' {
            // Word-boundary check: the char before `#` must not be a tag char or another `#`, so
            // `word#x` and `##x` are not tags but `(#x`, ` #x`, and a line-leading `#x` are.
            let boundary_ok = i == 0 || !(is_tag_char(chars[i - 1]) || chars[i - 1] == '#');
            let mut j = i + 1;
            while j < n && is_tag_char(chars[j]) {
                j += 1;
            }
            if boundary_ok && j > i + 1 {
                let raw: String = chars[i + 1..j].iter().collect();
                if let Some(t) = normalize_inline_tag(&raw) {
                    out.push(t);
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hfv-tagindex-{}-{}-{tag}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn mk_vault(root: &Path) {
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
    }

    #[test]
    fn normalize_strips_hash_lowercases_and_validates() {
        assert_eq!(normalize_tag("#Project"), Some("project".to_string()));
        assert_eq!(
            normalize_tag("Ryoshi-Games"),
            Some("ryoshi-games".to_string())
        );
        assert_eq!(
            normalize_tag("project/ryoshi"),
            Some("project/ryoshi".to_string())
        );
        // Invalid: empty, spaces, punctuation, malformed slashes.
        assert_eq!(normalize_tag("#"), None);
        assert_eq!(normalize_tag("a b"), None);
        assert_eq!(normalize_tag("a.b"), None);
        assert_eq!(normalize_tag("/lead"), None);
        assert_eq!(normalize_tag("trail/"), None);
        assert_eq!(normalize_tag("a//b"), None);
    }

    #[test]
    fn tag_prefixes_expands_nested_and_leaves_flat() {
        assert_eq!(tag_prefixes("a"), vec!["a".to_string()]);
        assert_eq!(
            tag_prefixes("a/b/c"),
            vec!["a".to_string(), "a/b".to_string(), "a/b/c".to_string()]
        );
    }

    #[test]
    fn inline_tags_finds_hashtags_and_skips_headings_numbers_and_fences() {
        let body = "\
# Heading is not a tag
This has an inline #ryoshi-games tag and #project/ryoshi nested.
A bare #123 is a number, not a tag, but #v2 is fine.
Mid-word email a#b is not a tag.
```
#not-a-tag inside a fence
```
Back to #done.";
        let mut tags = inline_tags(body);
        tags.sort();
        tags.dedup();
        // The scanner returns FULL tags only — ancestor-prefix expansion (`project` for
        // `project/ryoshi`) happens later, in `build`/`tag_prefixes`.
        assert_eq!(
            tags,
            vec![
                "done".to_string(),
                "project/ryoshi".to_string(),
                "ryoshi-games".to_string(),
                "v2".to_string(),
            ]
        );
    }

    #[test]
    fn note_tags_merges_frontmatter_list_scalar_and_inline() {
        // A YAML list, plus an inline body tag; all normalized + merged.
        let src = "\
---
title: Demo
tags:
  - Ryoshi-Games
  - project/ryoshi
---
Body mentions #wip and #ryoshi-games again.
";
        let tags = note_tags(src);
        assert!(tags.contains("ryoshi-games"));
        assert!(tags.contains("project/ryoshi"));
        assert!(tags.contains("wip"));
        // De-duplicated: #ryoshi-games appears in both frontmatter and body → one entry.
        assert_eq!(tags.iter().filter(|t| *t == "ryoshi-games").count(), 1);
    }

    #[test]
    fn frontmatter_scalar_tags_split_on_spaces_and_commas() {
        let src = "---\ntag: alpha, beta gamma\n---\nbody\n";
        let (fm, _) = split_frontmatter(src);
        let mut tags = frontmatter_tags(&fm.unwrap());
        tags.sort();
        assert_eq!(
            tags,
            vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
        );
    }

    #[test]
    fn build_indexes_notes_by_tag_with_nested_prefixes() {
        let root = tmp("build");
        mk_vault(&root);
        std::fs::write(root.join("A.md"), "---\ntags: [ryoshi-games]\n---\nhello\n").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/B.md"), "Body with #project/ryoshi tag\n").unwrap();
        // A note inside .obsidian must not be scanned; a non-markdown file is ignored.
        std::fs::write(root.join(".obsidian/notes.md"), "#project/ryoshi\n").unwrap();
        std::fs::write(root.join("image.png"), "#nope").unwrap();

        let index = TagIndex::build(&root, 1000);
        assert_eq!(index.vault_root(), root.as_path());

        // Direct tag.
        let games = index.notes_for("ryoshi-games");
        assert_eq!(games.len(), 1);
        assert!(games.iter().any(|p| p.ends_with("A.md")));

        // Nested tag: the note is reachable by the full tag AND the parent prefix.
        assert!(
            index
                .notes_for("project/ryoshi")
                .iter()
                .any(|p| p.ends_with("sub/B.md"))
        );
        assert!(
            index
                .notes_for("project")
                .iter()
                .any(|p| p.ends_with("sub/B.md")),
            "clicking a parent tag reveals notes carrying a child tag"
        );

        // Case-insensitive lookup; unknown tag empty.
        assert_eq!(index.notes_for("#Ryoshi-Games").len(), 1);
        assert!(index.notes_for("missing").is_empty());

        std::fs::remove_dir_all(&root).ok();
    }
}
