use super::*;

fn rejection(value: &str) -> Option<NameRejection> {
    match SkillFilePath::try_from(value) {
        Err(ProjectionError::InvalidSkillPath { reason }) => Some(reason),
        _ => None,
    }
}

#[test]
fn accepts_plain_nested_relative_paths() {
    for value in [
        "SKILL.md",
        "scripts/run.sh",
        "a/b/c.txt",
        ".hidden",
        "notes v2/final.md",
        "caf\u{e9}.md",
        "console.d/x",
    ] {
        assert!(SkillFilePath::try_from(value).is_ok(), "{value}");
    }
}

#[test]
fn rejects_each_unsafe_path_with_its_reason() {
    let cases = [
        ("", NameRejection::Empty),
        ("/etc/passwd", NameRejection::Absolute),
        ("a//b", NameRejection::EmptyComponent),
        ("a/", NameRejection::EmptyComponent),
        (".", NameRejection::DotComponent),
        ("..", NameRejection::DotComponent),
        ("a/./b", NameRejection::DotComponent),
        ("a/../b", NameRejection::DotComponent),
        ("a\\b", NameRejection::Backslash),
        ("C:/x", NameRejection::Colon),
        ("file.txt:stream", NameRejection::Colon),
        ("a\0b", NameRejection::ControlCharacter),
        ("a\nb", NameRejection::ControlCharacter),
        ("a\u{7f}b", NameRejection::ControlCharacter),
        ("CON", NameRejection::ReservedDeviceName),
        ("con.txt", NameRejection::ReservedDeviceName),
        ("dir/Nul.tar.gz", NameRejection::ReservedDeviceName),
        ("aux", NameRejection::ReservedDeviceName),
        ("PRN.md", NameRejection::ReservedDeviceName),
        ("com1", NameRejection::ReservedDeviceName),
        ("COM9.x", NameRejection::ReservedDeviceName),
        ("lpt1", NameRejection::ReservedDeviceName),
        ("LPT9.log", NameRejection::ReservedDeviceName),
        ("COM\u{b9}", NameRejection::ReservedDeviceName),
        ("name.", NameRejection::TrailingDotOrSpace),
        ("name ", NameRejection::TrailingDotOrSpace),
        ("a./b", NameRejection::TrailingDotOrSpace),
        ("...", NameRejection::TrailingDotOrSpace),
    ];
    for (value, expected) in cases {
        assert_eq!(rejection(value), Some(expected), "{value:?}");
    }
}

#[test]
fn accepts_names_that_only_resemble_reserved_devices() {
    for value in ["com0", "com10", "lpt0", "console", "auxiliary", "nully", "com", "prn_x"] {
        assert!(SkillFilePath::try_from(value).is_ok(), "{value}");
    }
}

#[test]
fn rejects_overlong_components_and_paths() {
    assert_eq!(
        rejection(&"x".repeat(256)),
        Some(NameRejection::TooLong { maximum: 255 })
    );
    assert!(SkillFilePath::try_from("x".repeat(255).as_str()).is_ok());
    let long = vec!["x".repeat(200); 6].join("/");
    assert_eq!(rejection(&long), Some(NameRejection::TooLong { maximum: 1024 }));
}

#[test]
fn exposes_the_validated_text() -> Result<(), ProjectionError> {
    let path = SkillFilePath::try_from("scripts/run.sh")?;
    assert_eq!(<&str>::from(&path), "scripts/run.sh");
    Ok(())
}
