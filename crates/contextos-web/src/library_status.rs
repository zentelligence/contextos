//! What a library release checks as: whether it reads whole, whether its code compiles, whether the libraries it requires
//! can be read, and whether the tools its code calls are the tools it declares.
//!
//! These are the checks installing a library makes over its source, made again over the installed release, so a release
//! that was damaged or whose required libraries have since gone shows it before a workflow release is locked to it.

use std::collections::BTreeMap;

use contextos_packages::PackageStore;
use contextos_workflows::ReleaseRef;
use contextos_workflows::call_rules::CallRules;

use crate::job_validation::check_syntax;
use crate::library_checks::library_tool_failures;
use crate::package_release_manifest::PackageRelease;
use crate::package_standing::{ReleaseStanding, StoreStanding};
use crate::vault_release_snapshot::read_library_code;
use crate::workflow_releases::ReleaseCondition;

/// What one check came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Check {
    Passed,
    /// The check found these problems, one line each.
    Failed(Vec<String>),
    /// The check could not be made, for the reason given. It is never taken for a pass.
    NotChecked(String),
}

/// A library release's four checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LibraryStatus {
    /// The release is whole: every file it lists is there, unchanged, and nothing else is.
    pub(crate) manifest: Check,
    /// Every module compiles.
    pub(crate) syntax: Check,
    /// The libraries the release requires are installed and read whole.
    pub(crate) imports: Check,
    /// The release's code calls only tools its `library.toml` declares.
    pub(crate) declared_tools: Check,
}

impl LibraryStatus {
    fn unchecked(manifest: Check, reason: &str) -> Self {
        let not_checked = || Check::NotChecked(reason.to_owned());
        Self {
            manifest,
            syntax: not_checked(),
            imports: not_checked(),
            declared_tools: not_checked(),
        }
    }
}

impl From<&ReleaseCondition> for Check {
    fn from(condition: &ReleaseCondition) -> Self {
        match condition {
            ReleaseCondition::Whole => Self::Passed,
            ReleaseCondition::Removed => Self::Failed(vec!["the release has no folder in the store".to_owned()]),
            ReleaseCondition::Incomplete(findings) | ReleaseCondition::Damaged(findings) => {
                Self::Failed(findings.clone())
            }
            ReleaseCondition::Unverifiable(reason) => Self::NotChecked(format!("it could not be verified: {reason}")),
        }
    }
}

/// Checks the library release `held`, read from `store`.
pub(crate) async fn check_library_release<S>(store: &S, held: &ReleaseStanding) -> LibraryStatus
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let manifest = Check::from(&held.condition);
    if manifest != Check::Passed {
        return LibraryStatus::unchecked(manifest, "the release does not read whole");
    }
    let code = match read_library_code(store, &held.release.name, &held.release.version).await {
        Ok(code) => code,
        Err(failure) => {
            return LibraryStatus {
                manifest,
                syntax: Check::NotChecked("the libraries the release requires could not be read".to_owned()),
                imports: Check::Failed(vec![failure.to_string()]),
                declared_tools: Check::NotChecked("the libraries the release requires could not be read".to_owned()),
            };
        }
    };
    let found = |failures: Vec<contextos_workflows::ValidationFailure>| {
        if failures.is_empty() {
            Check::Passed
        } else {
            Check::Failed(
                failures
                    .into_iter()
                    .map(|failure| format!("{}: {}", failure.artefact, failure.message))
                    .collect(),
            )
        }
    };
    LibraryStatus {
        manifest,
        syntax: found(check_syntax("", &code.script).await),
        imports: Check::Passed,
        declared_tools: found(
            library_tool_failures(&code.script, "", &code.library_tools, &CallRules::default()).await,
        ),
    }
}

/// The checks of every library release in `standing`, in the order the store lists them.
pub(crate) async fn check_libraries<S>(store: &S, standing: &StoreStanding) -> BTreeMap<ReleaseRef, LibraryStatus>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let mut checked = BTreeMap::new();
    for held in standing.releases.iter().filter(|held| held.is_library()) {
        checked.insert(held.release.clone(), check_library_release(store, held).await);
    }
    checked
}

/// Every library release of `standing` as not checked, for a store that could not be opened to check them.
pub(crate) fn unchecked_libraries(standing: &StoreStanding, reason: &str) -> BTreeMap<ReleaseRef, LibraryStatus> {
    standing
        .releases
        .iter()
        .filter(|held| held.is_library())
        .map(|held| {
            (
                held.release.clone(),
                LibraryStatus::unchecked(Check::NotChecked(reason.to_owned()), reason),
            )
        })
        .collect()
}

#[cfg(test)]
#[path = "library_status_test.rs"]
mod tests;
