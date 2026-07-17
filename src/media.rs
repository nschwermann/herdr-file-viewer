//! Media rendering — capability-gated inline preview of images and video.
//!
//! A sibling to the external content renderers (glow/delta/bat): an **image renderer** that
//! activates only when the terminal supports an inline-graphics protocol AND a capable CLI
//! backend is on `PATH`. Detection is cheap and cached ([`detect`]). When both are present, the
//! content pane advertises an inline preview the user can paint over a suspended terminal (the
//! same suspend/resume hand-off the editor uses, so no graphics escape ever reaches the ratatui
//! frame buffer — the constitution's "never emit escape garbage to an incapable terminal").
//! When either is missing, it falls back to a clean textual placeholder: file type, dimensions
//! (cheaply parsed from the header when possible), and size. It never crashes and never emits raw
//! bytes.
//!
//! Everything here is **read-only** (constitution §1): it classifies by extension, parses image
//! headers (a bounded read), probes the environment/`PATH`, and builds argv — it never writes a
//! file. The env/`PATH` probes are injected so the whole surface is unit-testable on any host.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// A terminal inline-graphics protocol, detected from the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsProtocol {
    /// The Kitty graphics protocol (kitty, Ghostty, WezTerm, Konsole, …).
    Kitty,
    /// The iTerm2 inline-image protocol.
    Iterm2,
    /// DEC sixel graphics.
    Sixel,
}

impl GraphicsProtocol {
    /// A short human label for the placeholder's capability line.
    pub fn label(self) -> &'static str {
        match self {
            GraphicsProtocol::Kitty => "kitty",
            GraphicsProtocol::Iterm2 => "iterm2",
            GraphicsProtocol::Sixel => "sixel",
        }
    }
}

/// An inline-image CLI backend, in the detection priority order the task specifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageBackend {
    /// `kitten icat` — kitty's own image displayer.
    KittenIcat,
    /// `chafa` — auto-detects the terminal's protocol (kitty/sixel/iterm2) and degrades to
    /// Unicode symbols. The recommended default.
    Chafa,
    /// `timg` — terminal image/video viewer.
    Timg,
    /// `viu` — a simple terminal image viewer.
    Viu,
}

impl ImageBackend {
    /// The program name to probe on `PATH` (the launcher binary).
    pub fn program(self) -> &'static str {
        match self {
            ImageBackend::KittenIcat => "kitten",
            ImageBackend::Chafa => "chafa",
            ImageBackend::Timg => "timg",
            ImageBackend::Viu => "viu",
        }
    }

    /// A short human label for the placeholder's capability line.
    pub fn label(self) -> &'static str {
        match self {
            ImageBackend::KittenIcat => "kitten icat",
            ImageBackend::Chafa => "chafa",
            ImageBackend::Timg => "timg",
            ImageBackend::Viu => "viu",
        }
    }

    /// The detection priority list (first available wins).
    pub const PRIORITY: [ImageBackend; 4] = [
        ImageBackend::KittenIcat,
        ImageBackend::Chafa,
        ImageBackend::Timg,
        ImageBackend::Viu,
    ];
}

/// A tool for extracting a representative poster frame from a video, in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoTool {
    /// `ffmpegthumbnailer` — a single-purpose thumbnailer.
    FfmpegThumbnailer,
    /// `ffmpeg` — extract a frame at ~10% of the duration.
    Ffmpeg,
}

impl VideoTool {
    pub fn program(self) -> &'static str {
        match self {
            VideoTool::FfmpegThumbnailer => "ffmpegthumbnailer",
            VideoTool::Ffmpeg => "ffmpeg",
        }
    }
}

/// Which media class a file is, by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

impl MediaKind {
    pub fn label(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
        }
    }
}

const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "tif", "avif", "heic", "heif", "ico",
    "ppm", "pgm", "pbm",
];
const VIDEO_EXTS: &[&str] = &[
    "mp4", "mov", "mkv", "webm", "avi", "m4v", "wmv", "flv", "mpg", "mpeg", "ogv", "3gp",
];

/// Classify a path as image/video by its (case-insensitive) extension, or `None` for anything
/// else. Pure and cheap — no file read.
pub fn classify(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if IMAGE_EXTS.contains(&ext.as_str()) {
        Some(MediaKind::Image)
    } else if VIDEO_EXTS.contains(&ext.as_str()) {
        Some(MediaKind::Video)
    } else {
        None
    }
}

/// Detect the terminal's inline-graphics protocol from environment variables, via an **injected**
/// getter so it is testable without touching process env. Returns the best-guess protocol, or
/// `None` when nothing indicates inline-graphics support.
///
/// Heuristics (env only — no terminal round-trip): Kitty for kitty/Ghostty/WezTerm/Konsole,
/// iTerm2 for iTerm, sixel for a `TERM` that advertises it. Conservative: an unknown terminal
/// yields `None`, so the placeholder path is used rather than risking escape garbage.
pub fn detect_protocol(get: impl Fn(&str) -> Option<String>) -> Option<GraphicsProtocol> {
    let val = |k: &str| get(k).filter(|s| !s.is_empty());
    let term = val("TERM").unwrap_or_default().to_ascii_lowercase();
    let term_program = val("TERM_PROGRAM").unwrap_or_default().to_ascii_lowercase();

    // Kitty graphics protocol: kitty itself, Ghostty, WezTerm, Konsole.
    if term.contains("kitty")
        || val("KITTY_WINDOW_ID").is_some()
        || term_program == "ghostty"
        || val("GHOSTTY_RESOURCES_DIR").is_some()
        || term_program == "wezterm"
        || val("WEZTERM_PANE").is_some()
        || val("KONSOLE_VERSION").is_some()
    {
        return Some(GraphicsProtocol::Kitty);
    }
    // iTerm2 inline images.
    if term_program == "iterm.app" || val("ITERM_SESSION_ID").is_some() {
        return Some(GraphicsProtocol::Iterm2);
    }
    // Sixel-capable terminals that advertise it via TERM (e.g. `foot`, `xterm-sixel`, `mlterm`).
    if term.contains("sixel") || term.contains("foot") || term.contains("mlterm") {
        return Some(GraphicsProtocol::Sixel);
    }
    None
}

/// Select the first available image backend from [`ImageBackend::PRIORITY`], via an **injected**
/// availability predicate (`have(program)` — true when the program is on `PATH`).
pub fn detect_image_backend(have: impl Fn(&str) -> bool) -> Option<ImageBackend> {
    ImageBackend::PRIORITY
        .into_iter()
        .find(|b| have(b.program()))
}

/// Select the video poster tool (ffmpegthumbnailer preferred, else ffmpeg), via `have`.
pub fn detect_video_tool(have: impl Fn(&str) -> bool) -> Option<VideoTool> {
    if have("ffmpegthumbnailer") {
        Some(VideoTool::FfmpegThumbnailer)
    } else if have("ffmpeg") {
        Some(VideoTool::Ffmpeg)
    } else {
        None
    }
}

/// The resolved media capability for this session: the terminal protocol, the image backend, and
/// the video poster tool (each `None` when unavailable). `Copy` — three small enums.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MediaCapability {
    pub protocol: Option<GraphicsProtocol>,
    pub image_backend: Option<ImageBackend>,
    pub video_tool: Option<VideoTool>,
}

impl MediaCapability {
    /// Resolve from injected probes (pure — used directly by tests).
    pub fn resolve(get_env: impl Fn(&str) -> Option<String>, have: impl Fn(&str) -> bool) -> Self {
        MediaCapability {
            protocol: detect_protocol(get_env),
            image_backend: detect_image_backend(&have),
            video_tool: detect_video_tool(&have),
        }
    }

    /// Whether an inline **image** can be painted: a protocol AND an image backend are present.
    pub fn can_show_image(&self) -> bool {
        self.protocol.is_some() && self.image_backend.is_some()
    }

    /// Whether an inline **video** poster can be shown: image display is possible AND a poster
    /// tool is present to extract a frame.
    pub fn can_show_video(&self) -> bool {
        self.can_show_image() && self.video_tool.is_some()
    }

    /// Whether the given media kind can be previewed inline under this capability.
    pub fn can_show(&self, kind: MediaKind) -> bool {
        match kind {
            MediaKind::Image => self.can_show_image(),
            MediaKind::Video => self.can_show_video(),
        }
    }
}

/// The process-lifetime media capability, detected once from the real environment + `PATH`.
/// Cheap and cached (constitution: keep detection cheap and cached).
pub fn detect() -> MediaCapability {
    static CAP: OnceLock<MediaCapability> = OnceLock::new();
    *CAP.get_or_init(|| MediaCapability::resolve(|k| std::env::var(k).ok(), on_path))
}

/// Whether `prog` is an executable on `PATH` — a cheap directory scan (no process spawn). Windows
/// also tries the `PATHEXT` extensions.
fn on_path(prog: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".to_string())
            .split(';')
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&path).any(|dir| {
        exts.iter().any(|ext| {
            let candidate = dir.join(format!("{prog}{ext}"));
            candidate.is_file()
        })
    })
}

/// Cheaply probe an image's pixel dimensions by parsing only its header (a bounded read of the
/// first bytes — never the whole file). Supports PNG, GIF, BMP, JPEG, and WEBP; returns `None`
/// for an unsupported/short/corrupt header (the placeholder then simply omits dimensions).
pub fn image_dimensions(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    // 64 KiB is plenty for a header (JPEG's SOF can sit past the thumbnail); bounded so a huge
    // file is never slurped.
    file.take(64 * 1024).read_to_end(&mut buf).ok()?;
    dimensions_from_bytes(&buf)
}

/// Parse pixel dimensions from an in-memory image header. Split out from [`image_dimensions`] so
/// the format parsers are unit-testable without touching the filesystem.
pub fn dimensions_from_bytes(b: &[u8]) -> Option<(u32, u32)> {
    // PNG: 8-byte signature, then IHDR (width, height as big-endian u32 at offsets 16/20).
    if b.len() >= 24 && b.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        let w = be_u32(&b[16..20])?;
        let h = be_u32(&b[20..24])?;
        return Some((w, h));
    }
    // GIF: "GIF87a"/"GIF89a", then logical-screen width/height as little-endian u16 at offset 6.
    if b.len() >= 10 && (b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")) {
        let w = le_u16(&b[6..8])? as u32;
        let h = le_u16(&b[8..10])? as u32;
        return Some((w, h));
    }
    // BMP: "BM", width/height as little-endian i32 at offsets 18/22 (height may be negative =
    // top-down; report its magnitude).
    if b.len() >= 26 && b.starts_with(b"BM") {
        let w = le_u32(&b[18..22])?;
        let h = le_u32(&b[22..26])?;
        return Some((w, (h as i32).unsigned_abs()));
    }
    // WEBP: "RIFF"...."WEBP" then a VP8/VP8L/VP8X chunk.
    if b.len() >= 30
        && b.starts_with(b"RIFF")
        && &b[8..12] == b"WEBP"
        && let Some(dim) = webp_dimensions(b)
    {
        return Some(dim);
    }
    // JPEG: scan the segment chain for a Start-Of-Frame marker (SOF0..SOFF, excluding the
    // non-frame markers), whose payload carries height then width (big-endian u16).
    if b.len() >= 4 && b[0] == 0xff && b[1] == 0xd8 {
        return jpeg_dimensions(b);
    }
    None
}

fn be_u32(b: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(..4)?.try_into().ok()?))
}
fn le_u32(b: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(..4)?.try_into().ok()?))
}
fn le_u16(b: &[u8]) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(..2)?.try_into().ok()?))
}
fn be_u16(b: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(..2)?.try_into().ok()?))
}

fn webp_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let fourcc = &b[12..16];
    match fourcc {
        b"VP8 " => {
            // Lossy: after a 10-byte frame tag the 14-bit width/height sit at offsets 26/28.
            let w = (le_u16(&b[26..28])? & 0x3fff) as u32;
            let h = (le_u16(&b[28..30])? & 0x3fff) as u32;
            Some((w, h))
        }
        b"VP8L" => {
            // Lossless: 1 signature byte then 14+14 bits of (width-1, height-1) at offset 21.
            let bits = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
            let w = (bits & 0x3fff) + 1;
            let h = ((bits >> 14) & 0x3fff) + 1;
            Some((w, h))
        }
        b"VP8X" => {
            // Extended: 24-bit (width-1, height-1) little-endian at offsets 24/27.
            let w = u32::from_le_bytes([b[24], b[25], b[26], 0]) + 1;
            let h = u32::from_le_bytes([b[27], b[28], b[29], 0]) + 1;
            Some((w, h))
        }
        _ => None,
    }
}

fn jpeg_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2; // past SOI (FF D8)
    while i + 9 < b.len() {
        if b[i] != 0xff {
            i += 1;
            continue;
        }
        // Skip fill bytes.
        let mut marker = b[i + 1];
        let mut j = i + 1;
        while marker == 0xff && j + 1 < b.len() {
            j += 1;
            marker = b[j];
        }
        // Markers without a length payload.
        if (0xd0..=0xd9).contains(&marker) || marker == 0x01 {
            i = j + 1;
            continue;
        }
        let len = be_u16(&b[j + 1..j + 3])? as usize;
        // SOF0..SOFF except DHT(C4), DAC(CC), and RSTn — the frame headers carry the dimensions.
        if (0xc0..=0xcf).contains(&marker) && marker != 0xc4 && marker != 0xc8 && marker != 0xcc {
            let h = be_u16(&b[j + 4..j + 6])? as u32;
            let w = be_u16(&b[j + 6..j + 8])? as u32;
            return Some((w, h));
        }
        i = j + 1 + len;
    }
    None
}

/// Human file-size label (`820 B`, `1.4 MB`).
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}

/// Build the content-pane **placeholder** for a media file: an icon + the file type, its
/// dimensions (when cheaply parsed), its on-disk size, and a capability line telling the user
/// whether an inline preview is available (and how) or what is missing. This is what the pane
/// always shows for media; it is plain text, so it can never emit escape garbage. Pure over its
/// inputs.
pub fn placeholder(
    kind: MediaKind,
    file_name: &str,
    size: Option<u64>,
    dimensions: Option<(u32, u32)>,
    cap: &MediaCapability,
) -> String {
    let icon = match kind {
        MediaKind::Image => "🖼",
        MediaKind::Video => "🎬",
    };
    let mut lines = vec![format!("{icon}  {file_name}"), String::new()];
    lines.push(format!("Type:  {}", kind.label()));
    if let Some((w, h)) = dimensions {
        lines.push(format!("Size:  {w} × {h} px"));
    }
    if let Some(bytes) = size {
        lines.push(format!("File:  {}", human_size(bytes)));
    }
    lines.push(String::new());

    if cap.can_show(kind) {
        let backend = cap.image_backend.map(|b| b.label()).unwrap_or("?");
        let protocol = cap.protocol.map(|p| p.label()).unwrap_or("?");
        lines.push(format!(
            "Inline preview available ({protocol} via {backend}) — press Enter to view.",
        ));
    } else if cap.protocol.is_none() {
        lines.push(
            "No inline-graphics protocol detected in this terminal — showing file info only."
                .to_string(),
        );
    } else if kind == MediaKind::Video && cap.video_tool.is_none() {
        lines.push(
            "No video poster tool (install ffmpeg or ffmpegthumbnailer) — showing file info only."
                .to_string(),
        );
    } else {
        lines.push(
            "No inline image backend (install chafa, kitten, timg, or viu) — showing file info \
             only."
                .to_string(),
        );
    }
    lines.join("\n")
}

/// Build the argv that paints `image` inline via `backend`, sized to `cols` × `rows` character
/// cells. `protocol` steers chafa's format; the other backends auto-detect. The image path is
/// always the final, single argv element (never shell-split), so spaces/metacharacters stay
/// literal.
pub fn image_argv(
    backend: ImageBackend,
    protocol: GraphicsProtocol,
    image: &Path,
    cols: u16,
    rows: u16,
) -> Vec<String> {
    let path = image.to_string_lossy().into_owned();
    match backend {
        ImageBackend::KittenIcat => vec![
            "kitten".into(),
            "icat".into(),
            "--clear".into(),
            "--place".into(),
            format!("{cols}x{rows}@0x0"),
            path,
        ],
        ImageBackend::Chafa => {
            let format = match protocol {
                GraphicsProtocol::Kitty => "kitty",
                GraphicsProtocol::Iterm2 => "iterm",
                GraphicsProtocol::Sixel => "sixels",
            };
            vec![
                "chafa".into(),
                format!("--format={format}"),
                format!("--size={cols}x{rows}"),
                path,
            ]
        }
        ImageBackend::Timg => vec!["timg".into(), format!("-g{cols}x{rows}"), path],
        ImageBackend::Viu => vec![
            "viu".into(),
            "-w".into(),
            cols.to_string(),
            "-h".into(),
            rows.to_string(),
            path,
        ],
    }
}

/// Build the argv that extracts a poster frame from `video` into `out` (a temp image). For
/// ffmpeg the frame is taken at ~10% of the duration (`-ss`), so it is representative rather than
/// a black lead-in frame; ffmpegthumbnailer picks a representative frame itself. Paths are single
/// argv elements.
pub fn video_poster_argv(
    tool: VideoTool,
    video: &Path,
    out: &Path,
    duration_secs: Option<f64>,
) -> Vec<String> {
    let vid = video.to_string_lossy().into_owned();
    let out = out.to_string_lossy().into_owned();
    match tool {
        VideoTool::FfmpegThumbnailer => vec![
            "ffmpegthumbnailer".into(),
            "-i".into(),
            vid,
            "-o".into(),
            out,
            "-s".into(),
            "0".into(), // 0 = original size
            "-t".into(),
            "10%".into(),
        ],
        VideoTool::Ffmpeg => {
            // Seek to ~10% of the duration when known, else a small fixed offset past the lead-in.
            let seek = duration_secs.map(|d| (d * 0.10).max(0.0)).unwrap_or(1.0);
            vec![
                "ffmpeg".into(),
                "-y".into(),
                "-ss".into(),
                format!("{seek:.2}"),
                "-i".into(),
                vid,
                "-frames:v".into(),
                "1".into(),
                "-q:v".into(),
                "3".into(),
                out,
            ]
        }
    }
}

/// The read-only media-preview seam: paint a media file over a suspended terminal. Behind a trait
/// so the controller stays unit-testable (the live implementation, in `app.rs`, suspends the TUI,
/// runs the backend, waits for a keypress, and resumes). Never mutates the previewed file.
pub trait MediaViewer {
    /// Preview `path` (an image or video). Returns the outcome; the live viewer takes over the
    /// terminal and returns [`MediaOutcome::TookOver`].
    fn view(&mut self, path: &Path, kind: MediaKind) -> MediaOutcome;
}

/// The result of a media-preview attempt (mirrors the editor hand-off's outcomes).
#[derive(Debug, PartialEq, Eq)]
pub enum MediaOutcome {
    /// The preview ran and drew over the terminal; the run loop forces a full repaint.
    TookOver,
    /// The preview could not run (backend missing / capability absent). Carries a user-facing
    /// reason for the notice.
    Failed(String),
}

/// A scratch path for a video poster frame, under the system temp dir, unique to `video`'s name
/// and this process. Not created here — the poster tool writes it; the caller cleans it up.
pub fn poster_scratch(video: &Path, salt: u64) -> PathBuf {
    let stem = video
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "poster".to_string());
    let safe: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    std::env::temp_dir().join(format!(
        "hfv-poster-{}-{}-{}.png",
        std::process::id(),
        salt,
        safe
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_by_extension() {
        assert_eq!(classify(Path::new("a/b.PNG")), Some(MediaKind::Image));
        assert_eq!(classify(Path::new("clip.mp4")), Some(MediaKind::Video));
        assert_eq!(classify(Path::new("x.rs")), None);
        assert_eq!(classify(Path::new("noext")), None);
    }

    #[test]
    fn protocol_detects_ghostty_as_kitty() {
        let env = |k: &str| match k {
            "TERM_PROGRAM" => Some("ghostty".to_string()),
            _ => None,
        };
        assert_eq!(detect_protocol(env), Some(GraphicsProtocol::Kitty));
    }

    #[test]
    fn protocol_detects_kitty_and_iterm_and_sixel() {
        assert_eq!(
            detect_protocol(|k| (k == "TERM").then(|| "xterm-kitty".to_string())),
            Some(GraphicsProtocol::Kitty)
        );
        assert_eq!(
            detect_protocol(|k| (k == "TERM_PROGRAM").then(|| "iTerm.app".to_string())),
            Some(GraphicsProtocol::Iterm2)
        );
        assert_eq!(
            detect_protocol(|k| (k == "TERM").then(|| "foot".to_string())),
            Some(GraphicsProtocol::Sixel)
        );
    }

    #[test]
    fn protocol_none_for_a_plain_terminal() {
        let env = |k: &str| match k {
            "TERM" => Some("xterm-256color".to_string()),
            _ => None,
        };
        assert_eq!(detect_protocol(env), None);
    }

    #[test]
    fn image_backend_priority_prefers_kitten_then_chafa() {
        // Only chafa + viu present → chafa (higher priority than viu).
        let have = |p: &str| matches!(p, "chafa" | "viu");
        assert_eq!(detect_image_backend(have), Some(ImageBackend::Chafa));
        // kitten present → wins over everything.
        let have_all = |_: &str| true;
        assert_eq!(
            detect_image_backend(have_all),
            Some(ImageBackend::KittenIcat)
        );
        // none present → None.
        assert_eq!(detect_image_backend(|_| false), None);
    }

    #[test]
    fn video_tool_prefers_thumbnailer_then_ffmpeg() {
        assert_eq!(
            detect_video_tool(|p| p == "ffmpeg"),
            Some(VideoTool::Ffmpeg)
        );
        assert_eq!(
            detect_video_tool(|p| matches!(p, "ffmpeg" | "ffmpegthumbnailer")),
            Some(VideoTool::FfmpegThumbnailer)
        );
        assert_eq!(detect_video_tool(|_| false), None);
    }

    #[test]
    fn capability_gating_requires_protocol_and_backend() {
        // Ghostty (kitty) but NO backend installed — exactly this machine's state: cannot show.
        let cap = MediaCapability::resolve(
            |k| (k == "TERM_PROGRAM").then(|| "ghostty".to_string()),
            |_| false,
        );
        assert_eq!(cap.protocol, Some(GraphicsProtocol::Kitty));
        assert_eq!(cap.image_backend, None);
        assert!(!cap.can_show_image(), "no backend → no inline image");
        assert!(!cap.can_show_video());

        // Protocol + chafa but no ffmpeg: image yes, video no.
        let cap = MediaCapability::resolve(
            |k| (k == "TERM_PROGRAM").then(|| "ghostty".to_string()),
            |p| p == "chafa",
        );
        assert!(cap.can_show_image());
        assert!(!cap.can_show_video(), "no poster tool → no video");

        // Backend present but plain terminal (no protocol): cannot show.
        let cap = MediaCapability::resolve(|_| None, |_| true);
        assert!(!cap.can_show_image(), "no protocol → no inline image");
    }

    #[test]
    fn placeholder_degrades_cleanly_with_no_backend() {
        // This machine's real situation: Ghostty protocol, no image backend. The placeholder must
        // name the type/size and say info-only — never crash, never emit escapes.
        let cap = MediaCapability {
            protocol: Some(GraphicsProtocol::Kitty),
            image_backend: None,
            video_tool: Some(VideoTool::Ffmpeg),
        };
        let text = placeholder(
            MediaKind::Image,
            "photo.png",
            Some(2048),
            Some((800, 600)),
            &cap,
        );
        assert!(text.contains("photo.png"));
        assert!(text.contains("image"));
        assert!(text.contains("800 × 600"));
        assert!(text.contains("2.0 KB"));
        assert!(
            text.to_lowercase().contains("no inline image backend"),
            "degraded notice names the missing backend: {text}"
        );
        // Never any escape byte.
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn placeholder_advertises_preview_when_capable() {
        let cap = MediaCapability {
            protocol: Some(GraphicsProtocol::Kitty),
            image_backend: Some(ImageBackend::Chafa),
            video_tool: Some(VideoTool::Ffmpeg),
        };
        let text = placeholder(MediaKind::Image, "a.png", Some(10), None, &cap);
        assert!(text.contains("Inline preview available"));
        assert!(text.contains("kitty via chafa"));
    }

    #[test]
    fn png_dimensions_from_header() {
        // 1×1 PNG signature + IHDR with width=1 height=1.
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        b.extend_from_slice(&[0, 0, 0, 13]); // IHDR length
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&300u32.to_be_bytes());
        b.extend_from_slice(&200u32.to_be_bytes());
        assert_eq!(dimensions_from_bytes(&b), Some((300, 200)));
    }

    #[test]
    fn gif_and_bmp_dimensions() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&640u16.to_le_bytes());
        gif.extend_from_slice(&480u16.to_le_bytes());
        assert_eq!(dimensions_from_bytes(&gif), Some((640, 480)));

        let mut bmp = b"BM".to_vec();
        bmp.resize(18, 0);
        bmp.extend_from_slice(&1024i32.to_le_bytes());
        bmp.extend_from_slice(&(-768i32).to_le_bytes()); // top-down (negative) height
        assert_eq!(dimensions_from_bytes(&bmp), Some((1024, 768)));
    }

    #[test]
    fn jpeg_dimensions_from_sof0() {
        // SOI, then a minimal SOF0 (FF C0) segment: len, precision, height, width.
        let mut b = vec![0xff, 0xd8];
        b.extend_from_slice(&[0xff, 0xc0]); // SOF0
        b.extend_from_slice(&[0x00, 0x11]); // length 17
        b.push(8); // precision
        b.extend_from_slice(&720u16.to_be_bytes()); // height
        b.extend_from_slice(&1280u16.to_be_bytes()); // width
        b.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]); // components
        assert_eq!(dimensions_from_bytes(&b), Some((1280, 720)));
    }

    #[test]
    fn dimensions_none_for_unknown_or_short() {
        assert_eq!(dimensions_from_bytes(b"not an image"), None);
        assert_eq!(dimensions_from_bytes(&[0x89, b'P']), None);
    }

    #[test]
    fn human_size_scales() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(2 * 1024 * 1024), "2.0 MB");
    }

    #[test]
    fn image_argv_chafa_sets_format_and_size_and_path_last() {
        let argv = image_argv(
            ImageBackend::Chafa,
            GraphicsProtocol::Kitty,
            Path::new("/a b/pic.png"),
            80,
            24,
        );
        assert_eq!(argv[0], "chafa");
        assert!(argv.iter().any(|a| a == "--format=kitty"));
        assert!(argv.iter().any(|a| a == "--size=80x24"));
        assert_eq!(
            argv.last().unwrap(),
            "/a b/pic.png",
            "path is one literal arg"
        );
    }

    #[test]
    fn image_argv_kitten_and_viu_shapes() {
        let k = image_argv(
            ImageBackend::KittenIcat,
            GraphicsProtocol::Kitty,
            Path::new("p.png"),
            10,
            5,
        );
        assert_eq!(&k[0..2], &["kitten".to_string(), "icat".to_string()]);
        assert_eq!(k.last().unwrap(), "p.png");

        let v = image_argv(
            ImageBackend::Viu,
            GraphicsProtocol::Sixel,
            Path::new("p.png"),
            10,
            5,
        );
        assert_eq!(v[0], "viu");
        assert_eq!(v.last().unwrap(), "p.png");
    }

    #[test]
    fn video_poster_argv_seeks_ten_percent_for_ffmpeg() {
        let argv = video_poster_argv(
            VideoTool::Ffmpeg,
            Path::new("/v/clip.mp4"),
            Path::new("/tmp/out.png"),
            Some(100.0),
        );
        let ss = argv.iter().position(|a| a == "-ss").unwrap();
        assert_eq!(argv[ss + 1], "10.00", "10% of 100s");
        assert!(argv.iter().any(|a| a == "/v/clip.mp4"));
        assert_eq!(argv.last().unwrap(), "/tmp/out.png");
    }

    #[test]
    fn video_poster_argv_thumbnailer_shape() {
        let argv = video_poster_argv(
            VideoTool::FfmpegThumbnailer,
            Path::new("clip.mkv"),
            Path::new("out.png"),
            None,
        );
        assert_eq!(argv[0], "ffmpegthumbnailer");
        assert!(argv.windows(2).any(|w| w == ["-i", "clip.mkv"]));
        assert!(argv.windows(2).any(|w| w == ["-o", "out.png"]));
    }
}
