use contextos_core::VaultSet;
use contextos_projection::{DefinitionKind, HostKind, Repair};

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn finding(kind: FindingKind, repair: Repair, vault: Option<&str>) -> Finding {
    Finding {
        kind,
        host: Some(HostKind::ClaudeCode),
        definition: Some(DefinitionKind::Agent),
        name: "reviewer".to_owned(),
        vault: vault.map(str::to_owned),
        other_vault: None,
        repair,
        message: "A sentence.".to_owned(),
    }
}

/// Two configured vaults and the names they are configured under; the folders live as long as this does.
struct Vaults {
    _dirs: Vec<tempfile::TempDir>,
    roots: VaultSet,
    names: Vec<String>,
}

fn vaults() -> Result<Vaults, Box<dyn std::error::Error>> {
    let dirs = (0..2)
        .map(|_| tempfile::Builder::new().prefix("vault").tempdir())
        .collect::<Result<Vec<_>, _>>()?;
    let config = crate::Config::try_from(dirs.iter().map(|dir| dir.path().to_path_buf()).collect::<Vec<_>>())?;
    let roots = VaultSet::try_from(&config)?;
    let names = roots.iter().map(|root| root.name().to_owned()).collect();
    Ok(Vaults {
        _dirs: dirs,
        roots,
        names,
    })
}

fn check(finding: &Finding, roots: &VaultSet) -> DoctorCheck {
    DoctorCheck::from(&ProjectionFinding { finding, roots })
}

#[test]
fn a_finding_names_the_configured_vault_its_source_came_from() -> TestResult {
    let Vaults { roots, names, .. } = vaults()?;

    let check = check(
        &finding(FindingKind::ProjectionStale, Repair::Automatic, Some(&names[1])),
        &roots,
    );

    assert_eq!(check.vault_index, Some(1));
    assert_eq!(check.vault_name.as_deref(), Some(names[1].as_str()));
    Ok(())
}

#[test]
fn a_finding_recorded_against_a_vault_that_is_not_configured_names_no_vault() -> TestResult {
    let Vaults { roots, .. } = vaults()?;

    let check = check(
        &finding(
            FindingKind::ProjectionOrphaned,
            Repair::Automatic,
            Some("a-vault-since-removed"),
        ),
        &roots,
    );

    assert_eq!(check.vault_index, None);
    assert_eq!(
        check.vault_name, None,
        "a vault the operator cannot address must not be named"
    );
    assert_eq!(check.remediation, Some(Remediation::WithdrawProjection));
    Ok(())
}

#[test]
fn an_auto_fixable_finding_gets_the_remediation_its_kind_calls_for_and_a_manual_one_none() -> TestResult {
    let Vaults { roots, .. } = vaults()?;
    for (kind, expected) in [
        (FindingKind::ProjectionStale, Remediation::Reproject),
        (FindingKind::ProjectionHandEdited, Remediation::Reproject),
        (FindingKind::ProjectionOrphaned, Remediation::WithdrawProjection),
        (FindingKind::OrientationBlock, Remediation::RestoreOrientation),
    ] {
        let check = check(&finding(kind, Repair::Automatic, None), &roots);
        assert_eq!(check.remediation, Some(expected), "{kind:?}");
        assert!(check.auto_fixable, "{kind:?}");
    }
    let manual = check(&finding(FindingKind::OrientationBlock, Repair::Manual, None), &roots);
    assert_eq!(manual.remediation, None);
    assert!(!manual.auto_fixable);
    Ok(())
}
