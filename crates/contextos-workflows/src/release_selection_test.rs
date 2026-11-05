use super::*;

fn versions(list: &[&str]) -> Vec<Version> {
    list.iter().filter_map(|text| Version::parse(text).ok()).collect()
}

fn named(text: &str) -> Version {
    Version::parse(text).unwrap_or_else(|_| Version::new(0, 0, 0))
}

#[test]
fn the_named_release_is_chosen_even_when_a_newer_one_is_installed() {
    let installed = versions(&["1.2.0", "1.10.0"]);

    assert_eq!(select_release(&installed, &named("1.2.0")).ok(), Some(named("1.2.0")));
}

#[test]
fn the_newest_release_is_never_chosen_in_place_of_the_named_one() {
    let installed = versions(&["1.2.0", "1.10.0", "1.9.3"]);

    let error = select_release(&installed, &named("1.5.0")).err();

    assert!(
        matches!(&error, Some(PackageError::ReleaseUnavailable { requested }) if requested == "1.5.0"),
        "{error:?}"
    );
}

#[test]
fn a_pre_release_runs_when_it_is_the_one_named() {
    let installed = versions(&["1.2.0", "2.0.0-beta.1"]);

    assert_eq!(
        select_release(&installed, &named("2.0.0-beta.1")).ok(),
        Some(named("2.0.0-beta.1"))
    );
}

#[test]
fn nothing_installed_fails_naming_the_release_asked_for() {
    let error = select_release(&[], &named("1.0.0")).err();

    assert!(
        matches!(&error, Some(PackageError::ReleaseUnavailable { requested }) if requested == "1.0.0"),
        "{error:?}"
    );
}

fn state_with(current: Option<&str>, previous: Option<&str>, future: Option<&str>) -> PackageReleases {
    let mut releases = PackageReleases::default();
    releases.workflows.insert(
        "sync".to_owned(),
        crate::release_state::WorkflowVersions {
            current: current.map(|text| Version::parse(text).unwrap_or(Version::new(0, 0, 0))),
            previous: previous.map(|text| Version::parse(text).unwrap_or(Version::new(0, 0, 0))),
            future: future.map(|text| Version::parse(text).unwrap_or(Version::new(0, 0, 0))),
            ..Default::default()
        },
    );
    releases
}

fn chosen(choice: &str, releases: Option<&PackageReleases>) -> Result<Version, ReleaseChoiceError> {
    let choice = ReleaseChoice::try_from(choice)?;
    Version::try_from(OneOffRelease {
        choice: &choice,
        workflow: "sync",
        releases,
    })
}

#[test]
fn the_words_current_future_and_previous_and_an_exact_version_are_choices() {
    assert_eq!(ReleaseChoice::try_from("current"), Ok(ReleaseChoice::Current));
    assert_eq!(ReleaseChoice::try_from("future"), Ok(ReleaseChoice::Future));
    assert_eq!(ReleaseChoice::try_from("previous"), Ok(ReleaseChoice::Previous));
    assert_eq!(
        ReleaseChoice::try_from("1.2.3"),
        Ok(ReleaseChoice::Version(Version::new(1, 2, 3)))
    );
}

#[test]
fn another_word_and_a_pre_release_or_build_version_are_not_choices() {
    for text in ["newest", "", "1.2", "1.2.3-rc.1", "1.2.3+build"] {
        assert_eq!(
            ReleaseChoice::try_from(text),
            Err(ReleaseChoiceError::NotAChoice(text.to_owned())),
            "{text:?}"
        );
    }
}

#[test]
fn each_word_takes_the_version_the_state_records_under_it() {
    let state = state_with(Some("2.0.0"), Some("1.0.0"), Some("3.0.0"));
    assert_eq!(chosen("current", Some(&state)), Ok(Version::new(2, 0, 0)));
    assert_eq!(chosen("previous", Some(&state)), Ok(Version::new(1, 0, 0)));
    assert_eq!(chosen("future", Some(&state)), Ok(Version::new(3, 0, 0)));
}

#[test]
fn a_word_the_state_records_nothing_under_is_refused_as_not_runnable() {
    let state = state_with(Some("2.0.0"), None, None);
    for (word, standing) in [("previous", "previous"), ("future", "future")] {
        let error = chosen(word, Some(&state)).err();
        assert_eq!(error, Some(ReleaseChoiceError::NoSuchStanding(standing)), "{word}");
    }
    assert_eq!(
        chosen("current", None),
        Err(ReleaseChoiceError::NoSuchStanding("current"))
    );
}

#[test]
fn an_exact_version_is_run_when_it_is_current_previous_or_a_future_release() {
    let state = state_with(Some("2.0.0"), Some("1.0.0"), Some("4.0.0"));
    for text in ["2.0.0", "1.0.0", "4.0.0"] {
        assert_eq!(
            chosen(text, Some(&state)),
            Ok(Version::parse(text).unwrap_or(Version::new(0, 0, 0))),
            "{text}"
        );
    }
}

#[test]
fn an_exact_version_older_than_current_that_is_not_previous_is_refused_as_retired() {
    let state = state_with(Some("2.0.0"), Some("1.1.0"), None);
    let error = chosen("1.0.0", Some(&state)).err();
    assert_eq!(error, Some(ReleaseChoiceError::Retired(Version::new(1, 0, 0))));
}

#[test]
fn a_workflow_with_no_current_runs_any_exact_version_as_a_future_release() {
    assert_eq!(chosen("1.0.0", None), Ok(Version::new(1, 0, 0)));
}
