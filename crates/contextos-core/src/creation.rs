use time::OffsetDateTime;

use crate::VaultPath;

/// Reports when a vault file was first created, from a source outside the file's own content (version history,
/// filesystem metadata).
///
/// `None` means the source cannot say, never an error: the caller chooses its own fallback, so a source cannot
/// invent a date that would later be mistaken for a real one.
pub trait ReportsCreation: Send + Sync {
    fn created_at(&self, path: &VaultPath) -> Option<OffsetDateTime>;
}

/// A source that never knows a creation date.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCreationReports;

impl ReportsCreation for NoCreationReports {
    fn created_at(&self, _path: &VaultPath) -> Option<OffsetDateTime> {
        None
    }
}

/// Two sources in order of trust: `fallback` is asked only when `primary` cannot say.
#[derive(Clone, Copy, Debug)]
pub struct FirstReportedCreation<A, B> {
    pub primary: A,
    pub fallback: B,
}

impl<A, B> ReportsCreation for FirstReportedCreation<A, B>
where
    A: ReportsCreation,
    B: ReportsCreation,
{
    fn created_at(&self, path: &VaultPath) -> Option<OffsetDateTime> {
        self.primary.created_at(path).or_else(|| self.fallback.created_at(path))
    }
}

/// A source that may not exist for a given vault (Git, say, where a vault has versioning disabled).
impl<T> ReportsCreation for Option<T>
where
    T: ReportsCreation,
{
    fn created_at(&self, path: &VaultPath) -> Option<OffsetDateTime> {
        self.as_ref().and_then(|source| source.created_at(path))
    }
}
