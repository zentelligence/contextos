use crate::HostKind;

/// A point in the update of a host's instructions file, reported to an [`InstructionsObserver`] once it is reached.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstructionsStep {
    /// The file was read and the new bytes were worked out from what was read; replacing the file is next.
    Planned,
}

/// Observes the update of a host's instructions file. Tests change the file here, between the read and the replace, so
/// the window an operator's own save can land in is exercised without relying on timing.
pub trait InstructionsObserver: Send + Sync {
    /// Called when `step` is reached for `host`'s instructions file.
    fn reached(&self, host: HostKind, step: InstructionsStep);
}

/// An observer that does nothing.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NoInstructionsObserver;

impl InstructionsObserver for NoInstructionsObserver {
    fn reached(&self, _host: HostKind, _step: InstructionsStep) {}
}
