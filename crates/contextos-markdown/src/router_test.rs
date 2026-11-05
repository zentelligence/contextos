use super::*;

#[test]
fn a_file_kind_is_read_from_its_extension_without_regard_to_case() {
    let cases = [
        ("a/b.md", FileKind::Note),
        ("B.MD", FileKind::Note),
        ("tasks.base", FileKind::Base),
        ("board.canvas", FileKind::Canvas),
        ("flow.mermaid", FileKind::Diagram),
        ("p.PNG", FileKind::Image),
        ("p.jpeg", FileKind::Image),
        ("p.svg", FileKind::Image),
        ("p.webp", FileKind::Image),
        ("s.mp3", FileKind::Audio),
        ("s.flac", FileKind::Audio),
        ("v.mp4", FileKind::Video),
        ("v.webm", FileKind::Video),
        ("d.pdf", FileKind::Pdf),
        ("x.txt", FileKind::Other),
        ("Makefile", FileKind::Other),
        ("folder.md/file", FileKind::Other),
    ];
    for (path, kind) in cases {
        assert_eq!(FileKind::from(path), kind, "{path}");
    }
}

#[test]
fn notes_bases_canvases_and_diagrams_are_pages_and_nothing_else_is() {
    for kind in [FileKind::Note, FileKind::Base, FileKind::Canvas, FileKind::Diagram] {
        assert!(kind.is_page(), "{kind:?}");
    }
    for kind in [
        FileKind::Image,
        FileKind::Audio,
        FileKind::Video,
        FileKind::Pdf,
        FileKind::Other,
    ] {
        assert!(!kind.is_page(), "{kind:?}");
    }
}
