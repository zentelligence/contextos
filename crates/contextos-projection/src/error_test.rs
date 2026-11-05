use super::*;

#[test]
fn every_code_is_in_the_projection_family() {
    let errors = [
        ProjectionError::InvalidSlug {
            reason: NameRejection::Empty,
        },
        ProjectionError::InvalidSkillPath {
            reason: NameRejection::Empty,
        },
        ProjectionError::InvalidExtension {
            reason: NameRejection::Empty,
        },
        ProjectionError::InvalidConfigurationId {
            reason: NameRejection::Empty,
        },
        ProjectionError::InvalidSkillFolder { reason: "x" },
        ProjectionError::InvalidDefinition {
            field: "model",
            reason: DefinitionRejection::Unrecognised,
        },
        ProjectionError::InvalidHostSetting {
            field: "model",
            reason: NameRejection::Empty,
        },
        ProjectionError::TargetRootRejected {
            reason: TargetRootRejection::NotAbsolute,
        },
        ProjectionError::TargetRootUnresolvable {
            source: io::Error::other("x"),
        },
        ProjectionError::LinkBelowRoot { entry: "a".into() },
        ProjectionError::UnsupportedEntry { entry: "a".into() },
        ProjectionError::Read {
            entry: "a".into(),
            source: io::Error::other("x"),
        },
        ProjectionError::ManifestMalformed {
            reason: ManifestRejection::TargetMismatch,
        },
        ProjectionError::ManifestUnreadable {
            source: io::Error::other("x"),
        },
    ];
    for error in &errors {
        assert!(error.code().starts_with("projection/"), "{}", error.code());
        assert_ne!(error.remediation(), "");
    }
}

#[test]
fn the_documented_codes_are_stable() {
    assert_eq!(
        ProjectionError::LinkBelowRoot { entry: "a".into() }.code(),
        "projection/path-rejected"
    );
    assert_eq!(
        ProjectionError::ManifestMalformed {
            reason: ManifestRejection::TargetMismatch
        }
        .code(),
        "projection/manifest-malformed"
    );
    assert_eq!(
        ProjectionError::from(FsError::NotFound { path: "x".into() }).code(),
        "projection/write-failed"
    );
}

#[test]
fn host_setting_failures_have_their_own_configuration_code() {
    let setting = ProjectionError::InvalidHostSetting {
        field: "server",
        reason: NameRejection::InvalidCharacter,
    };
    let identifier = ProjectionError::InvalidConfigurationId {
        reason: NameRejection::Empty,
    };
    assert_eq!(setting.code(), "projection/invalid-configuration");
    assert_eq!(identifier.code(), "projection/invalid-configuration");
}

#[test]
fn definition_failures_keep_the_invalid_definition_code() {
    let definition = ProjectionError::InvalidDefinition {
        field: "tools",
        reason: DefinitionRejection::Missing,
    };
    assert_eq!(definition.code(), "projection/invalid-definition");
    assert_eq!(
        definition.to_string(),
        "invalid definition: field `tools`: it is missing"
    );
}

#[test]
fn rendering_failures_report_the_write_failed_code() {
    let error = toml::to_string(&1_u8)
        .err()
        .map(|source| ProjectionError::RenderToml { source });
    assert_eq!(error.map(|error| error.code()), Some("projection/write-failed"));
}

#[test]
fn a_malformed_orientation_block_has_its_own_stable_code_and_an_actionable_remediation() {
    let error = ProjectionError::MalformedBlock {
        reason: BlockRejection::BeginWithoutEnd,
    };
    assert_eq!(error.code(), "projection/malformed-block");
    assert!(error.remediation().contains("marker"), "{}", error.remediation());
    assert!(error.to_string().contains("end"), "{error}");
}

#[test]
fn the_invalid_definition_remediation_names_the_tier_and_access_fields_and_not_the_host_fields() {
    let error = ProjectionError::InvalidDefinition {
        field: "tier",
        reason: DefinitionRejection::Missing,
    };
    let remediation = error.remediation();
    assert!(remediation.contains("`tier`"), "{remediation}");
    assert!(remediation.contains("`access`"), "{remediation}");
    assert!(
        !remediation.contains("`model`") && !remediation.contains("`tools`"),
        "{remediation}"
    );
}

#[test]
fn the_malformed_manifest_remediation_says_matching_entries_are_adopted_again_on_the_next_run() {
    let error = ProjectionError::ManifestMalformed {
        reason: ManifestRejection::TargetMismatch,
    };
    let remediation = error.remediation();
    assert!(remediation.contains("adopted again on the next run"), "{remediation}");
    assert!(!remediation.contains("operator's own"), "{remediation}");
}

#[test]
fn a_reparse_point_below_the_target_root_has_its_own_stable_code() {
    let error = ProjectionError::ReparsePoint {
        entry: "reviewer.md".to_owned(),
    };
    assert_eq!(error.code(), "projection/reparse-point");
    assert!(error.to_string().contains("reviewer.md"), "{error}");
}

#[test]
fn the_reparse_point_remediation_names_a_cloud_placeholder_and_never_asks_for_a_link_to_be_removed() {
    let remediation = ProjectionError::ReparsePoint {
        entry: "reviewer.md".to_owned(),
    }
    .remediation();
    assert!(remediation.contains("reparse point"), "{remediation}");
    assert!(remediation.contains("placeholder"), "{remediation}");
    assert!(remediation.contains("offline"), "{remediation}");
    assert!(!remediation.contains("Remove the link"), "{remediation}");
    assert!(!remediation.to_lowercase().contains("remove the link"), "{remediation}");
}

#[test]
fn a_link_below_the_target_root_keeps_its_path_rejected_code_and_link_remediation() {
    let error = ProjectionError::LinkBelowRoot {
        entry: "reviewer.md".to_owned(),
    };
    assert_eq!(error.code(), "projection/path-rejected");
    assert!(
        error.remediation().contains("Remove the link"),
        "{}",
        error.remediation()
    );
}
