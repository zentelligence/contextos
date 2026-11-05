//! What an install from source reports: lines of text, or one JSON document.

use std::fmt;

use crate::source_install_error::SourceInstallError;
use crate::vault_source_install::{SourceInstalled, SourceOutcome};

/// An install's result as text.
pub struct InstalledText<'a>(pub &'a SourceInstalled);

/// An install's result as one JSON document.
pub struct InstalledJson<'a>(pub &'a SourceInstalled);

/// A refusal as text, `refused {code}: {message}`.
pub struct RefusalText<'a>(pub &'a SourceInstallError);

/// A refusal as the `{"error":{"code":...,"message":...}}` document every command gives.
pub struct RefusalJson<'a>(pub &'a SourceInstallError);

impl fmt::Display for InstalledText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let installed = self.0;
        write!(
            formatter,
            "{} {} {} ({})",
            installed.release.outcome.as_str(),
            installed.release.package,
            installed.release.version,
            installed.kind,
        )?;
        if installed.removed_by_retention {
            formatter.write_str(", then removed again by retention because nothing keeps it")?;
        }
        write!(formatter, "\ndigest {}", installed.release.digest)?;
        for (workflow, standing) in &installed.standing {
            write!(formatter, "\n{workflow}: {standing}")?;
        }
        for requirement in &installed.requirements {
            write!(
                formatter,
                "\nrequires {} {}",
                requirement.library, requirement.requirement
            )?;
        }
        for library in &installed.libraries {
            write!(
                formatter,
                "\nlibrary {} {} {}",
                library.package,
                library.version,
                library.outcome.as_str()
            )?;
        }
        for warning in &installed.warnings {
            write!(formatter, "\nwarning {warning}")?;
        }
        Ok(())
    }
}

impl fmt::Display for InstalledJson<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The report is plain strings, which always serialise; the fallback keeps the result a document.
        match serde_json::to_string(self.0) {
            Ok(document) => formatter.write_str(&document),
            Err(_) => formatter.write_str(
                r#"{"error":{"code":"packages/store-unavailable","message":"the result could not be rendered"}}"#,
            ),
        }
    }
}

impl fmt::Display for RefusalText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            SourceInstallError::Invalid { failures } => {
                for (index, failure) in failures.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str("\n")?;
                    }
                    write!(
                        formatter,
                        "refused {} {}: {}",
                        failure.code.as_str(),
                        failure.artefact,
                        failure.message
                    )?;
                }
                Ok(())
            }
            other => write!(formatter, "refused {}: {other}", other.code()),
        }
    }
}

impl fmt::Display for RefusalJson<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let document = serde_json::json!({
            "error": { "code": self.0.code(), "message": self.0.to_string() }
        });
        write!(formatter, "{document}")
    }
}

impl SourceOutcome {
    /// The words an install reports a release's outcome with.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::AlreadyInstalled => "already installed",
            Self::Repaired => "restored",
            Self::Unchanged => "unchanged",
        }
    }
}

#[cfg(test)]
#[path = "source_install_report_test.rs"]
mod tests;
