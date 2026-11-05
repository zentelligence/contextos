use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn path(raw: &str) -> Result<VaultRelativePath, VaultRelativePathError> {
    VaultRelativePath::try_from(raw)
}

#[test]
fn an_empty_file_yields_the_scaffold_defaults() -> TestResult {
    let config = VaultManagementConfig::try_from("")?;
    assert_eq!(config, VaultManagementConfig::default());
    let register = config.register();
    assert_eq!(register.paths.base.as_str(), "memory/decisions.base");
    assert_eq!(register.paths.dir.as_str(), "memory/decisions");
    assert_eq!(register.decision.prefix, "VD");
    assert_eq!(register.question.prefix, "VQ");
    assert_eq!(
        register.question.template.as_ref().map(VaultRelativePath::as_str),
        Some("templates/registry-vault-question.md")
    );
    Ok(())
}

#[test]
fn every_registry_type_defaults_to_its_own_scaffold_folder() {
    let config = VaultManagementConfig::default();
    let expected = [
        (
            RegistryKind::Agents,
            "registry/agents",
            Some("templates/registry-agent.md"),
        ),
        (RegistryKind::Hats, "registry/hats", Some("templates/registry-hat.md")),
        (
            RegistryKind::Prompts,
            "registry/prompts",
            Some("templates/registry-prompt.md"),
        ),
        (
            RegistryKind::Roles,
            "registry/roles",
            Some("templates/registry-role.md"),
        ),
        (RegistryKind::Scripts, "registry/scripts", None),
        (
            RegistryKind::Skills,
            "registry/skills",
            Some("templates/registry-skill.md"),
        ),
    ];
    for (kind, dir, template) in expected {
        let paths = config.registry(kind);
        assert_eq!(paths.dir.as_str(), dir, "{kind:?}");
        assert_eq!(
            paths.template.as_ref().map(VaultRelativePath::as_str),
            template,
            "{kind:?}"
        );
    }
    assert_eq!(RegistryKind::ALL.len(), expected.len());
}

#[test]
fn a_partial_table_overrides_only_the_keys_it_sets() -> TestResult {
    let config =
        VaultManagementConfig::try_from("[decisions]\ndir = \"records\"\n\n[decisions.question]\nprefix = \"Q\"\n")?;
    let register = config.register();
    assert_eq!(register.paths.dir.as_str(), "records");
    assert_eq!(register.paths.base.as_str(), "memory/decisions.base");
    assert_eq!(register.question.prefix, "Q");
    assert_eq!(register.decision, RegisterSettings::default().decision);
    Ok(())
}

#[test]
fn an_explicitly_empty_template_means_no_template_not_the_default() -> TestResult {
    let config = VaultManagementConfig::try_from(
        "[registry.agents]\ntemplate = \"\"\n\n[decisions.decision]\ntemplate = \"\"\n",
    )?;
    assert_eq!(config.registry(RegistryKind::Agents).template, None);
    assert_eq!(config.register().decision.template, None);
    Ok(())
}

#[test]
fn an_unknown_key_or_table_is_rejected_rather_than_ignored() {
    for source in [
        "[decisions]\nfolder = \"memory/decisions\"\n",
        "[questions]\ndir = \"memory/questions\"\n",
        "[registry.workflows]\ndir = \"registry/workflows\"\n",
        "[decisions.task]\nprefix = \"A\"\n",
    ] {
        let error = VaultManagementConfig::try_from(source);
        assert!(
            matches!(error, Err(VaultManagementConfigError::Toml(_))),
            "{source}: {error:?}"
        );
    }
}

#[test]
fn an_unsafe_hidden_or_append_only_location_is_rejected_naming_its_key() -> TestResult {
    let cases = [
        (
            "[decisions]\ndir = \"../elsewhere\"\n",
            "decisions.dir",
            "path/invalid-segment",
        ),
        (
            "[decisions]\ndir = \".contextos\"\n",
            "decisions.dir",
            "vault-management/hidden-path",
        ),
        (
            "[registry.prompts]\ndir = \"memory/log\"\n",
            "registry.prompts.dir",
            "vault-management/reserved-location",
        ),
        (
            "[decisions]\ndir = \"memory/sessions/2026\"\n",
            "decisions.dir",
            "vault-management/reserved-location",
        ),
        (
            "[registry.roles]\ntemplate = \"templates/.drafts/role.md\"\n",
            "registry.roles.template",
            "vault-management/hidden-path",
        ),
    ];
    for (source, key, code) in cases {
        let error = VaultManagementConfig::try_from(source);
        let Err(error) = error else {
            return Err(format!("{source} was accepted").into());
        };
        assert_eq!(error.code(), code, "{source}");
        assert!(error.to_string().contains(key), "{source}: {error}");
    }
    Ok(())
}

#[test]
fn the_register_base_must_be_a_base_file_and_its_templates_markdown() {
    let base = VaultManagementConfig::try_from("[decisions]\nbase = \"memory/decisions.md\"\n");
    assert!(
        matches!(base, Err(VaultManagementConfigError::BaseExtension { ref key }) if key == "decisions.base"),
        "{base:?}"
    );
    let template = VaultManagementConfig::try_from("[decisions.question]\ntemplate = \"templates/question.txt\"\n");
    assert!(
        matches!(template, Err(VaultManagementConfigError::TemplateExtension { ref key }) if key == "decisions.question.template"),
        "{template:?}"
    );
}

#[test]
fn the_two_record_types_need_plain_and_distinct_id_prefixes() {
    let invalid = VaultManagementConfig::try_from("[decisions.decision]\nprefix = \"V-D\"\n");
    assert!(
        matches!(invalid, Err(VaultManagementConfigError::Prefix { ref key }) if key == "decisions.decision.prefix"),
        "{invalid:?}"
    );
    let shared = VaultManagementConfig::try_from("[decisions.question]\nprefix = \"vd\"\n");
    assert!(
        matches!(shared, Err(VaultManagementConfigError::DuplicatePrefix)),
        "{shared:?}"
    );
}

#[test]
fn updating_the_register_preserves_comments_and_every_other_table() -> TestResult {
    let source =
        "# Managed by hand as well.\n[registry.roles]\n# Kept beside the register.\ndir = \"registry/roles\"\n";
    let mut settings = RegisterSettings::default();
    settings.paths.dir = path("records/decisions")?;
    settings.paths.base = path("records/decisions.base")?;
    settings.question.prefix = "Q".to_owned();
    settings.decision.template = None;
    let updated = with_register_settings(source, &settings)?;
    assert!(updated.contains("# Managed by hand as well."), "{updated}");
    assert!(updated.contains("# Kept beside the register."), "{updated}");
    let reparsed = VaultManagementConfig::try_from(updated.as_str())?;
    assert_eq!(reparsed.register(), &settings);
    assert_eq!(reparsed.registry(RegistryKind::Roles).dir.as_str(), "registry/roles");
    Ok(())
}

#[test]
fn updating_a_registry_type_round_trips_through_a_fresh_file() -> TestResult {
    let paths = RegistryTypePaths {
        dir: path("library/prompts")?,
        template: Some(path("templates/prompt.md")?),
    };
    let updated = with_registry_type_paths("", RegistryKind::Prompts, &paths)?;
    assert!(
        !updated.contains("[registry]\n"),
        "the parent table stays implicit: {updated}"
    );
    let reparsed = VaultManagementConfig::try_from(updated.as_str())?;
    assert_eq!(reparsed.registry(RegistryKind::Prompts), &paths);
    assert_eq!(
        reparsed.registry(RegistryKind::Hats),
        VaultManagementConfig::default().registry(RegistryKind::Hats)
    );
    Ok(())
}

#[test]
fn clearing_a_registry_template_persists_as_an_explicit_empty_value() -> TestResult {
    let paths = RegistryTypePaths {
        dir: path("registry/roles")?,
        template: None,
    };
    let updated = with_registry_type_paths("", RegistryKind::Roles, &paths)?;
    assert_eq!(
        VaultManagementConfig::try_from(updated.as_str())?
            .registry(RegistryKind::Roles)
            .template,
        None
    );
    Ok(())
}

#[test]
fn an_update_refuses_to_rewrite_a_file_that_is_not_valid_toml() {
    let error = with_register_settings("[decisions\n", &RegisterSettings::default());
    assert!(matches!(error, Err(VaultManagementConfigError::Toml(_))), "{error:?}");
}

#[test]
fn kinds_round_trip_through_their_url_values() -> TestResult {
    for kind in RegistryKind::ALL {
        assert_eq!(RegistryKind::try_from(kind.slug())?, kind);
    }
    for kind in RecordType::ALL {
        assert_eq!(RecordType::try_from(kind.value())?, kind);
    }
    assert!(RegistryKind::try_from("apps").is_err());
    assert!(RecordType::try_from("task").is_err());
    Ok(())
}

#[test]
fn skills_are_folders_with_an_entry_file_and_scripts_keep_their_own_extension() {
    assert_eq!(
        RegistryKind::Skills.layout(),
        DefinitionLayout::Folder { entry: "SKILL.md" }
    );
    assert_eq!(RegistryKind::Scripts.layout(), DefinitionLayout::AnyFile);
    assert_eq!(RegistryKind::Agents.layout(), DefinitionLayout::Markdown);
}

#[test]
fn every_scaffold_default_satisfies_the_validation_rules() -> TestResult {
    let config = VaultManagementConfig::default();
    config.register().validate()?;
    let register = config.register();
    let mut defaults: Vec<&VaultRelativePath> = vec![&register.paths.base, &register.paths.dir];
    for kind in RecordType::ALL {
        defaults.extend(register.record_type(kind).template.as_ref());
    }
    for kind in RegistryKind::ALL {
        let paths = config.registry(kind);
        paths.validate(kind)?;
        defaults.push(&paths.dir);
        defaults.extend(paths.template.as_ref());
    }
    for default in defaults {
        assert_eq!(VaultRelativePath::try_from(default.as_str()).as_ref(), Ok(default));
    }
    Ok(())
}
