use crate::{DefinitionRejection, ProjectionError};

/// A vendor-neutral model class an agent definition asks for; each host's configuration maps it to a concrete model.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Tier {
    /// Quick, inexpensive work.
    Fast,
    /// The everyday default.
    Balanced,
    /// The most capable model, for hard problems.
    Deep,
}

impl TryFrom<&str> for Tier {
    type Error = ProjectionError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "fast" => Ok(Self::Fast),
            "balanced" => Ok(Self::Balanced),
            "deep" => Ok(Self::Deep),
            _ => Err(ProjectionError::InvalidDefinition {
                field: "tier",
                reason: DefinitionRejection::Unrecognised,
            }),
        }
    }
}

/// Which tools an agent may use.
///
/// Non-exhaustive so a list of tool patterns can join `all` and `read-only` without breaking callers.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Access {
    /// Whatever the session offers.
    All,
    /// Tools that read but never change state.
    ReadOnly,
}

impl TryFrom<&str> for Access {
    type Error = ProjectionError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "all" => Ok(Self::All),
            "read-only" => Ok(Self::ReadOnly),
            _ => Err(ProjectionError::InvalidDefinition {
                field: "access",
                reason: DefinitionRejection::Unrecognised,
            }),
        }
    }
}

#[cfg(test)]
#[path = "tier_test.rs"]
mod tests;
