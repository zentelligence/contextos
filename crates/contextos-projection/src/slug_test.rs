use super::*;

fn rejection(value: &str) -> Option<NameRejection> {
    match Slug::try_from(value) {
        Err(ProjectionError::InvalidSlug { reason }) => Some(reason),
        _ => None,
    }
}

#[test]
fn accepts_lowercase_words_joined_by_single_hyphens() {
    for value in ["a", "0", "code-reviewer", "a1-b2-c3", "x".repeat(64).as_str()] {
        assert!(Slug::try_from(value).is_ok(), "{value} should be accepted");
    }
}

#[test]
fn rejects_each_malformed_slug_with_its_reason() {
    assert_eq!(rejection(""), Some(NameRejection::Empty));
    assert_eq!(rejection(&"x".repeat(65)), Some(NameRejection::TooLong { maximum: 64 }));
    for value in [
        "Upper",
        "under_score",
        "dot.name",
        "sp ace",
        "slash/name",
        "caf\u{e9}",
        "a\0b",
        "../x",
        "a\\b",
    ] {
        assert_eq!(rejection(value), Some(NameRejection::InvalidCharacter), "{value:?}");
    }
    for value in ["-a", "a-", "a--b", "-"] {
        assert_eq!(rejection(value), Some(NameRejection::MisplacedHyphen), "{value:?}");
    }
}

#[test]
fn exposes_the_validated_text() -> Result<(), ProjectionError> {
    let slug = Slug::try_from("code-reviewer")?;
    assert_eq!(<&str>::from(&slug), "code-reviewer");
    Ok(())
}

#[test]
fn error_messages_never_echo_the_rejected_text() {
    let message = match Slug::try_from("SECRET Value") {
        Err(error) => error.to_string(),
        Ok(_) => String::new(),
    };
    assert!(!message.contains("SECRET"), "{message}");
}

#[test]
fn windows_device_names_are_not_slugs() {
    for name in ["con", "prn", "aux", "nul", "com1", "com9", "lpt1", "lpt9"] {
        assert!(
            matches!(
                Slug::try_from(name),
                Err(ProjectionError::InvalidSlug {
                    reason: NameRejection::ReservedDeviceName
                })
            ),
            "{name} was accepted as a slug"
        );
    }
}

#[test]
fn names_that_only_begin_with_a_device_name_are_still_slugs() {
    for name in [
        "console",
        "auxiliary",
        "nulls",
        "com10",
        "com",
        "lpt0",
        "con-sole",
        "my-con",
    ] {
        assert!(Slug::try_from(name).is_ok(), "{name} was refused as a slug");
    }
}

#[test]
fn a_slug_and_a_skill_file_path_agree_on_every_device_name_decision() {
    for name in [
        "con",
        "prn",
        "aux",
        "nul",
        "com1",
        "com9",
        "lpt1",
        "lpt9",
        "console",
        "auxiliary",
        "com",
        "com10",
        "lpt0",
    ] {
        let as_slug = rejection(name) == Some(NameRejection::ReservedDeviceName);
        let as_path = matches!(
            crate::SkillFilePath::try_from(name),
            Err(ProjectionError::InvalidSkillPath {
                reason: NameRejection::ReservedDeviceName
            })
        );
        assert_eq!(as_slug, as_path, "{name}: the two rules disagree");
    }
}
