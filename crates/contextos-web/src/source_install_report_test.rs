use super::*;

use std::collections::BTreeMap;

use contextos_workflows::ValidationCode;

use crate::vault_source_install::{ReleaseReport, RequirementReport};

fn release(package: &str, version: &str, outcome: SourceOutcome) -> ReleaseReport {
    ReleaseReport {
        package: package.to_owned(),
        version: version.to_owned(),
        digest: format!("sha256:{package}"),
        outcome,
    }
}

fn installed() -> SourceInstalled {
    SourceInstalled {
        release: release("ops", "1.2.0", SourceOutcome::Installed),
        kind: "workflows",
        standing: BTreeMap::from([
            ("sync".to_owned(), "future".to_owned()),
            ("update".to_owned(), "current".to_owned()),
        ]),
        requirements: vec![RequirementReport {
            library: "crm".to_owned(),
            requirement: "^1.0.0".to_owned(),
        }],
        libraries: vec![release("crm", "1.0.0", SourceOutcome::AlreadyInstalled)],
        warnings: Vec::new(),
        removed_by_retention: false,
    }
}

#[test]
fn the_text_names_the_release_its_standing_its_requirements_and_the_libraries_installed_on_the_way() {
    let text = InstalledText(&installed()).to_string();

    assert_eq!(
        text,
        "installed ops 1.2.0 (workflows)\n\
         digest sha256:ops\n\
         sync: future\n\
         update: current\n\
         requires crm ^1.0.0\n\
         library crm 1.0.0 already installed"
    );
}

#[test]
fn a_library_report_has_no_standing_and_no_requirements_lines_when_it_has_none() {
    let report = SourceInstalled {
        release: release("crm", "1.0.0", SourceOutcome::Repaired),
        kind: "library",
        standing: BTreeMap::new(),
        requirements: Vec::new(),
        libraries: Vec::new(),
        warnings: Vec::new(),
        removed_by_retention: false,
    };

    assert_eq!(
        InstalledText(&report).to_string(),
        "restored crm 1.0.0 (library)\ndigest sha256:crm"
    );
}

#[test]
fn the_json_document_carries_the_release_the_standing_and_the_libraries() -> Result<(), serde_json::Error> {
    let document: serde_json::Value = serde_json::from_str(&InstalledJson(&installed()).to_string())?;

    assert_eq!(document["release"]["version"], "1.2.0");
    assert_eq!(document["release"]["outcome"], "installed");
    assert_eq!(document["kind"], "workflows");
    assert_eq!(document["standing"]["sync"], "future");
    assert_eq!(document["requirements"][0]["requirement"], "^1.0.0");
    assert_eq!(document["libraries"][0]["outcome"], "already-installed");
    Ok(())
}

fn refusal() -> SourceInstallError {
    SourceInstallError::refusing(
        ValidationCode::ScriptSyntax,
        "sync/sync.rhai",
        "line 1: unexpected token",
    )
}

#[test]
fn a_refusal_in_text_gives_its_code_and_message() {
    assert_eq!(
        RefusalText(&refusal()).to_string(),
        "refused validation/script-syntax sync/sync.rhai: line 1: unexpected token"
    );
}

#[test]
fn a_refusal_in_json_is_the_error_document_with_the_code_and_message() -> Result<(), serde_json::Error> {
    let document: serde_json::Value = serde_json::from_str(&RefusalJson(&refusal()).to_string())?;

    assert_eq!(document["error"]["code"], "validation/script-syntax");
    assert!(
        document["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("sync/sync.rhai"))
    );
    Ok(())
}

#[test]
fn a_warning_after_the_install_is_a_line_of_text_and_an_entry_of_the_document() -> Result<(), serde_json::Error> {
    let mut report = installed();
    report.warnings = vec![crate::store_retention::RetentionWarning::raised(
        contextos_workflows::StoreKind::Vault,
        &crate::package_retention::RetentionError::from(std::io::Error::other("the jobs folder could not be listed")),
    )];

    let text = InstalledText(&report).to_string();
    let document: serde_json::Value = serde_json::from_str(&InstalledJson(&report).to_string())?;

    assert!(
        text.ends_with(
            "\nwarning packages/retention-failed: the shared jobs could not be listed: the jobs folder could not be listed"
        ),
        "{text}"
    );
    assert_eq!(document["warnings"][0]["code"], "packages/retention-failed");
    assert!(installed_document_has_no_warnings_key()?);
    Ok(())
}

fn installed_document_has_no_warnings_key() -> Result<bool, serde_json::Error> {
    let document: serde_json::Value = serde_json::from_str(&InstalledJson(&installed()).to_string())?;
    Ok(document.get("warnings").is_none())
}

#[test]
fn an_install_that_retention_removed_at_once_says_so_in_the_text_and_the_document() -> Result<(), serde_json::Error> {
    let mut report = installed();
    report.removed_by_retention = true;

    let text = InstalledText(&report).to_string();
    let document: serde_json::Value = serde_json::from_str(&InstalledJson(&report).to_string())?;

    assert!(
        text.starts_with("installed ops 1.2.0 (workflows), then removed again by retention because nothing keeps it\n"),
        "{text}"
    );
    assert_eq!(document["removed_by_retention"], true);
    assert!(
        serde_json::from_str::<serde_json::Value>(&InstalledJson(&installed()).to_string())?
            .get("removed_by_retention")
            .is_none()
    );
    Ok(())
}
