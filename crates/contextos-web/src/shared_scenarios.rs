//! Dry-run scenarios a package ships.
//!
//! A package may carry `{workflow-slug}/tests/{name}.toml`, in the format of a vault workflow's scenarios, covered by the
//! package's content digest like every other file. A dry run of a shared workflow reads them from the installed release
//! it resolved, so the scenario that proved a job is the one that shipped with the version it runs.

use contextos_workflows::PackageName;
use std::io;
use std::path::Path;

use semver::Version;
use thiserror::Error;

use crate::bounded_read::{BoundedReadError, read_text_within};
use crate::dry_run_scenario::{Scenario, ScenarioError};
use crate::package_store::package_version_dir;

/// A scenario is a short list of calls; anything larger is not one.
const MAX_SCENARIO_BYTES: u64 = 256 * 1024;

const TESTS_DIR_NAME: &str = "tests";

#[derive(Debug, Error)]
pub enum SharedScenarioError {
    #[error("package {package:?} {version} ships no scenario for workflow {workflow:?}, so it cannot be dry-run")]
    NoScenarios {
        package: String,
        version: Version,
        workflow: String,
    },
    #[error(
        "package {package:?} {version} ships {count} scenarios for workflow {workflow:?}; name one explicitly with --scenario"
    )]
    Ambiguous {
        package: String,
        version: Version,
        workflow: String,
        count: usize,
    },
    #[error("package {package:?} {version} ships no scenario named {scenario:?} for workflow {workflow:?}")]
    NotFound {
        package: String,
        version: Version,
        workflow: String,
        scenario: String,
    },
    #[error("scenario {scenario:?} could not be read: {source}")]
    Unreadable {
        scenario: String,
        #[source]
        source: BoundedReadError,
    },
    #[error("scenario {scenario:?} is invalid: {source}")]
    Invalid {
        scenario: String,
        #[source]
        source: Box<ScenarioError>,
    },
    #[error("the package's scenarios could not be listed: {0}")]
    Io(#[from] io::Error),
}

/// Loads the scenario `requested` names, or the only one the package ships for `workflow` when none is named, from the
/// installed release `version` of `package`.
///
/// # Errors
///
/// Returns [`SharedScenarioError::NoScenarios`] when the package ships none for the workflow,
/// [`SharedScenarioError::Ambiguous`] when it ships several and none is named, [`SharedScenarioError::NotFound`] when
/// the named one is not shipped or the name is not a plain file name, and [`SharedScenarioError::Invalid`] when the file
/// does not parse as a scenario.
pub fn load_shared_scenario(
    store_root: &Path,
    package: &PackageName,
    version: &Version,
    workflow: &str,
    requested: Option<&str>,
) -> Result<(String, Scenario), SharedScenarioError> {
    let tests_dir = package_version_dir(store_root, package, version)
        .join(workflow)
        .join(TESTS_DIR_NAME);
    let not_found = |scenario: &str| SharedScenarioError::NotFound {
        package: package.to_string(),
        version: version.clone(),
        workflow: workflow.to_owned(),
        scenario: scenario.to_owned(),
    };
    let name = match requested {
        Some(name) if is_plain_name(name) => name.to_owned(),
        Some(name) => return Err(not_found(name)),
        None => sole_scenario(&tests_dir, package.as_str(), version, workflow)?,
    };
    let text = match read_text_within(&tests_dir.join(format!("{name}.toml")), MAX_SCENARIO_BYTES) {
        Ok(text) => text,
        Err(BoundedReadError::Io(error)) if error.kind() == io::ErrorKind::NotFound => return Err(not_found(&name)),
        Err(source) => return Err(SharedScenarioError::Unreadable { scenario: name, source }),
    };
    match Scenario::parse(&text) {
        Ok(scenario) => Ok((name, scenario)),
        Err(source) => Err(SharedScenarioError::Invalid {
            scenario: name,
            source: Box::new(source),
        }),
    }
}

/// The name of the one scenario in `tests_dir`.
fn sole_scenario(
    tests_dir: &Path,
    package: &str,
    version: &Version,
    workflow: &str,
) -> Result<String, SharedScenarioError> {
    let mut names = Vec::new();
    match std::fs::read_dir(tests_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if let (true, Ok(file_name)) = (entry.file_type()?.is_file(), entry.file_name().into_string())
                    && let Some(name) = file_name.strip_suffix(".toml")
                {
                    names.push(name.to_owned());
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    names.sort();
    match names.len() {
        0 => Err(SharedScenarioError::NoScenarios {
            package: package.to_owned(),
            version: version.clone(),
            workflow: workflow.to_owned(),
        }),
        1 => Ok(names.remove(0)),
        count => Err(SharedScenarioError::Ambiguous {
            package: package.to_owned(),
            version: version.clone(),
            workflow: workflow.to_owned(),
            count,
        }),
    }
}

/// A name that is one ordinary file name: not empty, not `.` or `..`, and holding no separator. A colon is refused too,
/// because on Windows it makes the rest of the name an alternate data stream.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0', ':'])
}

#[cfg(test)]
#[path = "shared_scenarios_test.rs"]
mod tests;
