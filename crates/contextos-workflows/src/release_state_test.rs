use super::*;

use ReleaseAllowance::{
    Allowed, AllowedMovingCurrent, AlreadyFixed, NotReachable, Refused, ReturnSuccessorMovingCurrent,
    ReturnSuccessorOnly,
};

const USES: [ReleaseUse; 7] = [
    ReleaseUse::NewJob,
    ReleaseUse::VersionChangedInPlace,
    ReleaseUse::Successor,
    ReleaseUse::Enable,
    ReleaseUse::Resume,
    ReleaseUse::Reactivate,
    ReleaseUse::NewLock,
];

/// The table of what each release allows, row by row and column by column as the specification tabulates it.
fn table() -> [(ReleaseKind, [ReleaseAllowance; 7]); 4] {
    [
        (
            ReleaseKind::Current,
            [Allowed, Allowed, Allowed, Allowed, Allowed, Allowed, Allowed],
        ),
        (
            ReleaseKind::Future,
            [
                Allowed,
                Allowed,
                Allowed,
                AllowedMovingCurrent,
                NotReachable,
                Allowed,
                Allowed,
            ],
        ),
        (
            ReleaseKind::Previous,
            [
                Refused,
                Refused,
                ReturnSuccessorOnly,
                ReturnSuccessorMovingCurrent,
                Allowed,
                Refused,
                AlreadyFixed,
            ],
        ),
        (
            ReleaseKind::Retired,
            [Refused, Refused, Refused, Refused, Allowed, Refused, Refused],
        ),
    ]
}

#[test]
fn each_release_allows_exactly_what_the_table_says() {
    for (kind, row) in table() {
        for (usage, expected) in USES.into_iter().zip(row) {
            assert_eq!(kind.allows(usage), expected, "{kind:?} for {usage:?}");
        }
    }
}

#[test]
fn only_a_return_successor_may_use_the_ways_marked_return_successor_only() {
    for allowance in [ReturnSuccessorOnly, ReturnSuccessorMovingCurrent] {
        assert!(allowance.permits(true), "{allowance:?}");
        assert!(!allowance.permits(false), "{allowance:?}");
    }
    for allowance in [Allowed, AllowedMovingCurrent] {
        assert!(allowance.permits(true) && allowance.permits(false), "{allowance:?}");
    }
    for allowance in [Refused, NotReachable, AlreadyFixed] {
        assert!(!allowance.permits(true) && !allowance.permits(false), "{allowance:?}");
    }
}

#[test]
fn only_an_enabling_on_a_future_release_or_a_return_to_previous_moves_current() {
    for (kind, row) in table() {
        for (usage, expected) in USES.into_iter().zip(row) {
            let moves = matches!(expected, AllowedMovingCurrent | ReturnSuccessorMovingCurrent);
            assert_eq!(expected.moves_current(), moves, "{kind:?} for {usage:?}");
            if moves {
                assert_eq!(usage, ReleaseUse::Enable, "{kind:?}");
            }
        }
    }
}

#[test]
fn current_previous_and_future_are_reserved_job_slugs_and_no_other_slug_is() {
    for reserved in ["current", "previous", "future"] {
        assert!(is_reserved_job_slug(reserved), "{reserved}");
    }
    for free in ["nightly", "current-run", "futures", "previous-2", ""] {
        assert!(!is_reserved_job_slug(free), "{free}");
    }
}

mod recorded_future {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn versions(texts: &[&str]) -> Result<Vec<Version>, semver::Error> {
        texts.iter().map(|text| Version::parse(text)).collect()
    }

    fn recorded(releases: &PackageReleases, workflow: &str) -> Option<String> {
        releases
            .workflows
            .get(workflow)
            .and_then(|held| held.future.as_ref())
            .map(Version::to_string)
    }

    fn with_current(current: &str) -> Result<PackageReleases, semver::Error> {
        let mut releases = PackageReleases::default();
        releases.workflows.entry("sync".to_owned()).or_default().current = Some(Version::parse(current)?);
        Ok(releases)
    }

    #[test]
    fn with_no_current_the_newest_installed_release_is_future() -> TestResult {
        let mut releases = PackageReleases::default();

        let changed = releases.record_future("sync", &versions(&["1.0.0", "1.2.0", "1.1.0"])?);

        assert_eq!(recorded(&releases, "sync").as_deref(), Some("1.2.0"));
        assert!(changed);
        Ok(())
    }

    #[test]
    fn the_newest_installed_release_newer_than_current_is_future() -> TestResult {
        let mut releases = with_current("1.1.0")?;

        releases.record_future("sync", &versions(&["1.0.0", "1.1.0", "1.2.0", "1.3.0"])?);

        assert_eq!(recorded(&releases, "sync").as_deref(), Some("1.3.0"));
        Ok(())
    }

    #[test]
    fn no_installed_release_newer_than_current_leaves_no_future_and_clears_a_recorded_one() -> TestResult {
        let mut releases = with_current("1.3.0")?;
        releases.workflows.entry("sync".to_owned()).or_default().future = Some(Version::parse("1.3.0")?);

        let changed = releases.record_future("sync", &versions(&["1.0.0", "1.3.0"])?);

        assert_eq!(recorded(&releases, "sync"), None);
        assert!(changed);
        Ok(())
    }

    #[test]
    fn the_previous_release_is_never_future() -> TestResult {
        let mut releases = with_current("1.0.0")?;
        releases.workflows.entry("sync".to_owned()).or_default().previous = Some(Version::parse("1.2.0")?);

        releases.record_future("sync", &versions(&["1.0.0", "1.1.0", "1.2.0"])?);

        assert_eq!(recorded(&releases, "sync").as_deref(), Some("1.1.0"));
        Ok(())
    }

    #[test]
    fn a_release_with_a_retired_marker_is_never_future() -> TestResult {
        let mut releases = with_current("1.0.0")?;
        releases
            .releases
            .entry(Version::parse("1.2.0")?)
            .or_default()
            .workflows
            .entry("sync".to_owned())
            .or_default()
            .retired = Some(RetiredRelease {
            cause: RetiredReleaseCause::Superseded,
            at: time::macros::datetime!(2026-10-04 03:00:00 UTC),
        });

        releases.record_future("sync", &versions(&["1.0.0", "1.1.0", "1.2.0"])?);

        assert_eq!(recorded(&releases, "sync").as_deref(), Some("1.1.0"));
        Ok(())
    }

    #[test]
    fn recording_the_value_already_recorded_changes_nothing() -> TestResult {
        let mut releases = PackageReleases::default();
        let installed = versions(&["1.0.0", "1.2.0"])?;
        releases.record_future("sync", &installed);
        let before = releases.clone();

        let changed = releases.record_future("sync", &installed);

        assert!(!changed);
        assert_eq!(releases, before);
        Ok(())
    }

    #[test]
    fn a_workflow_with_nothing_installed_and_nothing_recorded_gains_no_entry() {
        let mut releases = PackageReleases::default();

        let changed = releases.record_future("sync", &[]);

        assert!(!changed);
        assert_eq!(releases, PackageReleases::default());
    }

    #[test]
    fn another_workflow_is_not_touched() -> TestResult {
        let mut releases = with_current("1.0.0")?;
        releases.workflows.entry("other".to_owned()).or_default().future = Some(Version::parse("9.0.0")?);

        releases.record_future("sync", &versions(&["1.0.0", "1.1.0"])?);

        assert_eq!(recorded(&releases, "other").as_deref(), Some("9.0.0"));
        Ok(())
    }
}

mod future_kind {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap_or_else(|_| Version::new(0, 0, 0))
    }

    fn with(current: Option<&str>, recorded_future: Option<&str>) -> PackageReleases {
        let mut releases = PackageReleases::default();
        let held = releases.workflows.entry("sync".to_owned()).or_default();
        held.current = current.map(v);
        held.future = recorded_future.map(v);
        releases
    }

    #[test]
    fn an_unmarked_release_newer_than_current_but_older_than_the_recorded_future_is_future() {
        let releases = with(Some("1.0.0"), Some("1.3.0"));

        assert_eq!(releases.kind_of("sync", &v("1.2.0")), ReleaseKind::Future);
        assert_eq!(releases.kind_of("sync", &v("1.3.0")), ReleaseKind::Future);
    }

    #[test]
    fn an_unmarked_release_newer_than_current_is_future_when_nothing_is_recorded() {
        let releases = with(Some("1.0.0"), None);

        assert_eq!(releases.kind_of("sync", &v("1.1.0")), ReleaseKind::Future);
    }

    #[test]
    fn with_no_current_every_unmarked_release_is_future() {
        let releases = with(None, Some("2.0.0"));

        for text in ["0.1.0", "1.0.0", "2.0.0", "3.0.0"] {
            assert_eq!(releases.kind_of("sync", &v(text)), ReleaseKind::Future, "{text}");
        }
        assert_eq!(
            PackageReleases::default().kind_of("sync", &v("1.0.0")),
            ReleaseKind::Future,
            "a workflow with nothing recorded"
        );
    }

    #[test]
    fn an_unmarked_release_older_than_current_is_retired() {
        let releases = with(Some("1.2.0"), Some("1.3.0"));

        assert_eq!(releases.kind_of("sync", &v("1.1.5")), ReleaseKind::Retired);
    }

    #[test]
    fn a_marked_release_newer_than_current_is_retired_not_future() {
        let mut releases = with(Some("1.0.0"), None);
        releases
            .releases
            .entry(v("1.1.0"))
            .or_default()
            .workflows
            .entry("sync".to_owned())
            .or_default()
            .retired = Some(RetiredRelease {
            cause: RetiredReleaseCause::Superseded,
            at: time::macros::datetime!(2026-10-04 03:00:00 UTC),
        });

        assert_eq!(releases.kind_of("sync", &v("1.1.0")), ReleaseKind::Retired);
    }

    #[test]
    fn the_recorded_future_stays_the_newest_of_several_future_releases() {
        let mut releases = with(Some("1.0.0"), None);
        let installed = [v("1.0.0"), v("1.1.0"), v("1.2.0")];

        releases.record_future("sync", &installed);

        assert_eq!(releases.kind_of("sync", &v("1.1.0")), ReleaseKind::Future);
        assert_eq!(releases.kind_of("sync", &v("1.2.0")), ReleaseKind::Future);
        assert_eq!(
            releases.workflows.get("sync").and_then(|held| held.future.clone()),
            Some(v("1.2.0"))
        );
    }
}
