//! Media rendering — capability-gated inline preview of images and video.
//!
//! A sibling to the external content renderers (glow/delta/bat): the content pane draws images
//! and video poster frames **inline in the ratatui frame** via the terminal's graphics protocol
//! (kitty/sixel/iterm2), driven by the `ratatui-image` crate (see `app::MediaPane`). This module
//! owns the read-only, testable *decisions* around that: classify a path, parse image dimensions
//! cheaply from the header, detect the terminal's graphics protocol and the video poster tool
//! from the environment/`PATH`, build the poster-extraction argv, and format the metadata
//! placeholder the pane shows above the image (or alone, when no graphics protocol is available).
//!
//! Detection is cheap and cached ([`detect`]). The actual pixels are painted by the app layer,
//! which owns the terminal and the `ratatui-image` picker; this module never emits escape bytes.
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

/// The inline-media descriptor the controller hands the app each tick: which media file is on
/// display, its class, and how many metadata rows sit above the inline image. The app's
/// `MediaPane` loads/evicts the image protocol keyed on `path`, and places the image in the
/// content pane's rows below `header_rows`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineMedia {
    pub path: PathBuf,
    pub kind: MediaKind,
    pub header_rows: u16,
}

/// A resolved inline **image embed** within a rendered markdown note (`![[img|w]]` / `![](path)`):
/// which display line its reserved band starts at, the image file, and the band size in cells. The
/// app's `MediaPane` paints the image over the band `[content_line, content_line + rows)` when that
/// band is within the content viewport (scroll-aware). Produced by the render worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaEmbed {
    pub content_line: usize,
    pub path: PathBuf,
    pub cols: u16,
    pub rows: u16,
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

/// The resolved media capability for this session: the terminal's (env-detected) graphics
/// protocol and the video poster tool (each `None` when unavailable). `Copy` — two small enums.
///
/// The env-detected `protocol` is an *advisory* hint used to enable the feature and to upgrade the
/// `ratatui-image` picker's protocol when its own terminal query comes up empty; the picker's live
/// query is authoritative for what actually paints. `video_tool` gates whether a video can show a
/// poster frame at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MediaCapability {
    pub protocol: Option<GraphicsProtocol>,
    pub video_tool: Option<VideoTool>,
}

impl MediaCapability {
    /// Resolve from injected probes (pure — used directly by tests).
    pub fn resolve(get_env: impl Fn(&str) -> Option<String>, have: impl Fn(&str) -> bool) -> Self {
        MediaCapability {
            protocol: detect_protocol(get_env),
            video_tool: detect_video_tool(&have),
        }
    }

    /// Whether an inline **image** can be painted: a graphics protocol is available. The image
    /// pixels are encoded by `ratatui-image`, so no external CLI backend is required.
    pub fn can_show_image(&self) -> bool {
        self.protocol.is_some()
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

/// Build the content-pane **metadata header** for a media file: an icon + the file type, its
/// dimensions (when cheaply parsed), and its on-disk size. When the image itself renders inline
/// below (auto-displayed by the app's `MediaPane`), this is just that metadata plus a trailing
/// blank spacer row — the returned line count is used as the header height above the image. When
/// no graphics protocol is available (`inline == false`), or a video has no poster tool, a
/// closing notice explains that only file info is shown. Plain text, so it can never emit escape
/// garbage. Pure over its inputs.
///
/// - `inline`: a terminal graphics protocol is available, so the image will paint below.
/// - `video_poster`: a poster-extraction tool (ffmpeg/ffmpegthumbnailer) is on `PATH`.
pub fn placeholder(
    kind: MediaKind,
    file_name: &str,
    size: Option<u64>,
    dimensions: Option<(u32, u32)>,
    inline: bool,
    video_poster: bool,
) -> String {
    // No emoji in the header: some terminals render image/video emoji double-width while
    // unicode-width reports single, which would misalign the pane by a cell. A plain label is safe.
    let mut lines = vec![format!("[{}]  {file_name}", kind.label()), String::new()];
    lines.push(format!("Type:  {}", kind.label()));
    if let Some((w, h)) = dimensions {
        lines.push(format!("Size:  {w} × {h} px"));
    }
    if let Some(bytes) = size {
        lines.push(format!("File:  {}", human_size(bytes)));
    }
    // A blank spacer: either the gap above the inline image, or before a notice.
    lines.push(String::new());

    // Whether the image/poster actually paints below.
    let shows_inline = inline && (kind == MediaKind::Image || video_poster);
    if !shows_inline {
        if !inline {
            lines
                .push("No inline-graphics terminal detected — showing file info only.".to_string());
        } else {
            // inline-capable terminal, but a video with no poster tool.
            lines.push(
                "No video poster tool (install ffmpeg or ffmpegthumbnailer) — showing file info \
                 only."
                    .to_string(),
            );
        }
    }
    lines.join("\n")
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
    fn capability_gating_needs_only_a_graphics_protocol() {
        // Ghostty (kitty): an image can show inline — no external backend required (ratatui-image
        // encodes the pixels). Video still needs a poster tool.
        let cap = MediaCapability::resolve(
            |k| (k == "TERM_PROGRAM").then(|| "ghostty".to_string()),
            |_| false,
        );
        assert_eq!(cap.protocol, Some(GraphicsProtocol::Kitty));
        assert!(cap.can_show_image(), "a graphics protocol suffices");
        assert!(!cap.can_show_video(), "no poster tool → no video");

        // Protocol + ffmpeg: both image and video can show.
        let cap = MediaCapability::resolve(
            |k| (k == "TERM_PROGRAM").then(|| "ghostty".to_string()),
            |p| p == "ffmpeg",
        );
        assert!(cap.can_show_image());
        assert!(cap.can_show_video(), "protocol + poster tool → video");

        // Plain terminal (no protocol): cannot show.
        let cap = MediaCapability::resolve(|_| None, |_| true);
        assert!(!cap.can_show_image(), "no protocol → no inline image");
    }

    #[test]
    fn placeholder_is_metadata_only_when_inline_capable() {
        // Inline-capable image: the placeholder is just the metadata (type/size/dimensions) — the
        // image itself paints below, so no "press Enter" / capability line, and no escape bytes.
        let text = placeholder(
            MediaKind::Image,
            "photo.png",
            Some(2048),
            Some((800, 600)),
            true,  // inline graphics available
            false, // no video poster tool (irrelevant for an image)
        );
        assert!(text.contains("photo.png"));
        assert!(text.contains("image"));
        assert!(text.contains("800 × 600"));
        assert!(text.contains("2.0 KB"));
        assert!(
            !text.to_lowercase().contains("only"),
            "no info-only notice when the image renders inline: {text}"
        );
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn placeholder_degrades_cleanly_without_a_graphics_terminal() {
        // No graphics protocol: metadata plus an info-only notice.
        let text = placeholder(
            MediaKind::Image,
            "photo.png",
            Some(2048),
            Some((800, 600)),
            false,
            false,
        );
        assert!(text.contains("photo.png"));
        assert!(
            text.to_lowercase().contains("file info only"),
            "degraded notice explains info-only: {text}"
        );
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn placeholder_video_without_poster_tool_notes_ffmpeg() {
        let text = placeholder(MediaKind::Video, "clip.mp4", Some(10), None, true, false);
        assert!(
            text.to_lowercase().contains("ffmpeg"),
            "a video with no poster tool names ffmpeg: {text}"
        );
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
