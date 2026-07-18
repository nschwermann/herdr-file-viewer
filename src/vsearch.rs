//! Vault content search — grep the *contents* of every note in an Obsidian vault for a query, so a
//! phrase can be found across the whole vault without leaving the viewer (Obsidian's global search).
//!
//! Prefers **ripgrep** (`rg`) when it is on `PATH` — fast, respects the vault's `.gitignore`, and
//! skips hidden dirs (`.obsidian`, `.trash`) by default — and falls back to a pure, dependency-free
//! in-Rust scan of the vault's markdown files when `rg` is absent, so the feature always works. Both
//! paths use **literal** (not regex) **smartcase** matching, mirroring the in-file `/` search: a
//! lowercase query is case-insensitive, a query with any uppercase is case-sensitive.
//!
//! The `rg` invocation is behind the injected [`ContentSearcher`] seam (the live one is wired in
//! `app.rs`) so the controller — and its tests — never spawn a subprocess: an un-wired controller
//! uses [`search_fallback`] directly, keeping tests hermetic (AGENTS.md: external commands are
//! injected). Read-only (constitution §1): every path only reads files, bounded, and never writes.

use crate::obsidian::markdown_index;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The most notes the fallback scan walks. A generous bound so a pathological vault can't make the
/// scan unbounded; shared with the vault walk. `rg` is bounded by its own `--max-count`.
pub const MAX_FILES: usize = 20_000;

/// The most bytes the fallback reads from any single note. Guards a pathological file.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// The most matches `rg` reports **per file** (`--max-count`), so one note with hundreds of hits
/// can't crowd out the rest of the vault before the overall limit is reached.
pub const PER_FILE_CAP: usize = 20;

/// The longest match-line preview kept, in characters — a long minified line can't blow the row.
pub const PREVIEW_MAX_CHARS: usize = 200;

/// One content-search hit: a note, the 1-based line the query matched on, and that line's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// The note's path relative to the vault root (e.g. `folder/Note.md`) — the display path.
    pub rel: PathBuf,
    /// The note's absolute path — what the viewer opens on confirm.
    pub abs: PathBuf,
    /// The 1-based line number the match was found on (where the viewer scrolls to).
    pub line: usize,
    /// The matched line's text, whitespace-trimmed and length-capped for the preview row.
    pub preview: String,
}

/// The vault content-search seam: run a query over a vault's note contents and return the hits, best
/// bounded by `limit`. Behind a trait so the live implementation (ripgrep, in `app.rs`) is injected
/// and the controller's tests stay hermetic (an un-wired controller uses [`search_fallback`]).
pub trait ContentSearcher {
    /// Search `vault_root`'s notes for `query`, returning at most `limit` hits in a stable order.
    fn search(&self, vault_root: &Path, query: &str, limit: usize) -> Vec<SearchHit>;
}

/// Whether the query is matched case-sensitively (smartcase): case-sensitive iff it contains any
/// uppercase character, else case-insensitive. Mirrors the in-file `/` search.
pub fn is_case_sensitive(query: &str) -> bool {
    query.chars().any(|c| c.is_uppercase())
}

/// Run the vault content search the live way: ripgrep if available, else the pure fallback. Bounded
/// by `limit` (total hits). Used by the live [`ContentSearcher`] wired in `app.rs`.
pub fn search(vault_root: &Path, query: &str, limit: usize) -> Vec<SearchHit> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    match run_ripgrep(vault_root, query, limit) {
        Some(hits) => hits,
        None => search_fallback(vault_root, query, limit),
    }
}

/// Build the ripgrep argv for a vault content search: literal (`--fixed-strings`), smartcase, line
/// numbers, no heading, plain output, markdown files only, `--max-count` per file, searching
/// `vault_root`. Pure (no spawn) so the argv is unit-testable. `--` terminates flags so a query
/// starting with `-` is treated as a pattern, not an option.
pub fn ripgrep_argv(vault_root: &Path, query: &str) -> Vec<String> {
    vec![
        "rg".into(),
        "--line-number".into(),
        "--no-heading".into(),
        "--color=never".into(),
        "--smart-case".into(),
        "--fixed-strings".into(),
        "--type".into(),
        "markdown".into(),
        "--max-count".into(),
        PER_FILE_CAP.to_string(),
        "--".into(),
        query.to_string(),
        vault_root.to_string_lossy().into_owned(),
    ]
}

/// Spawn ripgrep and parse its output into hits (at most `limit`), or `None` when `rg` is not on
/// `PATH` / could not be launched (so the caller falls back). A non-zero exit with "no matches" (rg's
/// exit code 1) is a normal empty result, not a failure. Read-only: only reads via `rg`.
fn run_ripgrep(vault_root: &Path, query: &str, limit: usize) -> Option<Vec<SearchHit>> {
    let argv = ripgrep_argv(vault_root, query);
    let output = Command::new(&argv[0]).args(&argv[1..]).output().ok()?;
    // rg exits 0 (matches), 1 (no matches — a normal empty result), or 2 (an actual error). Treat
    // only a real error as a fallback trigger; 0/1 both parse (1 yields no lines).
    if let Some(code) = output.status.code()
        && code >= 2
    {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(
        stdout
            .lines()
            .filter_map(|l| parse_rg_line(l, vault_root))
            .take(limit)
            .collect(),
    )
}

/// Parse one ripgrep `--no-heading --line-number` output line — `PATH:LINE:TEXT` — into a hit, or
/// `None` when it doesn't match that shape. Scans for the first `:<digits>:` so a `PATH` containing a
/// colon (e.g. a Windows drive letter) parses correctly. Pure and total.
pub fn parse_rg_line(line: &str, vault_root: &Path) -> Option<SearchHit> {
    let mut start = 0;
    while let Some(rel) = line[start..].find(':') {
        let colon = start + rel;
        let rest = &line[colon + 1..];
        if let Some(next) = rest.find(':') {
            let num = &rest[..next];
            if !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()) {
                let path_str = &line[..colon];
                if path_str.is_empty() {
                    return None;
                }
                let lineno: usize = num.parse().ok()?;
                let text = &rest[next + 1..];
                let abs = PathBuf::from(path_str);
                let rel_path = abs
                    .strip_prefix(vault_root)
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|_| abs.clone());
                return Some(SearchHit {
                    rel: rel_path,
                    abs,
                    line: lineno,
                    preview: clip_preview(text),
                });
            }
        }
        start = colon + 1;
    }
    None
}

/// Pure, dependency-free fallback: scan every markdown note under `vault_root` for `query` (literal,
/// smartcase), returning at most `limit` hits in walk-then-line order. Read-only and bounded
/// ([`MAX_FILES`], [`MAX_FILE_BYTES`], [`PER_FILE_CAP`] per file). Used when `rg` is unavailable and
/// by the controller's tests (which never spawn a subprocess), so it must match `rg`'s semantics.
pub fn search_fallback(vault_root: &Path, query: &str, limit: usize) -> Vec<SearchHit> {
    let query = query.trim_end_matches(['\n', '\r']);
    if query.is_empty() {
        return Vec::new();
    }
    let case_sensitive = is_case_sensitive(query);
    let needle = if case_sensitive {
        query.to_string()
    } else {
        query.to_lowercase()
    };
    let mut hits = Vec::new();
    'outer: for rel in markdown_index(vault_root, MAX_FILES) {
        let abs = vault_root.join(&rel);
        let Some(source) = read_bounded(&abs) else {
            continue;
        };
        let mut per_file = 0;
        for (i, raw) in source.lines().enumerate() {
            let matched = if case_sensitive {
                raw.contains(&needle)
            } else {
                raw.to_lowercase().contains(&needle)
            };
            if matched {
                hits.push(SearchHit {
                    rel: rel.clone(),
                    abs: abs.clone(),
                    line: i + 1,
                    preview: clip_preview(raw),
                });
                per_file += 1;
                if hits.len() >= limit {
                    break 'outer;
                }
                if per_file >= PER_FILE_CAP {
                    break;
                }
            }
        }
    }
    hits
}

/// Trim a match line to a bounded, single-line preview: strip surrounding whitespace and cap the
/// length (char-boundary-safe) with an ellipsis, so a long/minified line can't blow the row width.
fn clip_preview(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= PREVIEW_MAX_CHARS {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(PREVIEW_MAX_CHARS).collect();
    out.push('…');
    out
}

/// Read up to [`MAX_FILE_BYTES`] of `path`, lossily decoded as UTF-8. `None` on any I/O error.
fn read_bounded(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_FILE_BYTES).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hfv-vsearch-{}-{}-{tag}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::create_dir_all(d.join(".obsidian")).unwrap();
        d
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn fallback_finds_matches_across_notes_with_line_numbers() {
        let root = tmp("basic");
        write(&root, "A.md", "first line\nthe quick fox\nlast");
        write(&root, "sub/B.md", "nothing here\nquick again\n");
        write(&root, "C.md", "no match");
        write(&root, ".obsidian/x.md", "quick but hidden"); // dotdir skipped

        let mut hits = search_fallback(&root, "quick", 100);
        hits.sort_by(|a, b| a.rel.cmp(&b.rel).then(a.line.cmp(&b.line)));
        assert_eq!(hits.len(), 2, "two notes match, dotdir skipped: {hits:?}");
        assert_eq!(hits[0].rel, PathBuf::from("A.md"));
        assert_eq!(hits[0].line, 2);
        assert_eq!(hits[0].preview, "the quick fox");
        assert_eq!(hits[1].rel, PathBuf::from("sub/B.md"));
        assert_eq!(hits[1].line, 2);
        assert_eq!(hits[1].abs, root.join("sub/B.md"));
    }

    #[test]
    fn fallback_is_smartcase() {
        let root = tmp("case");
        write(&root, "N.md", "Foo and foo and FOO");
        // Lowercase query: case-insensitive → matches the line (has 'foo').
        assert_eq!(search_fallback(&root, "foo", 100).len(), 1);
        // Uppercase in query: case-sensitive → 'Foo' matches only the exact case.
        let hits = search_fallback(&root, "Foo", 100);
        assert_eq!(hits.len(), 1, "the line contains 'Foo'");
        // A query with a case not present matches nothing.
        assert!(search_fallback(&root, "FOX", 100).is_empty());
    }

    #[test]
    fn fallback_respects_the_limit() {
        let root = tmp("limit");
        write(&root, "N.md", "x\nx\nx\nx\nx\n");
        assert_eq!(
            search_fallback(&root, "x", 3).len(),
            3,
            "capped at the limit"
        );
    }

    #[test]
    fn fallback_empty_query_is_empty() {
        let root = tmp("empty");
        write(&root, "N.md", "content");
        assert!(search_fallback(&root, "   ", 100).is_empty());
    }

    #[test]
    fn parse_rg_line_extracts_path_line_text() {
        let root = Path::new("/vault");
        let hit = parse_rg_line("/vault/folder/Note.md:42:the matched text", root).unwrap();
        assert_eq!(hit.rel, PathBuf::from("folder/Note.md"));
        assert_eq!(hit.abs, PathBuf::from("/vault/folder/Note.md"));
        assert_eq!(hit.line, 42);
        assert_eq!(hit.preview, "the matched text");
    }

    #[test]
    fn parse_rg_line_handles_a_colon_in_the_path_and_the_text() {
        // A Windows-style drive path (colon in PATH) and a colon in the TEXT must still parse: the
        // first `:<digits>:` is the line-number field.
        // Use a forward-slashed root so the `rel` strip is platform-independent; the point of the
        // test is that a colon in the PATH (drive letter) and a colon in the TEXT don't confuse the
        // `:<digits>:` line-field scan.
        let root = Path::new("/C:/vault");
        let hit = parse_rg_line("/C:/vault/Note.md:7:key: value", root).unwrap();
        assert_eq!(hit.line, 7);
        assert_eq!(hit.preview, "key: value");
        assert_eq!(hit.rel, PathBuf::from("Note.md"));
    }

    #[test]
    fn parse_rg_line_rejects_non_matches() {
        assert!(parse_rg_line("not a hit line", Path::new("/v")).is_none());
        assert!(parse_rg_line("", Path::new("/v")).is_none());
        assert!(parse_rg_line(":5:no path", Path::new("/v")).is_none());
    }

    #[test]
    fn ripgrep_argv_is_literal_smartcase_markdown_scoped() {
        let argv = ripgrep_argv(Path::new("/vault"), "-weird query");
        assert_eq!(argv[0], "rg");
        assert!(
            argv.contains(&"--fixed-strings".to_string()),
            "literal, not regex"
        );
        assert!(argv.contains(&"--smart-case".to_string()));
        assert!(argv.contains(&"--line-number".to_string()));
        // `--` precedes the pattern so a `-`-leading query isn't parsed as a flag.
        let dashdash = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(argv[dashdash + 1], "-weird query");
        assert_eq!(argv.last().unwrap(), "/vault");
    }

    #[test]
    fn clip_preview_trims_and_caps() {
        assert_eq!(clip_preview("  hello  "), "hello");
        let long = "a".repeat(PREVIEW_MAX_CHARS + 50);
        let clipped = clip_preview(&long);
        assert_eq!(clipped.chars().count(), PREVIEW_MAX_CHARS + 1); // + the ellipsis
        assert!(clipped.ends_with('…'));
    }
}
