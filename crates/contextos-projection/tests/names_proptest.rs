//! Property tests: accepted names are exactly the documented grammar, and no input panics.

use contextos_projection::{FileExtension, SkillFilePath, Slug};
use proptest::prelude::*;

fn is_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

proptest! {
    #[test]
    fn a_slug_is_accepted_exactly_when_it_matches_the_grammar(value in "\\PC{0,80}") {
        prop_assert_eq!(Slug::try_from(value.as_str()).is_ok(), is_slug(&value));
    }

    #[test]
    fn every_grammatical_slug_is_accepted(value in "[a-z0-9]{1,10}(-[a-z0-9]{1,10}){0,4}") {
        prop_assert!(Slug::try_from(value.as_str()).is_ok());
    }

    #[test]
    fn an_accepted_skill_path_has_only_safe_components(value in "[ -~]{0,40}") {
        if SkillFilePath::try_from(value.as_str()).is_ok() {
            prop_assert!(!value.starts_with('/'));
            prop_assert!(!value.contains('\\') && !value.contains(':'));
            for component in value.split('/') {
                prop_assert!(!component.is_empty() && component != "." && component != "..");
                prop_assert!(!component.ends_with('.') && !component.ends_with(' '));
            }
        }
    }

    #[test]
    fn a_skill_path_with_a_traversal_component_is_never_accepted(
        before in "[a-z]{1,5}(/[a-z]{1,5}){0,2}",
        after in "[a-z]{1,5}(/[a-z]{1,5}){0,2}",
    ) {
        let value = format!("{before}/../{after}");
        prop_assert!(SkillFilePath::try_from(value.as_str()).is_err());
    }

    #[test]
    fn arbitrary_text_never_panics_any_validator(value in "\\PC{0,100}") {
        let _ = SkillFilePath::try_from(value.as_str());
        let _ = FileExtension::try_from(value.as_str());
    }
}
