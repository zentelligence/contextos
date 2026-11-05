//! What an enabling did beyond the job itself: which predecessor it retired, and how the workflow's `current` and
//! `previous` releases moved. The command line prints it as text or JSON and a page shows it as text.

use std::fmt;

use contextos_workflows::{Enabled, ReleaseMove};
use serde::Serialize;

/// One release pointer's old and new version. `None` is no release.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VersionMove {
    pub from: Option<String>,
    pub to: Option<String>,
}

/// What an enabling did beyond the job.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EnablingReport {
    /// The cron expression the job now has.
    pub schedule: String,
    /// The key of the predecessor the enabling retired, when it retired one.
    pub retired: Option<String>,
    /// How `current` moved, when it did.
    pub current: Option<VersionMove>,
    /// How `previous` moved, when `current` did.
    pub previous: Option<VersionMove>,
}

impl From<&Enabled> for EnablingReport {
    fn from(enabled: &Enabled) -> Self {
        let (current, previous) = match &enabled.moved {
            Some(ReleaseMove {
                current_from,
                current_to,
                previous_from,
                previous_to,
            }) => (
                Some(VersionMove {
                    from: current_from.as_ref().map(ToString::to_string),
                    to: Some(current_to.to_string()),
                }),
                Some(VersionMove {
                    from: previous_from.as_ref().map(ToString::to_string),
                    to: previous_to.as_ref().map(ToString::to_string),
                }),
            ),
            None => (None, None),
        };
        Self {
            schedule: enabled.schedule.as_str().to_owned(),
            retired: enabled.retired.clone(),
            current,
            previous,
        }
    }
}

impl fmt::Display for VersionMove {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let word = |version: &Option<String>| version.clone().unwrap_or_else(|| "none".to_owned());
        write!(formatter, "from {} to {}", word(&self.from), word(&self.to))
    }
}

impl fmt::Display for EnablingReport {
    /// The sentences that follow "the job is now enabled": the schedule, the predecessor retired, and the releases moved.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Its schedule is {}.", self.schedule)?;
        if let Some(retired) = &self.retired {
            write!(formatter, " Its predecessor {retired} was retired.")?;
        }
        if let (Some(current), Some(previous)) = (&self.current, &self.previous) {
            write!(formatter, " Current moved {current}; previous moved {previous}.")?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "enabling_report_test.rs"]
mod tests;
