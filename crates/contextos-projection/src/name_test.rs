use super::*;

#[test]
fn accepts_short_lowercase_alphanumeric_extensions() {
    for value in ["md", "toml", "mdx2"] {
        assert!(FileExtension::try_from(value).is_ok(), "{value}");
    }
}

#[test]
fn rejects_empty_dotted_uppercase_separated_and_overlong_extensions() {
    for value in [
        "",
        ".md",
        "MD",
        "m/d",
        "m d",
        "tar.gz",
        "m\0d",
        "caf\u{e9}",
        &"x".repeat(17),
    ] {
        assert!(
            matches!(
                FileExtension::try_from(value),
                Err(ProjectionError::InvalidExtension { .. })
            ),
            "{value:?}"
        );
    }
}
