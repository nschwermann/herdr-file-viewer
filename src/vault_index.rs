//! Vault Index — a lightweight, one-shot snapshot of an Obsidian vault's notes and their links.
//!
//! The shared foundation the vault-navigation features build on: the **quick-switcher** (fuzzy-find
//! any note by name/alias) and the **backlinks** panel (which notes link *to* the current one) both
//! read this index. It enumerates every `.md` note under a vault, keyed by its vault-relative path,
//! file name, and frontmatter aliases, plus each note's *outgoing* links (wikilinks + relative
//! markdown links) resolved to the notes they point at — so backlinks is a cheap reverse lookup.
//!
//! **Read-only** (constitution §1): [`VaultIndex::build`] only reads the filesystem — a bounded,
//! dot-directory-skipping walk (via [`crate::obsidian::markdown_index`]) plus a bounded read of each
//! note — and never writes. It never panics: an unreadable note contributes an entry with no
//! aliases/links rather than aborting the build.
//!
//! **Scope + freshness.** The index is a *full scan* of exactly one vault, taken once and cached by
//! the caller (the Session Controller), then rebuilt on demand — on a re-root, or on the `r` refresh
//! — rather than incrementally maintained. A vault edited *outside* the viewer between refreshes can
//! therefore be momentarily stale; that is the deliberate YAGNI trade (no filesystem watcher), and
//! `r` is the escape hatch. The walk and per-note read are both bounded ([`MAX_NOTES`],
//! [`MAX_NOTE_BYTES`]) so a pathological vault can never stall the input thread or blow memory.

use crate::mdnote::{Frontmatter, PropValue, split_frontmatter};
use crate::obsidian::{is_markdown, markdown_index, resolve_target};
use crate::wikilink::parse_links;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The largest number of notes the index enumerates. A generous bound so a pathological vault can't
/// make the build unbounded; a real vault is far smaller. Shared with the walk in [`markdown_index`].
pub const MAX_NOTES: usize = 20_000;

/// The largest slice of a single note the index reads for alias/link extraction. A note far exceeds
/// any real frontmatter/link density well within this; the cap only guards a pathological file.
pub const MAX_NOTE_BYTES: u64 = 1024 * 1024;

/// One indexed note: its identity (vault-relative path, file-stem name, frontmatter aliases) and the
/// notes it links *out* to (resolved to their own vault-relative paths, deduped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteEntry {
    /// The note's path relative to the vault root (e.g. `folder/Note.md`). The index's key.
    pub rel: PathBuf,
    /// The note's file stem (`Note` for `folder/Note.md`) — its bare name, how a `[[Note]]` link
    /// and the quick-switcher address it.
    pub name: String,
    /// Alias names declared in the note's frontmatter (`alias:` / `aliases:`), in declaration order.
    /// Empty when the note has no frontmatter or no alias key. Also searchable in the quick-switcher.
    pub aliases: Vec<String>,
    /// The vault-relative paths this note links *out* to — every wikilink / relative markdown link
    /// that resolved to a note in the vault, deduped and in ascending path order. Unresolved links
    /// are dropped (they point at no note). The backlinks lookup scans these.
    pub links: Vec<PathBuf>,
}

impl NoteEntry {
    /// A human, single-line label for the note: its vault-relative path with the `.md`/`.markdown`
    /// extension dropped and separators normalised to `/` (how Obsidian names a note). Used as the
    /// quick-switcher / backlinks row text.
    pub fn display(&self) -> String {
        let s = self.rel.to_string_lossy().replace('\\', "/");
        strip_md_ext(&s).to_string()
    }
}

/// A built snapshot of one vault: its root plus every note under it. Cheap to query (a linear scan);
/// built once by [`build`](Self::build) and cached by the caller, rebuilt on re-root / refresh.
#[derive(Debug, Clone)]
pub struct VaultIndex {
    root: PathBuf,
    notes: Vec<NoteEntry>,
}

impl VaultIndex {
    /// Build the index for `vault_root`: enumerate every markdown note (bounded, dot-directory
    /// skipping), then for each read it (bounded) to pull its frontmatter aliases and resolve its
    /// outgoing links to other notes. Read-only and panic-free — an unreadable note yields an entry
    /// with no aliases/links rather than aborting.
    pub fn build(vault_root: &Path) -> VaultIndex {
        // The vault's note set, as vault-relative paths — the same list link resolution matches
        // against, so a link resolves to exactly one of these entries (or none).
        let rels = markdown_index(vault_root, MAX_NOTES);
        let notes = rels
            .iter()
            .map(|rel| {
                let abs = vault_root.join(rel);
                let source = read_note_bounded(&abs).unwrap_or_default();
                let (fm, _body) = split_frontmatter(&source);
                let aliases = fm.as_ref().map(frontmatter_aliases).unwrap_or_default();
                let name = rel
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let links = resolve_outgoing(&source, rel, &rels);
                NoteEntry {
                    rel: rel.clone(),
                    name,
                    aliases,
                    links,
                }
            })
            .collect();
        VaultIndex {
            root: vault_root.to_path_buf(),
            notes,
        }
    }

    /// The vault root this index was built for. The caller compares it to a freshly-detected vault to
    /// decide whether the cached index still applies.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every indexed note, in walk order. Exposed for the quick-switcher's candidate list and tests.
    pub fn notes(&self) -> &[NoteEntry] {
        &self.notes
    }

    /// How many notes the index holds.
    pub fn note_count(&self) -> usize {
        self.notes.len()
    }

    /// The notes that link *to* `target_rel` (a vault-relative note path) — a reverse lookup over the
    /// stored outgoing links. Returned in walk order (stable per build). Empty when nothing links to
    /// it. A note never counts as its own backlink (a self-link is skipped) so the panel lists only
    /// *other* notes.
    pub fn backlinks(&self, target_rel: &Path) -> Vec<&NoteEntry> {
        self.notes
            .iter()
            .filter(|n| n.rel != target_rel && n.links.iter().any(|l| l == target_rel))
            .collect()
    }
}

/// Pull the alias names out of a note's frontmatter: the values of an `alias`/`aliases` key
/// (case-insensitive), whether written as a scalar or a list. Trimmed; empties dropped. Order is the
/// frontmatter's declaration order.
fn frontmatter_aliases(fm: &Frontmatter) -> Vec<String> {
    let mut out = Vec::new();
    for (key, val) in &fm.entries {
        let k = key.to_ascii_lowercase();
        if k != "alias" && k != "aliases" {
            continue;
        }
        match val {
            PropValue::Scalar(s) => {
                let s = s.trim();
                if !s.is_empty() {
                    out.push(s.to_string());
                }
            }
            PropValue::List(items) => {
                for it in items {
                    let it = it.trim();
                    if !it.is_empty() {
                        out.push(it.to_string());
                    }
                }
            }
        }
    }
    out
}

/// Resolve a note's outgoing links to the vault-relative notes they point at: parse every
/// wikilink/embed/relative-markdown link, resolve each against the vault (shortest-unique-path, the
/// Obsidian way), drop the unresolved ones, then dedupe and sort so the result is deterministic and a
/// backlinks scan is cheap. `rels` is the vault's full note set (link resolution's candidate list).
fn resolve_outgoing(source: &str, source_rel: &Path, rels: &[PathBuf]) -> Vec<PathBuf> {
    let mut links: Vec<PathBuf> = parse_links(source)
        .into_iter()
        .filter_map(|link| resolve_target(&link.target, source_rel, rels))
        .collect();
    links.sort();
    links.dedup();
    links
}

/// Strip a trailing `.md` / `.markdown` extension (case-insensitive), leaving any other suffix
/// untouched (local copy of the same helper `obsidian` keeps private, so the display path drops the
/// extension without a cross-module dependency).
fn strip_md_ext(s: &str) -> &str {
    for ext in [".md", ".markdown"] {
        if s.len() >= ext.len() && s[s.len() - ext.len()..].eq_ignore_ascii_case(ext) {
            return &s[..s.len() - ext.len()];
        }
    }
    s
}

/// Read up to [`MAX_NOTE_BYTES`] of `path`, lossily decoded as UTF-8. `None` on any I/O error (never
/// panics), so a note that can't be read just contributes no aliases/links. Mirrors the bounded
/// reader the link navigator / outline use.
fn read_note_bounded(path: &Path) -> Option<String> {
    if !is_markdown(path) {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_NOTE_BYTES).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// A fresh, unique temp vault dir (no tempfile dep — matches the project's hermetic style).
    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hfv-vaultindex-{}-{}-{tag}",
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
    fn build_indexes_every_note_with_name_and_skips_dotdirs() {
        let root = tmp("basic");
        write(&root, "A.md", "# A");
        write(&root, "sub/B.md", "# B");
        write(&root, ".obsidian/ignored.md", "# nope");
        write(&root, "image.png", "x");

        let idx = VaultIndex::build(&root);
        assert_eq!(
            idx.note_count(),
            2,
            "only the two real notes, no dotdir/png"
        );
        let mut names: Vec<&str> = idx.notes().iter().map(|n| n.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["A", "B"]);
    }

    #[test]
    fn aliases_are_parsed_from_frontmatter_scalar_and_list() {
        let root = tmp("aliases");
        write(&root, "Scalar.md", "---\nalias: The One\n---\nbody");
        write(
            &root,
            "List.md",
            "---\naliases:\n  - First\n  - Second\n---\nbody",
        );
        write(&root, "Inline.md", "---\naliases: [X, Y]\n---\nbody");
        let idx = VaultIndex::build(&root);
        let by_name = |name: &str| {
            idx.notes()
                .iter()
                .find(|n| n.name == name)
                .unwrap()
                .aliases
                .clone()
        };
        assert_eq!(by_name("Scalar"), vec!["The One".to_string()]);
        assert_eq!(
            by_name("List"),
            vec!["First".to_string(), "Second".to_string()]
        );
        assert_eq!(by_name("Inline"), vec!["X".to_string(), "Y".to_string()]);
    }

    #[test]
    fn outgoing_links_are_resolved_deduped_and_sorted() {
        let root = tmp("links");
        // Home links to Target twice (wikilink + embed) and to a missing note.
        write(
            &root,
            "Home.md",
            "see [[Target]] and ![[Target]] and [[Missing]] and [rel](sub/Deep.md)",
        );
        write(&root, "Target.md", "# T");
        write(&root, "sub/Deep.md", "# D");
        let idx = VaultIndex::build(&root);
        let home = idx.notes().iter().find(|n| n.name == "Home").unwrap();
        assert_eq!(
            home.links,
            vec![PathBuf::from("Target.md"), PathBuf::from("sub/Deep.md")],
            "resolved, deduped (Target once), sorted; Missing dropped"
        );
    }

    #[test]
    fn backlinks_finds_notes_linking_to_a_target_and_excludes_self() {
        let root = tmp("backlinks");
        write(
            &root,
            "Target.md",
            "I link to [[Target]] myself (a self-link)",
        );
        write(&root, "A.md", "points at [[Target]]");
        write(&root, "B.md", "also [[Target|aliased]]");
        write(&root, "C.md", "no link here");
        let idx = VaultIndex::build(&root);
        let mut names: Vec<&str> = idx
            .backlinks(Path::new("Target.md"))
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["A", "B"],
            "A and B link to Target; Target's self-link and C are excluded"
        );
    }

    #[test]
    fn display_drops_the_md_extension_and_slashes_the_path() {
        let entry = NoteEntry {
            rel: PathBuf::from("folder/My Note.md"),
            name: "My Note".to_string(),
            aliases: vec![],
            links: vec![],
        };
        assert_eq!(entry.display(), "folder/My Note");
    }

    #[test]
    fn unreadable_or_empty_note_yields_entry_with_no_aliases_or_links() {
        let root = tmp("empty");
        write(&root, "Empty.md", "");
        let idx = VaultIndex::build(&root);
        let e = idx.notes().iter().find(|n| n.name == "Empty").unwrap();
        assert!(e.aliases.is_empty());
        assert!(e.links.is_empty());
    }
}
