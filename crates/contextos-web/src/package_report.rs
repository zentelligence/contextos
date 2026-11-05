//! What `contextos-web packages install` prints.

use std::fmt;

use crate::package_store::Installed;

/// What an install prints.
pub struct InstalledReport<'a>(pub &'a Installed);

impl fmt::Display for InstalledReport<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let identity = &self.0.identity;
        if self.0.removed_by_retention {
            writeln!(
                formatter,
                "Installed {} {}, then retention removed it again because nothing keeps it.",
                identity.name, identity.version
            )?;
        } else {
            writeln!(formatter, "Installed {} {}.", identity.name, identity.version)?;
        }
        for warning in &self.0.warnings {
            writeln!(formatter, "Warning: {warning}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "package_report_test.rs"]
mod tests;
