//! The checks static validation makes of what a job file and its workflow declare about people and places: who owns the
//! job, and which entity it belongs to. Both depend on vault configuration (`.contextos/team.toml` and
//! `.contextos/entities.toml`), so each can be unverifiable: configuration that cannot be read is an outage, never a
//! defect recorded against the job.

use contextos_core::{DeclaredOwner, EntitiesFile, OwnerStanding, TeamRoster, VAULT_SCOPE};
use contextos_workflows::{Unverifiable, ValidationCode, ValidationReport};

use crate::jobs::{declared_entity_of, declared_owner_of, failure};
use crate::mcp_client::{McpCallError, McpClient};
use crate::vault_content::{AttachedRead, read_attached};

/// How a validation sees the vault's team.
#[derive(Clone, Copy, Debug)]
pub enum OwnerCheck<'a> {
    /// The team is valid, so an owner can be checked against it.
    Roster(&'a TeamRoster),
    /// The team cannot be used, so the owner cannot be checked, and the reason says why. An outage or a configuration
    /// error, and not a defect in the job.
    Unavailable(&'a str),
}

/// What the files of one job declare that validation checks against the vault's configuration.
pub(crate) struct Declarations<'a> {
    /// The job file's name, which a failure of the job file names.
    pub job_file: &'a str,
    pub workflow_text: Option<&'a str>,
    pub job_text: Option<&'a str>,
}

/// Checks the owner and the entities a job's files declare, adding each failure, or each check that could not be made,
/// to `report`. A job file that is not TOML has nothing to check: its schema failure says so.
///
/// # Errors
///
/// Returns [`McpCallError`] when the server cannot be reached.
pub(crate) async fn check_declarations(
    client: &McpClient,
    vault_name: &str,
    declarations: &Declarations<'_>,
    owner: OwnerCheck<'_>,
    report: &mut ValidationReport,
) -> Result<(), McpCallError> {
    let Declarations {
        job_file,
        workflow_text,
        job_text,
    } = declarations;
    if let Some(job_text) = job_text
        && toml::from_str::<toml::Table>(job_text).is_ok()
    {
        check_owner(job_file, declared_owner_of(job_text).as_ref(), owner, report);
    }
    let declared = [
        (*job_file, job_text.and_then(declared_entity_of)),
        ("workflow.toml", workflow_text.and_then(declared_entity_of)),
    ];
    let named: Vec<(&str, String)> = declared
        .into_iter()
        .filter_map(|(artefact, entity)| Some((artefact, entity.filter(|entity| entity != VAULT_SCOPE)?)))
        .collect();
    if named.is_empty() {
        return Ok(());
    }
    match configured_entities(client, vault_name).await? {
        Ok(entities) => {
            for (artefact, entity) in named {
                if !entities.contains(&entity) {
                    report.failures.push(failure(
                        ValidationCode::EntityUnresolved,
                        artefact,
                        format!("entity {entity:?} is neither \"vault\" nor configured in .contextos/entities.toml"),
                    ));
                }
            }
        }
        Err(reason) => report.unverifiable.push(Unverifiable {
            check: "entity".to_owned(),
            reason,
        }),
    }
    Ok(())
}

/// Adds the failure for an owner who is missing or is no member, or the check that could not be made.
fn check_owner(job_file: &str, declared: Option<&DeclaredOwner>, owner: OwnerCheck<'_>, report: &mut ValidationReport) {
    let roster = match owner {
        OwnerCheck::Roster(roster) => roster,
        OwnerCheck::Unavailable(reason) => {
            report.unverifiable.push(Unverifiable {
                check: "owner".to_owned(),
                reason: reason.to_owned(),
            });
            return;
        }
    };
    let message = match OwnerStanding::from((declared, roster)) {
        OwnerStanding::Member(_) => return,
        OwnerStanding::NotAMember(slug) => {
            format!("owner {:?} is not a member of .contextos/team.toml", slug.as_str())
        }
        OwnerStanding::InvalidSlug(text) => {
            format!(
                "owner {text:?} is not a member slug (lowercase letters, digits, and hyphens) of .contextos/team.toml"
            )
        }
        OwnerStanding::Undeclared => "the job file declares no owner; name a member of .contextos/team.toml".to_owned(),
    };
    report
        .failures
        .push(failure(ValidationCode::OwnerUnresolved, job_file, message));
}

/// The slugs of the entities the vault configures: none when the file does not exist, and the reason the check cannot
/// be made when it cannot be read or does not parse.
async fn configured_entities(
    client: &McpClient,
    vault_name: &str,
) -> Result<Result<Vec<String>, String>, McpCallError> {
    let read = read_attached(client, &format!("{vault_name}://{}", contextos_core::ENTITIES_PATH)).await?;
    Ok(match read {
        AttachedRead::Text(text) => toml::from_str::<EntitiesFile>(&text.text)
            .map(|file| file.entities.into_iter().map(|entity| entity.slug).collect())
            .map_err(|_| format!("{} does not parse", contextos_core::ENTITIES_PATH)),
        AttachedRead::Refused { code } if code.as_deref() == Some("path/not-found") => Ok(Vec::new()),
        AttachedRead::Refused { .. } => Err(format!("{} could not be read", contextos_core::ENTITIES_PATH)),
        AttachedRead::NotText => Err(format!("{} is not text", contextos_core::ENTITIES_PATH)),
    })
}
