use std::path::Path;

use contextos_core::{ReportsCreation, VaultPath};
use time::OffsetDateTime;

use crate::Filesystem;

impl ReportsCreation for Filesystem {
    /// The birth time the filesystem records for the file. Not every platform and filesystem records one (older
    /// Linux kernels and some mounts do not), and a file removed after its path was validated has none; both report
    /// `None` rather than substituting the modification time, which would masquerade as a creation date.
    fn created_at(&self, path: &VaultPath) -> Option<OffsetDateTime> {
        std::fs::metadata(<&Path>::from(path))
            .ok()?
            .created()
            .ok()
            .map(OffsetDateTime::from)
    }
}
