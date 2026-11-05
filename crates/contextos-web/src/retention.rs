//! Retention sweep for aged-out succeeded run records.
//! Runs on a much slower cadence
//! than the scheduler's own tick evaluation (`scheduler.rs`'s
//! `run_forever`): a 90-day-default window has no need to be checked
//! every 30 seconds.
//!
//! Best-effort throughout, mirroring `scheduler.rs`'s own "degrade,
//! never fail the whole pass" discipline: a job, run, or step that
//! cannot be read or written is simply skipped and retried on the next
//! sweep, never treated as a fatal error for the vault.

use contextos_workflows::{archive_ledger_path, archive_line, has_aged_out, job_records_folder, parse_run_id};
use serde::Deserialize;
use serde_json::{Map, Value};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use crate::jobs::RegisteredJob;
use crate::mcp_client::McpClient;
use crate::vault_content::read_attached_text;

#[derive(Debug, Deserialize)]
struct SearchFilesResult {
    paths: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RunFrontmatter {
    #[serde(default)]
    status: String,
    #[serde(default)]
    started: Option<String>,
    #[serde(default)]
    completed: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FrontmatterReadResult {
    frontmatter: RunFrontmatter,
}

/// Sweeps every job in `jobs` for `status: succeeded` run records older
/// than `window`, archiving then deleting each one. `failed`/`skipped`
/// runs are exempt by construction: [`sweep_run`] only ever acts on a
/// record whose own `status` field reads `succeeded`.
pub async fn sweep_vault(
    client: &McpClient,
    vault_name: &str,
    jobs: &[RegisteredJob],
    now: OffsetDateTime,
    window: Duration,
) {
    for job in jobs {
        sweep_job(
            client,
            vault_name,
            &job.package,
            &job.workflow_slug,
            &job.slug,
            now,
            window,
        )
        .await;
    }
}

async fn sweep_job(
    client: &McpClient,
    vault_name: &str,
    package: &str,
    workflow_slug: &str,
    slug: &str,
    now: OffsetDateTime,
    window: Duration,
) {
    for relative_path in list_run_records(client, vault_name, package, workflow_slug, slug).await {
        sweep_run(
            client,
            vault_name,
            package,
            workflow_slug,
            slug,
            &relative_path,
            now,
            window,
        )
        .await;
    }
}

async fn list_run_records(
    client: &McpClient,
    vault_name: &str,
    package: &str,
    workflow_slug: &str,
    slug: &str,
) -> Vec<String> {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!(
            "{vault_name}://{}",
            job_records_folder(package, workflow_slug, slug)
        )),
    );
    args.insert("pattern".to_owned(), Value::String("**/R-*.md".to_owned()));
    args.insert("max_results".to_owned(), Value::Number(5000.into()));
    let Ok(result) = client.call_tool("fs_search_files".to_owned(), args).await else {
        return Vec::new();
    };
    if result.is_error == Some(true) {
        return Vec::new();
    }
    result
        .into_typed::<SearchFilesResult>()
        .map(|search| search.paths)
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
async fn sweep_run(
    client: &McpClient,
    vault_name: &str,
    package: &str,
    workflow_slug: &str,
    slug: &str,
    relative_path: &str,
    now: OffsetDateTime,
    window: Duration,
) {
    let full_path = format!("{}/{relative_path}", job_records_folder(package, workflow_slug, slug));
    let Some(frontmatter) = read_run_frontmatter(client, vault_name, &full_path).await else {
        return;
    };
    if frontmatter.status != "succeeded" {
        return;
    }
    let Some(completed) = frontmatter.completed.as_deref().and_then(parse_rfc3339) else {
        return;
    };
    if !has_aged_out(completed, now, window) {
        return;
    }
    let Some(started) = frontmatter.started.as_deref().and_then(parse_rfc3339) else {
        return;
    };
    let file_name = relative_path.rsplit('/').next().unwrap_or(relative_path);
    let Some(run_id) = parse_run_id(file_name) else {
        return;
    };
    archive_then_delete(
        client,
        vault_name,
        package,
        workflow_slug,
        slug,
        run_id,
        &full_path,
        completed,
        completed - started,
    )
    .await;
}

fn parse_rfc3339(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text, &Rfc3339).ok()
}

/// Appends `run_id`'s ledger line before deleting its record, never the
/// reverse: if the ledger write fails, the record is left in place
/// rather than risking a silent, unrecorded loss. Idempotent: a run
/// whose ledger line already exists (a sweep re-run after a crash
/// between the two steps) is deleted without appending a second line.
#[allow(clippy::too_many_arguments)]
async fn archive_then_delete(
    client: &McpClient,
    vault_name: &str,
    package: &str,
    workflow_slug: &str,
    slug: &str,
    run_id: u64,
    full_path: &str,
    completed: OffsetDateTime,
    duration: Duration,
) {
    let ledger_path = archive_ledger_path(package, workflow_slug, slug);
    let marker = format!("R-{run_id} |");
    let existing = read_text(client, vault_name, &ledger_path).await.unwrap_or_default();
    if !existing.contains(&marker) {
        let updated = format!("{existing}{}", archive_line(run_id, completed, "succeeded", duration));
        if write_text(client, vault_name, &ledger_path, &updated).await.is_err() {
            tracing::warn!(
                vault = vault_name,
                package,
                workflow = workflow_slug,
                job = slug,
                run_id,
                "failed to append the retention ledger line"
            );
            return;
        }
    }
    delete_record(client, vault_name, full_path).await;
}

async fn read_run_frontmatter(client: &McpClient, vault_name: &str, path: &str) -> Option<RunFrontmatter> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{path}")));
    let result = client.call_tool("frontmatter_read".to_owned(), args).await.ok()?;
    if result.is_error == Some(true) {
        return None;
    }
    result
        .into_typed::<FrontmatterReadResult>()
        .ok()
        .map(|read| read.frontmatter)
}

async fn read_text(client: &McpClient, vault_name: &str, path: &str) -> Option<String> {
    read_attached_text(client, &format!("{vault_name}://{path}"))
        .await
        .ok()
        .flatten()
        .map(|read| read.text)
}

async fn write_text(client: &McpClient, vault_name: &str, path: &str, content: &str) -> Result<(), ()> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{path}")));
    args.insert("content".to_owned(), Value::String(content.to_owned()));
    args.insert("force".to_owned(), Value::Bool(true));
    let result = client
        .call_tool("fs_write_file".to_owned(), args)
        .await
        .map_err(|_| ())?;
    if result.is_error == Some(true) { Err(()) } else { Ok(()) }
}

/// Recoverable trash semantics by default (`security.md`, `destructive_delete`
/// unset): this sweep deletes the aged-out record, not the vault's own
/// permanent-deletion configuration, so no `force`/hard-delete flag is
/// set here.
async fn delete_record(client: &McpClient, vault_name: &str, path: &str) {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{path}")));
    if let Err(error) = client.call_tool("fs_delete_file".to_owned(), args).await {
        tracing::warn!(vault = vault_name, path, %error, "failed to delete an aged-out job run record");
    }
}
