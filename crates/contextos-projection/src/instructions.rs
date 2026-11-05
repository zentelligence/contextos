//! Keeps the orientation block of each host's global instructions file in step with the vaults.

use contextos_core::OperationWarning;

use crate::instructions_observer::InstructionsObserver;
use crate::report::{InstructionsIssue, InstructionsSubject, ORIENTATION_SOURCE};
use crate::{
    DefinitionSource, Failure, Finding, HostKind, HostProjection, InstructionsFile, Mode, OrientationBlock,
    ProjectionError, ProjectionReport, SourceVault,
};

/// How many times the planned bytes are recomputed because the operator saved the file meanwhile.
const MAX_ATTEMPTS: u8 = 3;

/// What an instructions file needs, given what it holds.
enum Planned {
    /// It already holds what it should.
    Unchanged,
    /// It must become `bytes`, which `issue` explains.
    Write { bytes: Vec<u8>, issue: InstructionsIssue },
    /// It cannot be updated: its block is malformed or the source holds a marker.
    Refused(Box<(ProjectionError, Finding)>),
}

/// One pass over the hosts' global instructions files.
pub(crate) struct InstructionsPass<'a> {
    pub(crate) source: &'a dyn DefinitionSource,
    pub(crate) vaults: &'a [SourceVault],
    pub(crate) hosts: Vec<&'a HostProjection>,
    pub(crate) mode: Mode,
    pub(crate) observer: &'a dyn InstructionsObserver,
}

/// The source of the orientation: the first configured vault that has the file.
struct Orientation {
    vault: String,
    content: Vec<u8>,
}

/// Records that the orientation block of `host` could not be changed.
fn failed(report: &mut ProjectionReport, host: HostKind, code: &str) {
    report.failures.push(Failure {
        host,
        definition: None,
        name: ORIENTATION_SOURCE.to_owned(),
        code: code.to_owned(),
    });
}

fn warning(error: &ProjectionError, message: String) -> OperationWarning {
    OperationWarning {
        code: error.code().to_owned(),
        message,
    }
}

impl InstructionsPass<'_> {
    pub(crate) fn execute(&self, report: &mut ProjectionReport) {
        let hosts: Vec<(HostKind, &InstructionsFile)> = self
            .hosts
            .iter()
            .filter_map(|host| host.instructions().map(|file| (host.kind(), file)))
            .collect();
        for (host, file) in &hosts {
            if *host == HostKind::Codex && std::fs::symlink_metadata(file.override_file()).is_ok() {
                report.findings.push(Finding::from(&InstructionsSubject {
                    host: *host,
                    vault: None,
                    issue: InstructionsIssue::OverrideFile,
                }));
            }
        }
        // With no vault configured nothing can be said about the orientation, so no block is ever removed.
        if hosts.is_empty() || self.vaults.is_empty() {
            return;
        }
        let orientation = match self.orientation() {
            Ok(orientation) => orientation,
            Err(failure) => {
                let (vault, error) = *failure;
                // With one vault unreadable, which vault wins cannot be known, so every block is left as it is.
                report.warnings.push(warning(
                    &error,
                    format!(
                        "Could not read the global instructions in vault `{vault}`; the orientation blocks are unchanged. {}",
                        error.remediation()
                    ),
                ));
                return;
            }
        };
        for (host, file) in hosts {
            self.reconcile(host, file, orientation.as_ref(), report);
        }
    }

    /// The first vault's orientation, or the vault that could not be read and why. Boxed because a `PathBuf` is wider
    /// on Windows, which takes the unboxed pair past `clippy::result_large_err` there and nowhere else.
    fn orientation(&self) -> Result<Option<Orientation>, Box<(String, ProjectionError)>> {
        for vault in self.vaults {
            match self.source.read_orientation(vault) {
                Ok(Some(content)) => {
                    return Ok(Some(Orientation {
                        vault: vault.name.clone(),
                        content,
                    }));
                }
                Ok(None) => {}
                Err(error) => return Err(Box::new((vault.name.clone(), error))),
            }
        }
        Ok(None)
    }

    /// What the file must become to hold the orientation (or none) given what it holds now.
    fn plan(
        host: HostKind,
        vault: Option<&str>,
        orientation: Option<&Orientation>,
        existing: Option<&[u8]>,
    ) -> Planned {
        let wanted = match orientation {
            Some(orientation) => OrientationBlock::place(existing, &orientation.content),
            None => match existing {
                Some(existing) => OrientationBlock::withdraw(existing),
                None => return Planned::Unchanged,
            },
        };
        match wanted {
            Ok(bytes) if existing == Some(bytes.as_slice()) => Planned::Unchanged,
            Ok(bytes) => Planned::Write {
                bytes,
                issue: if orientation.is_some() {
                    InstructionsIssue::Differs
                } else {
                    InstructionsIssue::Orphaned
                },
            },
            Err(error) => {
                let issue = match &error {
                    ProjectionError::MalformedBlock { reason } => InstructionsIssue::Malformed(*reason),
                    _ => InstructionsIssue::SourceHoldsMarker,
                };
                let finding = Finding::from(&InstructionsSubject { host, vault, issue });
                Planned::Refused(Box::new((error, finding)))
            }
        }
    }

    fn reconcile(
        &self,
        host: HostKind,
        file: &InstructionsFile,
        orientation: Option<&Orientation>,
        report: &mut ProjectionReport,
    ) {
        let unreadable = |error: &ProjectionError, report: &mut ProjectionReport| {
            failed(report, host, error.code());
            report.warnings.push(warning(
                error,
                format!(
                    "Could not read the global instructions file for {host}, so its orientation block is unchanged. {}",
                    error.remediation()
                ),
            ));
        };
        let mut existing = match file.read() {
            Ok(existing) => existing,
            Err(error) => return unreadable(&error, report),
        };
        let vault = orientation.map(|orientation| orientation.vault.as_str());
        let mut attempts = 0_u8;
        let (bytes, issue) = loop {
            let (bytes, issue) = match Self::plan(host, vault, orientation, existing.as_deref()) {
                Planned::Write { bytes, issue } => (bytes, issue),
                Planned::Unchanged => return,
                Planned::Refused(refusal) => {
                    let (error, finding) = *refusal;
                    if matches!(self.mode, Mode::Event | Mode::Refresh) {
                        report.warnings.push(warning(&error, finding.message.clone()));
                    }
                    report.findings.push(finding);
                    return;
                }
            };
            if !self.mode.performs() {
                break (bytes, issue);
            }
            self.observer.reached(host, crate::InstructionsStep::Planned);
            // The operator may save the file between the read and the replace; the replace would discard that edit, so
            // read again and work out the bytes from what is there now.
            let current = match file.read() {
                Ok(current) => current,
                Err(error) => return unreadable(&error, report),
            };
            if current == existing {
                break (bytes, issue);
            }
            attempts = attempts.saturating_add(1);
            if attempts >= MAX_ATTEMPTS {
                failed(report, host, "projection/write-failed");
                report.warnings.push(OperationWarning {
                    code: "projection/write-failed".to_owned(),
                    message: format!(
                        "The global instructions file for {host} kept changing while the orientation block was being \
                         written, so it was left as it is. It will be tried again at the next change or start-up."
                    ),
                });
                return;
            }
            existing = current;
        };
        report
            .findings
            .push(Finding::from(&InstructionsSubject { host, vault, issue }));
        if !self.mode.performs() {
            return;
        }
        tracing::debug!(host = %host, "orientation block written");
        if let Err(error) = file.write(&bytes) {
            failed(report, host, error.code());
            report.warnings.push(warning(
                &error,
                format!(
                    "Could not write the orientation block to the global instructions file for {host}. {}",
                    error.remediation()
                ),
            ));
        }
    }
}
