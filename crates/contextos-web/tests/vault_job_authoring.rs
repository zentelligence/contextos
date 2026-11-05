//! The vault forms of `jobs create`, `show`, `replace`, and `list`, over a real vault behind a real `contextos`
//! session: the refusals of the shared forms, and the release rules by stage.

mod support;

use contextos_core::ContentHash;
use contextos_web::shared_job_authoring::{AuthoredOutcome, AuthoringError};
use contextos_web::vault_job_authoring::VaultJobAuthoring;
use contextos_web::vault_source_install::SourceInstall;
use contextos_web::vault_source_read::{SourceKind, SourceTarget};
use contextos_workflows::PackageName;
use sha2::{Digest, Sha256};
use support::BoxError;
use support::jobs::{JobVault, Spec, VAULT_NAME};

const VALID: &str = "name = \"Weekly\"\nowner = \"vela\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n";

/// A vault whose `sync` workflow has its release `1.0.0` installed, and `state` as its job state.
async fn vault(state: Option<&str>) -> Result<JobVault, BoxError> {
    let spec = Spec {
        state,
        ..Spec::default()
    };
    let vault = JobVault::open(&spec).await?;
    install(&vault, "1.0.0")?;
    Ok(vault)
}

fn install(vault: &JobVault, version: &str) -> std::io::Result<()> {
    vault.write(
        &format!(".contextos/packages/ops/{version}/manifest.toml"),
        &format!("name = \"sync\"\nversion = \"{version}\"\n"),
    )?;
    vault.write(
        &format!(".contextos/packages/ops/{version}/workflows.toml"),
        "workflows = [\"sync\"]\n",
    )
}

fn hash_of(text: &str) -> ContentHash {
    ContentHash::from(<[u8; 32]>::from(Sha256::digest(text.as_bytes())))
}

macro_rules! authoring {
    ($vault:expr, $servers:ident) => {{
        let client = $vault.clients.get("contextos").ok_or("no primary server")?;
        let $servers = vec!["contextos".to_owned()];
        (client, $servers)
    }};
}

fn refusal<T: std::fmt::Debug>(result: Result<T, AuthoringError>) -> Result<AuthoringError, BoxError> {
    match result {
        Err(error) => Ok(error),
        Ok(value) => Err(format!("expected a refusal, got {value:?}").into()),
    }
}

#[tokio::test]
async fn a_created_job_holds_the_text_given_with_its_comments_and_writes_no_state() -> Result<(), BoxError> {
    let vault = vault(None).await?;
    let (client, servers) = authoring!(vault, servers);
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };
    let bytes = "# why this runs\r\nname = \"Weekly\"   # trailing\r\nowner = \"vela\"\r\ntimeout_secs = 120\r\nversion = \"1.0.0\"\r\n";

    let created = authoring.create("ops", "sync", "weekly", bytes.as_bytes()).await?;

    assert_eq!(created.outcome, AuthoredOutcome::Created);
    // The vault stores text with unix line endings, so that is what is stored and what the hash is of.
    let stored = bytes.replace("\r\n", "\n");
    assert_eq!(vault.read("registry/workflows/ops/sync/jobs/weekly.toml")?, stored);
    assert_eq!(created.hash, hash_of(&stored));
    assert!(created.warnings.is_empty(), "{:?}", created.warnings);
    assert!(
        !vault.root().join(".contextos/jobs.toml").exists(),
        "create writes no state"
    );
    Ok(())
}

#[tokio::test]
async fn a_file_that_does_not_load_is_refused_with_the_cause_a_placed_file_would_get_and_nothing_is_written()
-> Result<(), BoxError> {
    let vault = vault(None).await?;
    let (client, servers) = authoring!(vault, servers);
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };
    let cases = [
        (
            "name = \"W\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nschedule = \"0 3 * * *\"\n",
            "validation/artefact-invalid",
        ),
        ("name = \"W\"\ntimeout_secs = 120\n", "validation/artefact-invalid"),
        (
            "name = \"W\"\ntimeout_secs = 120\nversion = \"^1.0.0\"\n",
            "validation/artefact-invalid",
        ),
        (
            "name = \"W\"\ntimeout_secs = 120\nversion = \"1.0.0-rc.1\"\n",
            "validation/artefact-invalid",
        ),
        (
            "name = \"W\"\ntimeout_secs = 120\nversion = \"1.0.0+b\"\n",
            "validation/artefact-invalid",
        ),
        (
            "name = \"W\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n[libraries]\nacme = \"1.0.0\"\n",
            "validation/artefact-invalid",
        ),
        (
            "name = \"  \"\ntimeout_secs = 120\nversion = \"1.0.0\"\n",
            "validation/artefact-invalid",
        ),
        ("name = \"W\"\nversion = \"1.0.0\"\n", "validation/artefact-invalid"),
        ("this is not toml", "validation/artefact-invalid"),
        (
            "name = \"W\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n[inputs]\nundeclared = 1\n",
            "validation/input-invalid",
        ),
    ];

    for (text, cause) in cases {
        let refused = refusal(authoring.create("ops", "sync", "weekly", text.as_bytes()).await)?;

        assert_eq!(refused.code(), "workflow/job-file-invalid", "{text}");
        assert_eq!(refused.cause(), Some(cause), "{text}");
        assert!(
            !vault
                .root()
                .join("registry/workflows/ops/sync/jobs/weekly.toml")
                .exists(),
            "{text}"
        );
    }
    let too_large = vec![b'a'; 64 * 1024 + 1];
    let refused = refusal(authoring.create("ops", "sync", "weekly", &too_large).await)?;
    assert_eq!(refused.code(), "workflow/job-file-invalid");
    let not_text = refusal(
        authoring
            .create("ops", "sync", "weekly", b"name = \"\xff\xfe\"\n")
            .await,
    )?;
    assert_eq!(not_text.code(), "workflow/job-file-invalid");
    Ok(())
}

#[tokio::test]
async fn a_slug_other_than_the_job_asked_for_and_a_key_that_is_not_usable_are_refused() -> Result<(), BoxError> {
    let vault = vault(None).await?;
    let (client, servers) = authoring!(vault, servers);
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };

    let mismatch = refusal(
        authoring
            .create("ops", "sync", "weekly", format!("{VALID}slug = \"other\"\n").as_bytes())
            .await,
    )?;
    let equal = authoring
        .create(
            "ops",
            "sync",
            "weekly",
            format!("{VALID}slug = \"weekly\"\n").as_bytes(),
        )
        .await?;
    let bad_job = refusal(authoring.create("ops", "sync", "Bad Name", VALID.as_bytes()).await)?;
    let device = refusal(authoring.create("ops", "sync", "nul", VALID.as_bytes()).await)?;

    assert_eq!(mismatch.code(), "workflow/job-key-mismatch");
    assert_eq!(equal.outcome, AuthoredOutcome::Created);
    assert_eq!(bad_job.code(), "workflow/job-key-invalid");
    assert_eq!(device.code(), "workflow/job-key-invalid");
    Ok(())
}

#[tokio::test]
async fn create_never_overwrites_and_says_why_a_key_cannot_be_used() -> Result<(), BoxError> {
    let vault = vault(Some(
        "[ops.sync.ghost]\nlifecycle = \"retired\"\nlifecycle_changed = \"2026-09-01T00:00:00Z\"\n",
    ))
    .await?;
    let (client, servers) = authoring!(vault, servers);
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };
    vault.write(
        "registry/workflows/ops/sync/jobs/other.toml",
        &format!("{VALID}slug = \"taken\"\n"),
    )?;

    let first = authoring.create("ops", "sync", "weekly", VALID.as_bytes()).await?;
    let again = authoring.create("ops", "sync", "weekly", VALID.as_bytes()).await?;
    let different = refusal(
        authoring
            .create("ops", "sync", "weekly", VALID.replace("Weekly", "Other").as_bytes())
            .await,
    )?;
    let taken = refusal(authoring.create("ops", "sync", "taken", VALID.as_bytes()).await)?;
    let remains = refusal(authoring.create("ops", "sync", "ghost", VALID.as_bytes()).await)?;

    assert_eq!(first.outcome, AuthoredOutcome::Created);
    assert_eq!(again.outcome, AuthoredOutcome::Unchanged);
    assert_eq!(different.code(), "workflow/job-exists");
    assert_eq!(taken.code(), "workflow/job-key-taken");
    assert_eq!(remains.code(), "workflow/job-state-remains");
    assert_eq!(vault.read("registry/workflows/ops/sync/jobs/weekly.toml")?, VALID);
    Ok(())
}

#[tokio::test]
async fn a_job_naming_a_release_that_is_not_installed_is_refused_as_package_unavailable() -> Result<(), BoxError> {
    let vault = vault(None).await?;
    let (client, servers) = authoring!(vault, servers);
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };

    let other_version = refusal(
        authoring
            .create("ops", "sync", "weekly", VALID.replace("1.0.0", "2.0.0").as_bytes())
            .await,
    )?;
    let other_workflow = refusal(authoring.create("ops", "nowhere", "weekly", VALID.as_bytes()).await)?;

    for refused in [other_version, other_workflow] {
        assert_eq!(refused.code(), "workflow/package-unavailable");
    }
    assert!(
        !vault
            .root()
            .join("registry/workflows/ops/sync/jobs/weekly.toml")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn a_new_job_may_name_the_current_or_a_future_release_and_not_the_previous_or_a_retired_one()
-> Result<(), BoxError> {
    let versions = "[ops.sync]\ncurrent = \"3.0.0\"\ncurrent_at = \"2026-11-03T03:00:00Z\"\n\
                 previous = \"2.0.0\"\nprevious_at = \"2026-11-03T03:00:00Z\"\n";
    let vault = vault(Some(versions)).await?;
    for version in ["2.0.0", "3.0.0", "4.0.0"] {
        install(&vault, version)?;
    }
    let (client, servers) = authoring!(vault, servers);
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };
    let naming = |version: &str| VALID.replace("1.0.0", version);

    let current = authoring
        .create("ops", "sync", "on-current", naming("3.0.0").as_bytes())
        .await?;
    let future = authoring
        .create("ops", "sync", "on-future", naming("4.0.0").as_bytes())
        .await?;
    let previous = refusal(
        authoring
            .create("ops", "sync", "on-previous", naming("2.0.0").as_bytes())
            .await,
    )?;
    let retired = refusal(
        authoring
            .create("ops", "sync", "on-retired", naming("1.0.0").as_bytes())
            .await,
    )?;

    assert_eq!(current.outcome, AuthoredOutcome::Created);
    assert_eq!(future.outcome, AuthoredOutcome::Created);
    assert_eq!(previous.code(), "workflow/transition-refused");
    assert!(previous.to_string().contains("previous"), "{previous}");
    assert_eq!(retired.code(), "workflow/transition-refused");
    assert!(retired.to_string().contains("retired release"), "{retired}");
    Ok(())
}

#[tokio::test]
async fn a_key_a_standing_successor_names_as_its_predecessor_is_taken() -> Result<(), BoxError> {
    let vault = vault(None).await?;
    let (client, servers) = authoring!(vault, servers);
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };
    vault.write(
        "registry/workflows/ops/sync/jobs/weekly-v2.toml",
        &format!("{VALID}predecessor = \"weekly\"\n"),
    )?;

    let refused = refusal(authoring.create("ops", "sync", "weekly", VALID.as_bytes()).await)?;

    assert_eq!(refused.code(), "workflow/job-key-taken");
    assert!(refused.to_string().contains("weekly-v2"), "{refused}");
    Ok(())
}

#[tokio::test]
async fn show_reports_the_file_and_its_hash_and_replace_needs_the_hash_last_shown() -> Result<(), BoxError> {
    let vault = vault(None).await?;
    let (client, servers) = authoring!(vault, servers);
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };
    authoring.create("ops", "sync", "weekly", VALID.as_bytes()).await?;

    let shown = authoring.show("ops", "sync", "weekly").await?;
    let revised = VALID.replace("Weekly", "Revised");
    let stale = refusal(
        authoring
            .replace("ops", "sync", "weekly", revised.as_bytes(), &hash_of("something else"))
            .await,
    )?;
    let replaced = authoring
        .replace("ops", "sync", "weekly", revised.as_bytes(), &shown.hash)
        .await?;
    let unchanged = authoring
        .replace("ops", "sync", "weekly", revised.as_bytes(), &hash_of(&revised))
        .await?;
    let missing = refusal(authoring.show("ops", "sync", "nothing").await)?;

    assert_eq!(shown.content, VALID);
    assert_eq!(shown.hash, hash_of(VALID));
    assert_eq!(stale.code(), "workflow/job-changed");
    assert!(stale.to_string().contains(<&str>::from(&shown.hash)), "{stale}");
    assert_eq!(replaced.outcome, AuthoredOutcome::Replaced);
    assert_eq!(unchanged.outcome, AuthoredOutcome::Unchanged);
    assert_eq!(missing.code(), "workflow/job-not-found");
    assert_eq!(vault.read("registry/workflows/ops/sync/jobs/weekly.toml")?, revised);
    Ok(())
}

#[tokio::test]
async fn a_replacement_may_not_re_key_the_job() -> Result<(), BoxError> {
    let vault = vault(None).await?;
    let (client, servers) = authoring!(vault, servers);
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };
    authoring.create("ops", "sync", "weekly", VALID.as_bytes()).await?;
    let shown = authoring.show("ops", "sync", "weekly").await?;

    let refused = refusal(
        authoring
            .replace(
                "ops",
                "sync",
                "weekly",
                format!("{VALID}slug = \"other\"\n").as_bytes(),
                &shown.hash,
            )
            .await,
    )?;

    assert_eq!(refused.code(), "workflow/job-key-mismatch");
    assert_eq!(vault.read("registry/workflows/ops/sync/jobs/weekly.toml")?, VALID);
    Ok(())
}

#[tokio::test]
async fn a_job_that_is_enabled_suspended_or_retired_cannot_change_its_version_or_predecessor() -> Result<(), BoxError> {
    for stage in ["enabled", "suspended", "retired"] {
        let state = format!(
            "[ops.sync.weekly]\nlifecycle = \"{stage}\"\nschedule = \"0 3 * * *\"\n\
             lifecycle_changed = \"2026-09-01T00:00:00Z\"\nlifecycle_actor = \"operator\"\n"
        );
        let vault = vault(Some(&state)).await?;
        install(&vault, "2.0.0")?;
        vault.write("registry/workflows/ops/sync/jobs/weekly.toml", VALID)?;
        let (client, servers) = authoring!(vault, servers);
        let authoring = VaultJobAuthoring {
            client: &client,
            vault: VAULT_NAME,
            known_servers: &servers,
        };
        let shown = authoring.show("ops", "sync", "weekly").await?;

        for replacement in [
            VALID.replace("1.0.0", "2.0.0"),
            format!("{VALID}predecessor = \"old\"\n"),
        ] {
            let refused = refusal(
                authoring
                    .replace("ops", "sync", "weekly", replacement.as_bytes(), &shown.hash)
                    .await,
            )?;

            assert_eq!(refused.code(), "workflow/transition-refused", "{stage}");
            assert!(refused.to_string().contains("jobs successor"), "{stage}: {refused}");
        }
        let renamed = VALID.replace("Weekly", "Renamed");
        let allowed = authoring
            .replace("ops", "sync", "weekly", renamed.as_bytes(), &shown.hash)
            .await?;
        assert_eq!(allowed.outcome, AuthoredOutcome::Replaced, "{stage}");
    }
    Ok(())
}

#[tokio::test]
async fn a_job_not_yet_enabled_may_change_its_version_to_an_installed_current_or_future_release() -> Result<(), BoxError>
{
    for stage in ["validated", "registered"] {
        let state = format!("[ops.sync.weekly]\nlifecycle = \"{stage}\"\n");
        let vault = vault(Some(&state)).await?;
        install(&vault, "2.0.0")?;
        vault.write("registry/workflows/ops/sync/jobs/weekly.toml", VALID)?;
        let (client, servers) = authoring!(vault, servers);
        let authoring = VaultJobAuthoring {
            client: &client,
            vault: VAULT_NAME,
            known_servers: &servers,
        };
        let shown = authoring.show("ops", "sync", "weekly").await?;

        let replaced = authoring
            .replace(
                "ops",
                "sync",
                "weekly",
                VALID.replace("1.0.0", "2.0.0").as_bytes(),
                &shown.hash,
            )
            .await?;
        let missing = refusal(
            authoring
                .replace(
                    "ops",
                    "sync",
                    "weekly",
                    VALID.replace("1.0.0", "9.0.0").as_bytes(),
                    &hash_of(&VALID.replace("1.0.0", "2.0.0")),
                )
                .await,
        )?;

        assert_eq!(replaced.outcome, AuthoredOutcome::Replaced, "{stage}");
        assert_eq!(missing.code(), "workflow/package-unavailable", "{stage}");
    }
    Ok(())
}

#[tokio::test]
async fn a_replacement_keeping_its_version_is_stored_with_a_warning_when_the_release_is_not_installed()
-> Result<(), BoxError> {
    let vault = vault(None).await?;
    vault.write(
        "registry/workflows/ops/sync/jobs/weekly.toml",
        &VALID.replace("1.0.0", "7.0.0"),
    )?;
    let (client, servers) = authoring!(vault, servers);
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };
    let shown = authoring.show("ops", "sync", "weekly").await?;
    let revised = VALID.replace("1.0.0", "7.0.0").replace("Weekly", "Revised");

    let replaced = authoring
        .replace("ops", "sync", "weekly", revised.as_bytes(), &shown.hash)
        .await?;

    assert_eq!(replaced.outcome, AuthoredOutcome::Replaced);
    assert_eq!(replaced.warnings.len(), 1, "{:?}", replaced.warnings);
    assert_eq!(replaced.warnings[0].code(), "workflow/package-unavailable");
    Ok(())
}

#[tokio::test]
async fn list_shows_every_job_with_stage_schedule_release_kind_links_hash_and_problems() -> Result<(), BoxError> {
    let state = "[ops.sync.nightly]\nlifecycle = \"enabled\"\nschedule = \"0 3 * * *\"\n\
                 lifecycle_changed = \"2026-09-01T00:00:00Z\"\nlifecycle_actor = \"operator\"\n\
                 [ops.sync.ghost]\nlifecycle = \"validated\"\n";
    let vault = vault(Some(state)).await?;
    vault.write(
        "registry/workflows/ops/sync/jobs/broken.toml",
        "name = \"B\"\ntimeout_secs = 5\n",
    )?;
    vault.write(
        "registry/workflows/ops/sync/jobs/nightly-next.toml",
        &format!("{VALID}predecessor = \"nightly\"\n"),
    )?;
    let (client, servers) = authoring!(vault, servers);
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };

    let rows = authoring.list().await?;

    let row = |job: &str| {
        rows.iter()
            .find(|row| row.key.job == job)
            .ok_or_else(|| format!("no row for {job}"))
    };
    let nightly = row("nightly")?;
    assert_eq!(nightly.key.package, "ops");
    assert_eq!(nightly.key.workflow, "sync");
    assert_eq!(nightly.lifecycle.as_str(), "enabled");
    assert_eq!(nightly.schedule.as_deref(), Some("0 3 * * *"));
    assert_eq!(nightly.ready, Some(true));
    assert_eq!(
        nightly.version.as_ref().map(ToString::to_string).as_deref(),
        Some("1.0.0")
    );
    assert_eq!(nightly.successor.as_deref(), Some("nightly-next"));
    assert!(nightly.hash.is_some() && nightly.problems.is_empty(), "{nightly:?}");
    let next = row("nightly-next")?;
    assert_eq!(next.predecessor.as_deref(), Some("nightly"));
    let broken = row("broken")?;
    assert_eq!(broken.problems.len(), 1);
    assert_eq!(broken.problems[0].code, "validation/artefact-invalid");
    let ghost = row("ghost")?;
    assert_eq!(ghost.hash, None);
    assert_eq!(ghost.problems[0].code, "workflow/job-not-found");
    Ok(())
}

/// Installs the vault's `ops` source as the next release, through a store that keeps a ledger.
async fn install_source(vault: &JobVault) -> Result<(), BoxError> {
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    SourceInstall {
        client: &client,
        vault: VAULT_NAME,
    }
    .install(
        &SourceTarget {
            kind: SourceKind::Workflows,
            name: PackageName::try_from("ops")?,
        },
        None,
    )
    .await?;
    Ok(())
}

fn releases_in_store(vault: &JobVault) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(vault.root().join(".contextos/packages/ops"))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// A vault whose store holds `ops` 1.0.0, 1.0.1, and 1.0.2, installed from source, with `nightly` naming 1.0.0.
async fn vault_with_three_releases() -> Result<JobVault, BoxError> {
    let vault = JobVault::open(&Spec::default()).await?;
    std::fs::remove_dir_all(vault.root().join(".contextos/packages"))?;
    install_source(&vault).await?;
    let holder = "registry/workflows/ops/sync/jobs/holder.toml";
    for (script, naming) in [("log_info(\"b\");", "1.0.0"), ("log_info(\"c\");", "1.0.1")] {
        vault.write(holder, &VALID.replace("1.0.0", naming))?;
        vault.write("registry/workflows/ops/sync/sync.rhai", script)?;
        install_source(&vault).await?;
    }
    std::fs::remove_file(vault.root().join(holder))?;
    assert_eq!(releases_in_store(&vault), ["1.0.0", "1.0.1", "1.0.2"]);
    Ok(vault)
}

#[tokio::test]
async fn changing_a_jobs_version_in_place_removes_the_release_it_no_longer_names() -> Result<(), BoxError> {
    let vault = vault_with_three_releases().await?;
    let (client, servers) = authoring!(vault, servers);
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };
    let shown = authoring.show("ops", "sync", "nightly").await?;
    let renamed = shown.content.replace("1.0.0", "1.0.1");

    let replaced = authoring
        .replace("ops", "sync", "nightly", renamed.as_bytes(), &shown.hash)
        .await?;

    assert_eq!(replaced.outcome, AuthoredOutcome::Replaced);
    assert!(replaced.warnings.is_empty(), "{:?}", replaced.warnings);
    // The job names 1.0.1 now and 1.0.2 is the newest future release.
    assert_eq!(releases_in_store(&vault), ["1.0.1", "1.0.2"]);
    Ok(())
}

#[tokio::test]
async fn a_replacement_that_keeps_the_version_runs_no_retention() -> Result<(), BoxError> {
    let vault = vault_with_three_releases().await?;
    let (client, servers) = authoring!(vault, servers);
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };
    let shown = authoring.show("ops", "sync", "nightly").await?;
    let renamed = shown.content.replace("Nightly", "Renamed");

    let replaced = authoring
        .replace("ops", "sync", "nightly", renamed.as_bytes(), &shown.hash)
        .await?;

    assert_eq!(replaced.outcome, AuthoredOutcome::Replaced);
    assert_eq!(releases_in_store(&vault), ["1.0.0", "1.0.1", "1.0.2"]);
    Ok(())
}
