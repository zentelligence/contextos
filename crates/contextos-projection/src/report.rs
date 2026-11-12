use contextos_core::OperationWarning;

use crate::{BlockRejection, DefinitionKind, Finding, FindingKind, HostKind, NameRejection, Rejection, Repair};

/// What a full pass over the definitions found and could not do.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProjectionReport {
    /// What `doctor` should say, whether or not the pass repaired it.
    pub findings: Vec<Finding>,
    /// Failures that stopped one definition or host, none of which stopped the others.
    pub warnings: Vec<OperationWarning>,
    /// Which entry each failure in `warnings` stopped, as data, so a caller never has to read the prose of a warning
    /// to learn whether a particular repair took.
    pub failures: Vec<Failure>,
}

/// The source path a host's orientation block is projected from, as findings and failures name it.
pub(crate) const ORIENTATION_SOURCE: &str = "registry/instructions/global.md";

/// One entry a pass tried to change and could not.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Failure {
    /// The host the change was meant for.
    pub host: HostKind,
    /// The kind of definition, or `None` for a host's global instructions file.
    pub definition: Option<DefinitionKind>,
    /// The slug of the definition, or the instructions source path, exactly as the matching finding names it.
    pub name: String,
    /// The stable code of the warning that reports the failure.
    pub code: String,
}

/// The facts a finding or warning message is written from.
pub(crate) struct Subject<'a> {
    pub(crate) kind: FindingKind,
    pub(crate) host: Option<HostKind>,
    pub(crate) definition: DefinitionKind,
    pub(crate) name: &'a str,
    pub(crate) vault: Option<&'a str>,
    pub(crate) other_vault: Option<&'a str>,
    pub(crate) rejection: Option<Rejection>,
    /// For a foreign-owner finding, the first 12 hexadecimal characters of the owning configuration's identity.
    pub(crate) owner: Option<&'a str>,
}

fn rejection_text(rejection: Option<Rejection>) -> String {
    rejection.map_or_else(String::new, |Rejection { field, reason }| {
        format!(" (field `{field}`: {reason})")
    })
}

impl From<&Subject<'_>> for Finding {
    fn from(value: &Subject<'_>) -> Self {
        let Subject {
            kind,
            host,
            definition,
            name,
            vault,
            other_vault,
            rejection,
            owner,
        } = value;
        let place = host.map_or_else(String::new, |host| format!(" for {host}"));
        let source = vault.map_or_else(String::new, |vault| format!(" from vault `{vault}`"));
        // What an existing entry is compared with: the named vault's definition when the finding knows the vault.
        let compared_with = |tense: &str| {
            vault.map_or_else(
                || "its source definition".to_owned(),
                |vault| format!("what vault `{vault}` {tense}"),
            )
        };
        let message = match kind {
            FindingKind::ProjectionStale => format!(
                "The {definition} `{name}`{place} is missing or differs from {}.",
                compared_with("now defines")
            ),
            FindingKind::ProjectionHandEdited => format!(
                "The projected {definition} `{name}`{place} was edited outside ContextOS; reprojecting replaces the edit."
            ),
            FindingKind::ProjectionOrphaned => format!(
                "The projected {definition} `{name}`{place} no longer has a source definition in any configured vault."
            ),
            FindingKind::ProjectionInvalidDefinition => format!(
                "The {definition} `{name}`{source} cannot be projected{}; correct the definition. Any earlier projection is left in place.",
                rejection_text(*rejection)
            ),
            FindingKind::ProjectionShadowed => format!(
                "Vault `{}` defines the {definition} `{name}` differently from vault `{}`, which is projected; rename, align, or remove one.",
                other_vault.unwrap_or("?"),
                vault.unwrap_or("?")
            ),
            FindingKind::ProjectionUnownedCollision => format!(
                "A file or folder at the {definition} `{name}`{place} was not projected by ContextOS and differs from {}; move it or rename the definition.",
                compared_with("defines")
            ),
            FindingKind::ProjectionForeignOwner => {
                let owner = owner.map_or_else(
                    || "another configuration".to_owned(),
                    |owner| format!("the configuration starting `{owner}`"),
                );
                format!(
                    "The {definition} `{name}`{place} is recorded as projected by {owner}; resolve it from that configuration or disable projection in one of them."
                )
            }
            FindingKind::OrientationBlock | FindingKind::CodexOverrideHidesAgents => {
                InstructionsIssue::describe_kind(*kind, *host)
            }
            FindingKind::CodexReadOnlyUnenforced => format!(
                "The read-only {definition} `{name}`{source} is projected to Codex, which can only switch off shell, apps, and plugins; MCP tools stay available to it."
            ),
        };
        Self {
            kind: *kind,
            host: *host,
            definition: Some(*definition),
            name: (*name).to_owned(),
            vault: vault.map(str::to_owned),
            other_vault: other_vault.map(str::to_owned),
            repair: Repair::from(*kind),
            message,
        }
    }
}

/// The warning code for a finding that is also raised on the write that caused it, if there is one.
pub(crate) const fn warning_code(kind: FindingKind) -> Option<&'static str> {
    match kind {
        FindingKind::ProjectionInvalidDefinition => Some("projection/invalid-definition"),
        FindingKind::ProjectionUnownedCollision => Some("projection/unowned-collision"),
        FindingKind::ProjectionForeignOwner => Some("projection/foreign-owner"),
        FindingKind::ProjectionStale
        | FindingKind::ProjectionHandEdited
        | FindingKind::ProjectionOrphaned
        | FindingKind::ProjectionShadowed
        | FindingKind::CodexReadOnlyUnenforced
        | FindingKind::OrientationBlock
        | FindingKind::CodexOverrideHidesAgents => None,
    }
}

/// A warning for a definition whose file or folder name is not a valid slug.
pub(crate) fn invalid_name_warning(
    definition: DefinitionKind,
    vault: &str,
    raw: &str,
    reason: NameRejection,
) -> OperationWarning {
    OperationWarning {
        code: "projection/invalid-definition".to_owned(),
        message: format!(
            "The {definition} named {raw:?} in vault `{vault}` cannot be projected: its name is invalid ({reason}). \
             Rename it to lowercase letters and digits separated by single hyphens."
        ),
    }
}

/// A finding for a definition whose file or folder name is not a valid slug.
pub(crate) fn invalid_name_finding(
    definition: DefinitionKind,
    vault: &str,
    raw: &str,
    reason: NameRejection,
) -> Finding {
    Finding {
        kind: FindingKind::ProjectionInvalidDefinition,
        host: None,
        definition: Some(definition),
        name: raw.to_owned(),
        vault: Some(vault.to_owned()),
        other_vault: None,
        repair: Repair::Manual,
        message: format!(
            "The {definition} named {raw:?} in vault `{vault}` cannot be projected: its name is invalid ({reason}). \
             Rename it to lowercase letters and digits separated by single hyphens."
        ),
    }
}

/// A warning for a definition invalid for a reason other than its name, raised once however many hosts there are.
pub(crate) fn invalid_definition_warning(subject: &Subject<'_>) -> OperationWarning {
    OperationWarning {
        code: "projection/invalid-definition".to_owned(),
        message: Finding::from(subject).message,
    }
}

/// What is wrong with a host's global instructions file.
#[derive(Clone, Copy, Debug)]
pub(crate) enum InstructionsIssue {
    /// The orientation block is missing or differs from the source.
    Differs,
    /// The orientation block remains though no configured vault defines the source any longer.
    Orphaned,
    /// The file's markers do not form exactly one block.
    Malformed(BlockRejection),
    /// The source holds a line that reads as a marker, so it cannot be placed in a block.
    SourceHoldsMarker,
    /// An `AGENTS.override.md` beside Codex's instructions file hides it.
    OverrideFile,
}

impl InstructionsIssue {
    const fn kind(self) -> FindingKind {
        match self {
            Self::Differs | Self::Orphaned | Self::Malformed(_) | Self::SourceHoldsMarker => {
                FindingKind::OrientationBlock
            }
            Self::OverrideFile => FindingKind::CodexOverrideHidesAgents,
        }
    }

    /// Whether the server may repair the finding: only a block it can place or withdraw without guessing.
    const fn repair(self) -> Repair {
        match self {
            Self::Differs | Self::Orphaned => Repair::Automatic,
            Self::Malformed(_) | Self::SourceHoldsMarker | Self::OverrideFile => Repair::Manual,
        }
    }

    /// The sentence for a finding of `kind` when no detail is known.
    fn describe_kind(kind: FindingKind, host: Option<HostKind>) -> String {
        let place = host.map_or_else(String::new, |host| format!(" for {host}"));
        match kind {
            FindingKind::CodexOverrideHidesAgents => format!(
                "An AGENTS.override.md sits beside the global instructions{place}, so Codex ignores them; remove or merge the override file."
            ),
            _ => format!("The ContextOS orientation block in the global instructions{place} needs attention."),
        }
    }

    fn message(self, host: HostKind, vault: Option<&str>) -> String {
        let source = vault.map_or_else(String::new, |vault| format!(" from vault `{vault}`"));
        match self {
            Self::Differs => format!(
                "The ContextOS orientation block in the global instructions for {host} is missing or differs from what registry/instructions/global.md{source} defines; restoring it replaces only the lines between its markers."
            ),
            Self::Orphaned => format!(
                "The ContextOS orientation block in the global instructions for {host} remains though no configured vault defines registry/instructions/global.md; withdrawing it removes only the block and its markers."
            ),
            Self::Malformed(reason) => format!(
                "The global instructions for {host} hold malformed ContextOS orientation block markers ({reason}); fix the markers by hand. The file is left unchanged."
            ),
            Self::SourceHoldsMarker => format!(
                "registry/instructions/global.md{source} holds a line that reads as an orientation block marker, so it cannot be placed in the global instructions for {host}; remove that line."
            ),
            Self::OverrideFile => Self::describe_kind(FindingKind::CodexOverrideHidesAgents, Some(host)),
        }
    }
}

/// A finding about a host's global instructions file.
pub(crate) struct InstructionsSubject<'a> {
    pub(crate) host: HostKind,
    pub(crate) vault: Option<&'a str>,
    pub(crate) issue: InstructionsIssue,
}

impl From<&InstructionsSubject<'_>> for Finding {
    fn from(value: &InstructionsSubject<'_>) -> Self {
        let InstructionsSubject { host, vault, issue } = value;
        Self {
            kind: issue.kind(),
            host: Some(*host),
            definition: None,
            name: match issue {
                InstructionsIssue::OverrideFile => "AGENTS.override.md",
                _ => ORIENTATION_SOURCE,
            }
            .to_owned(),
            vault: vault.map(str::to_owned),
            other_vault: None,
            repair: issue.repair(),
            message: issue.message(*host, *vault),
        }
    }
}

#[cfg(test)]
#[path = "report_test.rs"]
mod tests;
