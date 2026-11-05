//! A workflow's name: the folder it is kept in, in a package release and in a vault's source.
//!
//! It follows the rules a job key follows: lower-case letters, digits, and hyphens, never a name Windows reserves for a
//! device, and no longer than a package name, so it is one plain folder name on every platform.

use std::fmt;

use crate::package::PackageError;
use crate::package_name::{MAX_PACKAGE_NAME_LENGTH, is_device_name};

/// A checked workflow name.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkflowName(String);

impl WorkflowName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for WorkflowName {
    type Error = PackageError;

    fn try_from(raw: &str) -> Result<Self, PackageError> {
        let refuse = |reason: &'static str| PackageError::InvalidWorkflowName {
            name: raw.to_owned(),
            reason,
        };
        if raw.is_empty() {
            return Err(refuse("it is empty"));
        }
        if raw.len() > MAX_PACKAGE_NAME_LENGTH {
            return Err(refuse("it is longer than 64 characters"));
        }
        if !raw
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(refuse(
                "it holds a character other than a lower-case letter, a digit, or a hyphen",
            ));
        }
        if is_device_name(raw) {
            return Err(refuse("it is a name Windows reserves for a device"));
        }
        Ok(Self(raw.to_owned()))
    }
}

impl fmt::Display for WorkflowName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
#[path = "workflow_name_test.rs"]
mod tests;
