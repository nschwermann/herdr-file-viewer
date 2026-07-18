//! Obsidian vault awareness — vault detection, the `obsidian://` open URI, the vault-wide
//! markdown index, and Obsidian-style link-target resolution.
//!
//! Shared by the editor hand-off (the `e` key opens a vault `.md` in Obsidian instead of the
//! default editor) and by inline wikilink navigation (the `g` link navigator + follow/back
//! keys). Everything here is **read-only**: it inspects the filesystem to locate a vault and
//! enumerate its notes, and builds URI/target strings, but never writes (constitution §1).
//!
//! A **vault** is any ancestor directory of a file that contains a `.obsidian/` config folder;
//! the *nearest* such ancestor wins, so a nested vault resolves to the innermost one. The
//! detection, name, and vault-relative path are pure over their `Path` inputs; only
//! [`markdown_index`] touches the filesystem (a bounded, dotfolder-skipping walk).

use std::path::{Path, PathBuf};

/// A located Obsidian vault: the directory that holds the `.obsidian/` config folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vault {
    /// The vault root — the ancestor directory that contains `.obsidian/`.
    pub root: PathBuf,
}

impl Vault {
    /// The vault name Obsidian uses in a URI: the basename of the vault root. Empty only for a
    /// filesystem root, which is never a real vault.
    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// `path` made relative to the vault root (how Obsidian keys a note), or `None` when `path`
    /// is not inside the vault. Purely lexical over already-absolute inputs.
    pub fn relative<'p>(&self, path: &'p Path) -> Option<&'p Path> {
        path.strip_prefix(&self.root).ok()
    }
}

/// Find the Obsidian vault that owns `target`, if any: the **nearest** ancestor directory that
/// contains a `.obsidian/` folder. For a file, the search starts at its parent; for a directory,
/// at the directory itself. Returns `None` when no ancestor is a vault.
///
/// "Nearest wins" gives Obsidian's nested-vault behaviour: a note under `outer/inner/note.md`,
/// where both `outer/` and `outer/inner/` are vaults, resolves to `outer/inner`. The check is a
/// cheap `.obsidian` directory probe per ancestor — no vault contents are read here.
pub fn find_vault(target: &Path) -> Option<Vault> {
    // A file's own path is not an ancestor dir; begin at its parent. A directory searches itself
    // first (a vault root selected in the tree should detect as its own vault).
    let start: &Path = if target.is_dir() {
        target
    } else {
        target.parent()?
    };
    for ancestor in start.ancestors() {
        if ancestor.join(".obsidian").is_dir() {
            return Some(Vault {
                root: ancestor.to_path_buf(),
            });
        }
    }
    None
}

/// Whether `path` names a markdown file (by extension, case-insensitive) — the file class that
/// participates in vault detection for the editor hand-off and in wikilink resolution.
pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
        .unwrap_or(false)
}

/// Build the `obsidian://open?vault=<name>&file=<file>` URI for a vault-relative path.
///
/// Both the vault name and the file path are percent-encoded per RFC 3986 (see
/// [`percent_encode`]), so spaces, unicode, and path separators survive intact. The `.md`
/// extension is dropped from `file` (Obsidian addresses a note by name, not filename), and
/// backslashes are normalised to `/` so a Windows vault-relative path addresses the same note.
pub fn open_uri(vault_name: &str, vault_relative: &Path) -> String {
    let rel = vault_relative.to_string_lossy();
    // Normalise separators, then strip a trailing `.md`/`.markdown` (case-insensitive) — Obsidian
    // opens by note name without the extension.
    let rel = rel.replace('\\', "/");
    let file = strip_md_ext(&rel);
    format!(
        "obsidian://open?vault={}&file={}",
        percent_encode(vault_name),
        percent_encode(file),
    )
}

/// Strip a trailing `.md` / `.markdown` extension (case-insensitive) from a note path, leaving
/// any other suffix untouched. Returns a borrowed slice when nothing is stripped.
fn strip_md_ext(s: &str) -> &str {
    for ext in [".md", ".markdown"] {
        if s.len() >= ext.len() && s[s.len() - ext.len()..].eq_ignore_ascii_case(ext) {
            return &s[..s.len() - ext.len()];
        }
    }
    s
}

/// Percent-encode a string per RFC 3986: the unreserved set (`A-Z a-z 0-9 - . _ ~`) passes
/// through; every other byte — including space, `/`, and the multi-byte UTF-8 of non-ASCII —
/// becomes `%XX` with uppercase hex. This is what Obsidian's "Copy Obsidian URL" produces, so a
/// note whose name or folder contains spaces or unicode addresses correctly.
pub fn percent_encode(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

/// Enumerate every markdown note in the vault, as **vault-relative** paths, for wikilink
/// resolution. A bounded recursive walk that skips dot-directories (`.obsidian`, `.git`,
/// `.trash`, …) the way Obsidian excludes its own config and system folders. Read-only; returns
/// an empty list on any I/O error rather than propagating it (never panics).
///
/// `max_files` caps the walk so a pathological tree can't stall the UI thread; the caller passes
/// a generous bound. The order is the walk order (deterministic per filesystem enumeration is
/// not guaranteed, so resolution never relies on it — it ranks candidates explicitly).
pub fn markdown_index(vault_root: &Path, max_files: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![vault_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= max_files {
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
                    if is_markdown(&path)
                        && let Ok(rel) = path.strip_prefix(vault_root)
                    {
                        out.push(rel.to_path_buf());
                        if out.len() >= max_files {
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Resolve a wikilink/markdown-link **target** to a vault-relative note path the Obsidian way:
/// shortest-unique-path matching against the whole vault (NOT a filesystem-relative join).
///
/// `target` is the link's path part (anchor and alias already stripped by the parser), e.g.
/// `Note`, `folder/Note`, or `Note.md`. `source_rel` is the vault-relative path of the note the
/// link lives in (used to prefer a same-folder match on a tie). `index` is [`markdown_index`]'s
/// output. Returns the matched vault-relative path, or `None` when nothing matches (an
/// unresolved link).
///
/// Matching, in Obsidian's spirit:
/// 1. If `target` contains a `/`, it is a (possibly extension-less) **path**: match a note whose
///    vault-relative path equals it (case-insensitively), with or without the `.md` extension.
/// 2. Otherwise `target` is a **basename**: match every note whose file stem equals it
///    (case-insensitively). Among matches, prefer one in the *same folder* as the source, then
///    the **shortest path** (fewest components), breaking further ties lexicographically — so a
///    bare `[[Note]]` deterministically lands on the closest/shallowest `Note.md`.
pub fn resolve_target(target: &str, source_rel: &Path, index: &[PathBuf]) -> Option<PathBuf> {
    let needle = target.trim();
    if needle.is_empty() {
        return None;
    }
    let needle_norm = normalize_link_path(needle);

    if needle.contains('/') {
        // Path form: compare against each note's path, with and without the .md extension.
        return index
            .iter()
            .find(|rel| {
                let rel_str = normalize_link_path(&rel.to_string_lossy());
                let rel_no_ext = normalize_link_path(strip_md_ext(&rel.to_string_lossy()));
                rel_str == needle_norm || rel_no_ext == needle_norm
            })
            .cloned();
    }

    // Basename form: collect every note whose stem matches, then rank.
    let source_dir = source_rel.parent();
    let mut matches: Vec<&PathBuf> = index
        .iter()
        .filter(|rel| {
            rel.file_stem()
                .map(|s| s.to_string_lossy().to_lowercase() == needle_norm)
                .unwrap_or(false)
        })
        .collect();
    if matches.is_empty() {
        return None;
    }
    matches.sort_by(|a, b| {
        let a_same = a.parent() == source_dir;
        let b_same = b.parent() == source_dir;
        // Same-folder first, then fewest path components, then lexicographic path.
        b_same
            .cmp(&a_same)
            .then_with(|| a.components().count().cmp(&b.components().count()))
            .then_with(|| a.as_os_str().cmp(b.as_os_str()))
    });
    matches.first().map(|p| (*p).clone())
}

/// Resolve an image/attachment embed target to an **absolute** file path. `note` is the markdown
/// file the embed lives in. Tries note-relative first (handles a standard `![](sub/pic.png)` and a
/// same-folder `![[pic.png]]`); for a wiki embed that misses, resolves it the Obsidian way — search
/// the whole vault for a file with that name, shortest path winning. `max_files` bounds that walk.
/// Returns `None` when nothing matches. Read-only (a bounded `read_dir` walk + `is_file`).
pub fn find_attachment(note: &Path, target: &str, wiki: bool, max_files: usize) -> Option<PathBuf> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    // 1) Note-relative (covers standard markdown images and same-folder wiki embeds).
    if let Some(dir) = note.parent() {
        let rel = dir.join(target);
        if rel.is_file() {
            return Some(rel);
        }
    }
    // 2) Wiki embeds resolve vault-wide.
    if wiki && let Some(vault) = find_vault(note) {
        let vabs = vault.root.join(target);
        if vabs.is_file() {
            return Some(vabs);
        }
        return find_file_by_name(&vault.root, target, max_files);
    }
    None
}

/// Find the file in `vault_root` whose file name equals `name`'s basename (case-insensitive),
/// preferring the shortest vault-relative path (fewest components), then lexicographic. A bounded,
/// dotdir-skipping walk mirroring [`markdown_index`]. Returns an absolute path.
fn find_file_by_name(vault_root: &Path, name: &str, max_files: usize) -> Option<PathBuf> {
    let needle = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .to_lowercase();
    if needle.is_empty() {
        return None;
    }
    let mut best: Option<PathBuf> = None;
    let mut best_key: Option<(usize, std::ffi::OsString)> = None;
    let mut seen = 0usize;
    let mut stack = vec![vault_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if seen >= max_files {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ename = entry.file_name();
            if ename.to_string_lossy().starts_with('.') {
                continue;
            }
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => stack.push(path),
                Ok(ft) if ft.is_file() => {
                    seen += 1;
                    if ename.to_string_lossy().to_lowercase() != needle {
                        continue;
                    }
                    let rel = path.strip_prefix(vault_root).unwrap_or(&path);
                    let key = (rel.components().count(), rel.as_os_str().to_os_string());
                    if best_key.as_ref().is_none_or(|b| key < *b) {
                        best_key = Some(key);
                        best = Some(path.clone());
                    }
                }
                _ => {}
            }
        }
    }
    best
}

/// Lower-case and normalise separators for a lenient, case-insensitive path comparison (Obsidian
/// treats links case-insensitively on the common platforms). Also strips a leading `./`.
fn normalize_link_path(s: &str) -> String {
    let s = s.replace('\\', "/");
    let s = s.strip_prefix("./").unwrap_or(&s);
    s.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// A fresh, unique temp dir for a test (no tempfile dep — matches the project's hermetic style).
    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hfv-obsidian-{}-{}-{tag}",
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
    fn find_vault_detects_ancestor_with_obsidian_dir() {
        let root = tmp("vault");
        mk_vault(&root);
        let sub = root.join("notes/sub");
        std::fs::create_dir_all(&sub).unwrap();
        let note = sub.join("Note.md");
        std::fs::write(&note, "# hi").unwrap();

        let v = find_vault(&note).expect("a vault ancestor");
        assert_eq!(v.root, root);
        assert_eq!(v.name(), root.file_name().unwrap().to_string_lossy());
        assert_eq!(v.relative(&note).unwrap(), Path::new("notes/sub/Note.md"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn find_vault_nearest_ancestor_wins_for_nested_vaults() {
        // outer/ and outer/inner/ are BOTH vaults; a note under inner resolves to the inner one.
        let outer = tmp("outer");
        mk_vault(&outer);
        let inner = outer.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        mk_vault(&inner);
        let note = inner.join("deep/Note.md");
        std::fs::create_dir_all(note.parent().unwrap()).unwrap();
        std::fs::write(&note, "x").unwrap();

        let v = find_vault(&note).expect("a vault");
        assert_eq!(v.root, inner, "the nearest ancestor vault wins");
        std::fs::remove_dir_all(&outer).ok();
    }

    #[test]
    fn find_vault_none_outside_a_vault() {
        let dir = tmp("plain");
        let note = dir.join("Note.md");
        std::fs::write(&note, "x").unwrap();
        assert_eq!(find_vault(&note), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_uri_encodes_vault_and_file_and_drops_md_extension() {
        let uri = open_uri("My Vault", Path::new("Daily Notes/2026-07-17.md"));
        assert_eq!(
            uri,
            "obsidian://open?vault=My%20Vault&file=Daily%20Notes%2F2026-07-17"
        );
    }

    #[test]
    fn open_uri_encodes_unicode() {
        // A café note in a résumé folder: multi-byte UTF-8 must percent-encode byte-by-byte.
        let uri = open_uri("Zettel", Path::new("café/résumé.md"));
        assert_eq!(
            uri,
            "obsidian://open?vault=Zettel&file=caf%C3%A9%2Fr%C3%A9sum%C3%A9"
        );
    }

    #[test]
    fn percent_encode_leaves_unreserved_and_encodes_the_rest() {
        assert_eq!(percent_encode("aZ0-._~"), "aZ0-._~");
        assert_eq!(percent_encode("a b/c?d&e"), "a%20b%2Fc%3Fd%26e");
    }

    #[test]
    fn markdown_index_lists_notes_and_skips_dotdirs() {
        let root = tmp("idx");
        mk_vault(&root);
        std::fs::write(root.join("A.md"), "x").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/B.md"), "x").unwrap();
        // A note inside .obsidian and a non-markdown file must not appear.
        std::fs::write(root.join(".obsidian/workspace.md"), "x").unwrap();
        std::fs::write(root.join("image.png"), "x").unwrap();

        let mut idx = markdown_index(&root, 1000);
        idx.sort();
        assert_eq!(idx, vec![PathBuf::from("A.md"), PathBuf::from("sub/B.md")]);
        std::fs::remove_dir_all(&root).ok();
    }

    fn index(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn resolve_bare_name_matches_stem() {
        let idx = index(&["notes/Target.md", "other/Thing.md"]);
        assert_eq!(
            resolve_target("Target", Path::new("src/Home.md"), &idx),
            Some(PathBuf::from("notes/Target.md"))
        );
    }

    #[test]
    fn resolve_prefers_same_folder_then_shortest_path() {
        // Three notes named Target: one at the vault root (shortest), one in the source folder,
        // one deep. Same-folder must win over shortest-path.
        let idx = index(&["Target.md", "a/b/Target.md", "src/Target.md"]);
        assert_eq!(
            resolve_target("Target", Path::new("src/Home.md"), &idx),
            Some(PathBuf::from("src/Target.md")),
            "same folder as the source note wins the tie"
        );
        // With no same-folder match, the shortest path wins.
        let idx2 = index(&["a/b/Target.md", "z/Target.md"]);
        assert_eq!(
            resolve_target("Target", Path::new("src/Home.md"), &idx2),
            Some(PathBuf::from("z/Target.md")),
            "fewest components wins when neither is same-folder"
        );
    }

    #[test]
    fn resolve_path_form_matches_with_or_without_extension() {
        let idx = index(&["folder/Deep Note.md", "Deep Note.md"]);
        assert_eq!(
            resolve_target("folder/Deep Note", Path::new("Home.md"), &idx),
            Some(PathBuf::from("folder/Deep Note.md"))
        );
        assert_eq!(
            resolve_target("folder/Deep Note.md", Path::new("Home.md"), &idx),
            Some(PathBuf::from("folder/Deep Note.md"))
        );
    }

    #[test]
    fn resolve_is_case_insensitive_and_handles_unicode() {
        let idx = index(&["Café Notes/Résumé.md"]);
        assert_eq!(
            resolve_target("résumé", Path::new("Home.md"), &idx),
            Some(PathBuf::from("Café Notes/Résumé.md"))
        );
        assert_eq!(
            resolve_target("café notes/résumé", Path::new("Home.md"), &idx),
            Some(PathBuf::from("Café Notes/Résumé.md"))
        );
    }

    #[test]
    fn resolve_unresolved_returns_none() {
        let idx = index(&["A.md", "B.md"]);
        assert_eq!(resolve_target("Missing", Path::new("A.md"), &idx), None);
        assert_eq!(resolve_target("", Path::new("A.md"), &idx), None);
    }
}
