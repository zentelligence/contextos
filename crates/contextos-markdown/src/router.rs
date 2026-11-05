//! The port through which every vault URL is produced.

/// How a vault file is shown, by its extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileKind {
    /// A Markdown note, a page.
    Note,
    /// A `.base` view, a page.
    Base,
    /// A `.canvas` board, a page.
    Canvas,
    /// A `.mermaid` diagram, a page.
    Diagram,
    Image,
    Audio,
    Video,
    Pdf,
    /// Any other file.
    Other,
}

impl From<&str> for FileKind {
    fn from(path: &str) -> Self {
        let name = path.rsplit('/').next().unwrap_or(path);
        let Some((stem, extension)) = name.rsplit_once('.') else {
            return Self::Other;
        };
        if stem.is_empty() {
            return Self::Other;
        }
        match extension.to_ascii_lowercase().as_str() {
            "md" => Self::Note,
            "base" => Self::Base,
            "canvas" => Self::Canvas,
            "mermaid" => Self::Diagram,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "avif" | "ico" => Self::Image,
            "mp3" | "ogg" | "wav" | "m4a" | "flac" | "aac" | "opus" => Self::Audio,
            "mp4" | "webm" | "mov" | "ogv" | "mkv" => Self::Video,
            "pdf" => Self::Pdf,
            _ => Self::Other,
        }
    }
}

impl FileKind {
    /// Whether a file of this kind is shown as a page on the screen that owns it, rather than served as a file.
    #[must_use]
    pub const fn is_page(self) -> bool {
        matches!(self, Self::Note | Self::Base | Self::Canvas | Self::Diagram)
    }
}

/// Produces the URLs of vault files. The renderer never formats a vault URL itself.
pub trait LinkRouter {
    /// The URL of the page that shows the page file at `path` (vault-relative), with `anchor` (an element id on that
    /// page) appended when given, or `None` when no screen shows it.
    fn page(&self, path: &str, anchor: Option<&str>) -> Option<String>;

    /// The URL that serves the bytes of the file at `path` (vault-relative).
    fn file(&self, path: &str) -> String;
}

#[cfg(test)]
#[path = "router_test.rs"]
mod tests;
