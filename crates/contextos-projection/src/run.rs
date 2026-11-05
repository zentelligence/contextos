use std::collections::{BTreeSet, HashSet};

use contextos_core::{ContentHash, OperationWarning};
use contextos_fs::FsError;
use contextos_fs::host_file::{FolderReplaceObserver, NoFolderFaults};
use tracing::debug;

use crate::candidate::{Definition, Gathered, gather};
use crate::instructions::InstructionsPass;
use crate::instructions_observer::{InstructionsObserver, NoInstructionsObserver};
use crate::report::{Subject, invalid_definition_warning, invalid_name_finding, invalid_name_warning, warning_code};
use crate::{
    Access, Action, ConfigurationId, DefinitionKind, DefinitionName, DefinitionSource, Failure, Finding, FindingKind,
    HostFolder, HostKind, HostProjection, ManifestEntry, Mode, OwnershipManifest, Plan, PlanFinding, PlanInput,
    ProjectionError, ProjectionReport, Slug, SourceVault, TargetState,
};

/// Which definitions a run looks at.
pub(crate) enum Scope {
    /// Every definition in every vault, and every entry any host's manifest records.
    All,
    /// Exactly these slugs.
    Slugs {
        agents: BTreeSet<Slug>,
        skills: BTreeSet<Slug>,
        /// Whether the global instructions' orientation block is looked at too.
        orientation: bool,
    },
}

/// One folder's ownership manifest while a run changes it. `None` means it could not be read, so nothing in the
/// folder is touched: without the record, ownership cannot be told.
struct FolderState {
    manifest: Option<OwnershipManifest>,
    dirty: bool,
}

struct HostState<'a> {
    host: &'a HostProjection,
    agents: FolderState,
    skills: FolderState,
}

/// One pass over the definitions, for one purpose.
pub(crate) struct Run<'a> {
    source: &'a dyn DefinitionSource,
    configuration: &'a ConfigurationId,
    mode: Mode,
    vaults: Vec<SourceVault>,
    hosts: Vec<HostState<'a>>,
    report: ProjectionReport,
    raised: HashSet<(DefinitionKind, Slug)>,
    observer: &'a dyn InstructionsObserver,
    folders: &'a dyn FolderReplaceObserver,
}

fn warning_for(host: HostKind, definition: DefinitionKind, slug: &Slug, error: &ProjectionError) -> OperationWarning {
    OperationWarning {
        code: error.code().to_owned(),
        message: format!(
            "Could not project the {definition} `{}` to {host}. {}",
            <&str>::from(slug),
            error.remediation()
        ),
    }
}

impl<'a> Run<'a> {
    pub(crate) fn new(
        source: &'a dyn DefinitionSource,
        hosts: &'a [HostProjection],
        configuration: &'a ConfigurationId,
        mode: Mode,
    ) -> Self {
        let mut report = ProjectionReport::default();
        let hosts = hosts
            .iter()
            .map(|host| HostState {
                host,
                agents: load(host.kind(), DefinitionKind::Agent, host.agents(), &mut report),
                skills: load(host.kind(), DefinitionKind::Skill, host.skills(), &mut report),
            })
            .collect();
        Self {
            source,
            configuration,
            mode,
            vaults: source.vaults(),
            hosts,
            report,
            raised: HashSet::new(),
            observer: &NoInstructionsObserver,
            folders: &NoFolderFaults,
        }
    }

    /// The same run, reporting each step of an instructions file update to `observer`.
    pub(crate) fn observed_by(self, observer: &'a dyn InstructionsObserver) -> Self {
        Self { observer, ..self }
    }

    /// The same run, reporting each step of a skill folder swap to `folders`.
    pub(crate) fn with_folder_observer(self, folders: &'a dyn FolderReplaceObserver) -> Self {
        Self { folders, ..self }
    }

    /// Plans every definition in scope for every host and, unless only assessing, performs what was planned.
    pub(crate) fn execute(mut self, scope: Scope) -> ProjectionReport {
        let (agents, skills, orientation) = match scope {
            Scope::Slugs {
                agents,
                skills,
                orientation,
            } => (agents, skills, orientation),
            Scope::All => (
                self.every_slug(DefinitionKind::Agent),
                self.every_slug(DefinitionKind::Skill),
                true,
            ),
        };
        for (kind, slugs) in [(DefinitionKind::Agent, agents), (DefinitionKind::Skill, skills)] {
            for slug in slugs {
                self.reconcile(kind, &slug);
            }
        }
        self.save_manifests();
        if orientation {
            InstructionsPass {
                source: self.source,
                vaults: &self.vaults,
                hosts: self.hosts.iter().map(|state| state.host).collect(),
                mode: self.mode,
                observer: self.observer,
            }
            .execute(&mut self.report);
        }
        self.report
    }

    /// Every slug any vault defines or any host's manifest records.
    fn every_slug(&mut self, kind: DefinitionKind) -> BTreeSet<Slug> {
        let mut slugs = BTreeSet::new();
        for vault in &self.vaults {
            let names = match kind {
                DefinitionKind::Agent => self.source.agent_names(vault),
                DefinitionKind::Skill => self.source.skill_names(vault),
            };
            match names {
                Ok(names) => {
                    for name in names {
                        match name {
                            DefinitionName::Valid(slug) => {
                                slugs.insert(slug);
                            }
                            DefinitionName::Invalid { raw, reason } => {
                                self.report
                                    .findings
                                    .push(invalid_name_finding(kind, &vault.name, &raw, reason));
                                if matches!(self.mode, Mode::Event | Mode::Refresh) {
                                    self.report
                                        .warnings
                                        .push(invalid_name_warning(kind, &vault.name, &raw, reason));
                                }
                            }
                        }
                    }
                }
                Err(error) => self.report.warnings.push(OperationWarning {
                    code: error.code().to_owned(),
                    message: format!(
                        "Could not list the {kind} definitions in vault `{}`. {}",
                        vault.name,
                        error.remediation()
                    ),
                }),
            }
        }
        for state in &self.hosts {
            let folder = match kind {
                DefinitionKind::Agent => &state.agents,
                DefinitionKind::Skill => &state.skills,
            };
            if let Some(manifest) = &folder.manifest {
                slugs.extend(manifest.iter().map(|(slug, _)| slug.clone()));
            }
        }
        slugs
    }

    fn reconcile(&mut self, kind: DefinitionKind, slug: &Slug) {
        let gathered = match gather(self.source, &self.vaults, kind, slug) {
            Ok(gathered) => gathered,
            Err(error) => {
                // With one vault unreadable, which vault wins cannot be known, so the slug is left as it is.
                self.report.warnings.push(OperationWarning {
                    code: error.code().to_owned(),
                    message: format!(
                        "Could not read the {kind} definition `{}`; its projection is unchanged. {}",
                        <&str>::from(slug),
                        error.remediation()
                    ),
                });
                return;
            }
        };
        for index in 0..self.hosts.len() {
            self.reconcile_host(index, kind, slug, &gathered);
        }
    }

    fn reconcile_host(&mut self, index: usize, kind: DefinitionKind, slug: &Slug, gathered: &Gathered) {
        let Self {
            hosts,
            report,
            raised,
            configuration,
            mode,
            folders,
            ..
        } = self;
        let state = &mut hosts[index];
        let host = state.host;
        let (folder, folder_state) = match kind {
            DefinitionKind::Agent => (host.agents(), &mut state.agents),
            DefinitionKind::Skill => (host.skills(), &mut state.skills),
        };
        let (Some(folder), Some(manifest)) = (folder, folder_state.manifest.as_mut()) else {
            return;
        };
        let fail = |report: &mut ProjectionReport, error: &ProjectionError| {
            report.warnings.push(warning_for(host.kind(), kind, slug, error));
            report.failures.push(Failure {
                host: host.kind(),
                definition: Some(kind),
                name: <&str>::from(slug).to_owned(),
                code: error.code().to_owned(),
            });
        };

        let rendered = match render(host, gathered) {
            Ok(rendered) => rendered,
            Err(error) => return fail(report, &error),
        };
        let target = match target_state(folder, kind, slug, host, *mode) {
            Ok(target) => target,
            Err(error) => return fail(report, &error),
        };
        let projected = rendered.as_ref().map(Rendered::hash);
        let owner = manifest
            .get(slug)
            .filter(|entry| entry.configuration() != *configuration)
            .map(|entry| owner_prefix(entry.configuration()));
        let plan = Plan::from(PlanInput {
            candidates: &gathered.candidates,
            target: &target,
            entry: manifest.get(slug),
            configuration,
            projected: projected.as_ref(),
            mode: *mode,
        });
        report_findings(
            report,
            raised,
            &plan,
            &FindingContext {
                host: host.kind(),
                kind,
                slug,
                mode: *mode,
                owner: owner.as_deref(),
            },
        );
        read_only_finding(report, host.kind(), kind, slug, gathered);
        let hand_edit_replaced = *mode == Mode::Event
            && matches!(plan.action, Some(Action::Write(_)))
            && plan
                .findings
                .iter()
                .any(|finding| finding.kind == FindingKind::ProjectionHandEdited);
        let Some(action) = plan.action else {
            return;
        };
        if !mode.performs() {
            return;
        }
        debug!(host = %host.kind(), definition = %kind, slug = <&str>::from(slug), action = <&str>::from(&action), "projection action");
        let job = Job {
            folder,
            kind,
            slug,
            host,
            gathered,
            rendered: rendered.as_ref(),
            configuration,
            folders: *folders,
        };
        match job.perform(&action, manifest) {
            Ok(warning) => {
                folder_state.dirty = true;
                report.warnings.extend(warning);
                if hand_edit_replaced {
                    report
                        .warnings
                        .push(hand_edit_replaced_warning(host.kind(), kind, slug));
                }
            }
            Err(error) => fail(report, &error),
        }
    }

    fn save_manifests(&mut self) {
        for state in &self.hosts {
            for (folder, folder_state, kind) in [
                (state.host.agents(), &state.agents, DefinitionKind::Agent),
                (state.host.skills(), &state.skills, DefinitionKind::Skill),
            ] {
                let (Some(folder), Some(manifest)) =
                    (folder, folder_state.manifest.as_ref().filter(|_| folder_state.dirty))
                else {
                    continue;
                };
                if let Err(error) = manifest.write(folder.manifest(), folder.target()) {
                    self.report.warnings.push(OperationWarning {
                        code: error.code().to_owned(),
                        message: format!(
                            "Could not save the ownership record of the {kind} folder for {}. {}",
                            state.host.kind(),
                            error.remediation()
                        ),
                    });
                }
            }
        }
    }
}

/// The warning code for a hand edit that a write replaced because the source definition changed.
const HAND_EDITED_REPLACED: &str = "projection/hand-edited-replaced";

fn hand_edit_replaced_warning(host: HostKind, kind: DefinitionKind, slug: &Slug) -> OperationWarning {
    OperationWarning {
        code: HAND_EDITED_REPLACED.to_owned(),
        message: format!(
            "The {kind} `{}` for {host} had been edited outside ContextOS and was replaced because its source definition changed.",
            <&str>::from(slug)
        ),
    }
}

/// How many leading hexadecimal characters of a configuration's identity name it in a message.
const OWNER_PREFIX_CHARS: usize = 12;

fn owner_prefix(configuration: &ConfigurationId) -> String {
    <&str>::from(configuration).chars().take(OWNER_PREFIX_CHARS).collect()
}

impl From<&Action> for &'static str {
    fn from(action: &Action) -> Self {
        match action {
            Action::Write(_) => "write",
            Action::Adopt(_) => "adopt",
            Action::Withdraw => "withdraw",
        }
    }
}

/// Where a plan's findings are reported from.
struct FindingContext<'a> {
    host: HostKind,
    kind: DefinitionKind,
    slug: &'a Slug,
    mode: Mode,
    owner: Option<&'a str>,
}

/// Adds the plan's findings to the report and, on a write or refresh, the warnings that go with them.
fn report_findings(
    report: &mut ProjectionReport,
    raised: &mut HashSet<(DefinitionKind, Slug)>,
    plan: &Plan,
    context: &FindingContext<'_>,
) {
    let FindingContext {
        host,
        kind,
        slug,
        mode,
        owner,
    } = *context;
    for finding in &plan.findings {
        let subject = subject(host, kind, slug, finding, owner);
        report.findings.push(Finding::from(&subject));
        if matches!(mode, Mode::Event | Mode::Refresh)
            && let Some(code) = warning_code(finding.kind)
        {
            if finding.kind == FindingKind::ProjectionInvalidDefinition {
                if raised.insert((kind, slug.clone())) {
                    report.warnings.push(invalid_definition_warning(&subject));
                }
            } else {
                report.warnings.push(OperationWarning {
                    code: code.to_owned(),
                    message: Finding::from(&subject).message,
                });
            }
        }
    }
}

/// A Codex copy of a read-only agent cannot enforce read-only access, which `doctor` says.
fn read_only_finding(
    report: &mut ProjectionReport,
    host: HostKind,
    kind: DefinitionKind,
    slug: &Slug,
    gathered: &Gathered,
) {
    if let (Some(Definition::Agent(definition)), HostKind::Codex) = (gathered.winner(), host)
        && *definition.access() == Access::ReadOnly
        && let Some(vault) = winner_vault(gathered)
    {
        report.findings.push(Finding::from(&Subject {
            kind: FindingKind::CodexReadOnlyUnenforced,
            host: Some(host),
            definition: kind,
            name: <&str>::from(slug),
            vault: Some(vault),
            other_vault: None,
            rejection: None,
            owner: None,
        }));
    }
}

fn winner_vault(gathered: &Gathered) -> Option<&str> {
    gathered.candidates.iter().find_map(|candidate| match candidate {
        crate::Candidate::Valid { vault, .. } => Some(vault.as_str()),
        _ => None,
    })
}

fn subject<'a>(
    host: HostKind,
    kind: DefinitionKind,
    slug: &'a Slug,
    finding: &'a PlanFinding,
    owner: Option<&'a str>,
) -> Subject<'a> {
    Subject {
        kind: finding.kind,
        host: Some(host),
        definition: kind,
        name: <&str>::from(slug),
        vault: finding.vault.as_deref(),
        other_vault: finding.other_vault.as_deref(),
        rejection: finding.rejection,
        owner,
    }
}

fn load(
    host: HostKind,
    kind: DefinitionKind,
    folder: Option<&HostFolder>,
    report: &mut ProjectionReport,
) -> FolderState {
    // A folder that does not exist has nothing projected into it: its entries are left unassessed, silently.
    let Some(folder) = folder else {
        return FolderState {
            manifest: None,
            dirty: false,
        };
    };
    match OwnershipManifest::read(folder.manifest(), folder.target()) {
        Ok(manifest) => FolderState {
            manifest: Some(manifest),
            dirty: false,
        },
        Err(error) => {
            report.warnings.push(OperationWarning {
                code: error.code().to_owned(),
                message: format!(
                    "Could not read the ownership record of the {kind} folder for {host}, so it is left untouched. {}",
                    error.remediation()
                ),
            });
            FolderState {
                manifest: None,
                dirty: false,
            }
        }
    }
}

/// What the winning definition renders to for one host.
enum Rendered {
    Agent(crate::host::RenderedAgent),
    Skill(ContentHash),
}

impl Rendered {
    fn hash(&self) -> ContentHash {
        match self {
            Self::Agent(agent) => agent.hash.clone(),
            Self::Skill(hash) => hash.clone(),
        }
    }
}

fn render(host: &HostProjection, gathered: &Gathered) -> Result<Option<Rendered>, ProjectionError> {
    Ok(match gathered.winner() {
        None => None,
        Some(Definition::Agent(definition)) => Some(Rendered::Agent(host.render().render(definition)?)),
        Some(Definition::Skill(definition)) => Some(Rendered::Skill(ContentHash::from(definition.folder()))),
    })
}

fn target_state(
    folder: &HostFolder,
    kind: DefinitionKind,
    slug: &Slug,
    host: &HostProjection,
    mode: Mode,
) -> Result<TargetState, ProjectionError> {
    let target = folder.target();
    let hash = match kind {
        DefinitionKind::Agent => target.agent_file_hash(slug, &host.render().extension()?)?,
        DefinitionKind::Skill => {
            if mode.performs() {
                target.recover_skill_folder(slug)?;
            }
            target.read_skill_folder(slug)?.map(|skill| ContentHash::from(&skill))
        }
    };
    Ok(hash.map_or(TargetState::Absent, TargetState::Present))
}

/// Everything needed to carry out a planned action on one definition in one folder.
struct Job<'a> {
    folder: &'a HostFolder,
    kind: DefinitionKind,
    slug: &'a Slug,
    host: &'a HostProjection,
    gathered: &'a Gathered,
    rendered: Option<&'a Rendered>,
    configuration: &'a ConfigurationId,
    folders: &'a dyn FolderReplaceObserver,
}

impl Job<'_> {
    /// Carries out `action` and updates `manifest` to match. Nothing is recorded unless the change itself succeeded.
    ///
    /// Returns a warning when the change stands but left something behind for the next run to remove.
    fn perform(
        &self,
        action: &Action,
        manifest: &mut OwnershipManifest,
    ) -> Result<Option<OperationWarning>, ProjectionError> {
        let target = self.folder.target();
        match action {
            Action::Withdraw => {
                match self.kind {
                    DefinitionKind::Agent => target.remove_agent_file(self.slug, &self.host.render().extension()?)?,
                    DefinitionKind::Skill => target.remove_skill_folder(self.slug)?,
                };
                manifest.remove(self.slug);
            }
            Action::Adopt(recording) => {
                let recorded = entry(recording, recording.projected_hash.clone(), self.configuration)?;
                manifest.insert(self.slug.clone(), recorded);
            }
            Action::Write(recording) => {
                let written = match (self.gathered.winner(), self.rendered) {
                    (Some(Definition::Agent(_)), Some(Rendered::Agent(agent))) => {
                        target.write_agent_file(self.slug, &self.host.render().extension()?, &agent.bytes)?
                    }
                    (Some(Definition::Skill(definition)), _) => {
                        target.recover_skill_folder(self.slug)?;
                        match target.replace_skill_folder_observed(self.slug, definition.folder(), self.folders) {
                            Ok(written) => written,
                            Err(ProjectionError::Write(FsError::ReplaceCleanup { .. })) => {
                                manifest.insert(
                                    self.slug.clone(),
                                    entry(recording, ContentHash::from(definition.folder()), self.configuration)?,
                                );
                                return Ok(Some(self.cleanup_warning()));
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    _ => return Ok(None),
                };
                manifest.insert(self.slug.clone(), entry(recording, written, self.configuration)?);
            }
        }
        Ok(None)
    }

    fn cleanup_warning(&self) -> OperationWarning {
        OperationWarning {
            code: "projection/cleanup-incomplete".to_owned(),
            message: format!(
                "The {} `{}` was projected to {}, but the previous copy could not be removed. The next run removes it.",
                self.kind,
                <&str>::from(self.slug),
                self.host.kind()
            ),
        }
    }
}

fn entry(
    recording: &crate::Recording,
    projected: ContentHash,
    configuration: &ConfigurationId,
) -> Result<ManifestEntry, ProjectionError> {
    ManifestEntry::new(
        &recording.vault,
        &recording.source_path,
        recording.source_hash.clone(),
        projected,
        configuration.clone(),
    )
}
