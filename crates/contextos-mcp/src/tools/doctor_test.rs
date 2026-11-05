use contextos_core::{VaultRoot, VaultRootInput};

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn one_vault() -> Result<(tempfile::TempDir, VaultSet), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: directory.path().to_path_buf(),
        managed: false,
        name: Some("alpha".to_owned()),
    })?])?;
    Ok((directory, roots))
}

/// A failed check a tool performs the fix for, naming the vault at `vault_index`.
fn tool_backed_check(vault_index: usize) -> DoctorCheck {
    DoctorCheck {
        subject: "Managed indexes".to_owned(),
        status: DoctorStatus::Fail,
        message: "stale".to_owned(),
        action: Some("rebuild".to_owned()),
        auto_fixable: true,
        remediation: Some(Remediation::RebuildIndexes),
        vault_index: Some(vault_index),
        vault_name: Some("alpha".to_owned()),
        kind: DoctorKind::ManagedIndexes,
    }
}

#[test]
fn a_tool_backed_finding_for_a_configured_vault_is_planned_with_that_vault() -> TestResult {
    let (_directory, roots) = one_vault()?;
    let report = DoctorReport {
        checks: vec![tool_backed_check(0)],
    };

    let planned = remedies(&report, &roots, None);

    assert_eq!(planned.len(), 1);
    assert!(planned[0].root().is_some());
    Ok(())
}

#[test]
fn a_tool_backed_finding_that_names_no_configured_vault_is_never_planned() -> TestResult {
    let (_directory, roots) = one_vault()?;
    let report = DoctorReport {
        checks: vec![tool_backed_check(7)],
    };

    let planned = remedies(&report, &roots, None);

    assert!(
        planned.is_empty(),
        "a tool-backed remedy without a vault root must not reach a preview or a resolution: {}",
        planned.len()
    );
    Ok(())
}
