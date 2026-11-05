use super::*;

use serde_json::json;

#[test]
fn a_full_report_gives_every_vault_and_the_time_zone() -> Result<(), VaultInfoError> {
    let result = CallToolResult::structured(json!({
        "vaults": [
            {"name": "alpha", "path": "/vaults/alpha"},
            {"name": "beta", "path": "/vaults/beta"}
        ],
        "time_zone": {"name": "Australia/Perth", "rules": "AWST-8"}
    }));
    let report = VaultInfoReport::try_from(result)?;
    assert_eq!(
        report.vaults,
        vec![
            ReportedVault {
                name: "alpha".to_owned(),
                path: Some(PathBuf::from("/vaults/alpha")),
            },
            ReportedVault {
                name: "beta".to_owned(),
                path: Some(PathBuf::from("/vaults/beta")),
            },
        ]
    );
    assert_eq!(
        report.time_zone,
        Some(ReportedZone {
            name: Some("Australia/Perth".to_owned()),
            rules: Some("AWST-8".to_owned()),
        })
    );
    Ok(())
}

#[test]
fn a_refused_result_is_a_refusal_not_an_unparsable_answer() {
    let result = CallToolResult::structured_error(json!({"code": "refused", "message": "no"}));
    assert!(matches!(
        VaultInfoReport::try_from(result),
        Err(VaultInfoError::Refused)
    ));
}

#[test]
fn an_answer_that_is_not_a_report_of_vaults_is_unparsable() {
    let result = CallToolResult::structured(json!({"unexpected": true}));
    assert!(matches!(
        VaultInfoReport::try_from(result),
        Err(VaultInfoError::Unparsable)
    ));
}

#[test]
fn a_vault_without_a_path_is_still_reported() -> Result<(), VaultInfoError> {
    let result = CallToolResult::structured(json!({"vaults": [{"name": "alpha"}]}));
    let report = VaultInfoReport::try_from(result)?;
    assert_eq!(report.vaults[0].path, None);
    assert_eq!(report.time_zone, None);
    Ok(())
}

#[test]
fn fields_this_crate_does_not_use_are_ignored() -> Result<(), VaultInfoError> {
    let result = CallToolResult::structured(json!({
        "version": "9.9.9",
        "vaults": [{"name": "alpha", "path": "/v", "managed": true, "indexes": {"x": 1}}],
        "time_zone": {"name": "UTC", "offset": "+00:00"}
    }));
    let report = VaultInfoReport::try_from(result)?;
    assert_eq!(report.vaults.len(), 1);
    assert_eq!(report.time_zone.and_then(|zone| zone.name).as_deref(), Some("UTC"));
    Ok(())
}
