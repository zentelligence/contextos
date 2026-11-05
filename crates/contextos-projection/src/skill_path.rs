use crate::device_name::is_reserved_device;
use crate::{NameRejection, ProjectionError};

/// A file's path relative to its skill folder, with `/` separators and validated components.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SkillFilePath(String);

/// The longest whole path, in bytes.
const MAX_PATH_BYTES: usize = 1024;
/// The longest single component, in bytes (the common file system limit).
const MAX_COMPONENT_BYTES: usize = 255;

fn validate(value: &str) -> Result<(), NameRejection> {
    if value.is_empty() {
        return Err(NameRejection::Empty);
    }
    if value.len() > MAX_PATH_BYTES {
        return Err(NameRejection::TooLong {
            maximum: MAX_PATH_BYTES,
        });
    }
    if value.starts_with('/') {
        return Err(NameRejection::Absolute);
    }
    if value.contains('\\') {
        return Err(NameRejection::Backslash);
    }
    if value.contains(':') {
        return Err(NameRejection::Colon);
    }
    if value.chars().any(char::is_control) {
        return Err(NameRejection::ControlCharacter);
    }
    value.split('/').try_for_each(validate_component)
}

fn validate_component(component: &str) -> Result<(), NameRejection> {
    if component.is_empty() {
        return Err(NameRejection::EmptyComponent);
    }
    if component == "." || component == ".." {
        return Err(NameRejection::DotComponent);
    }
    if component.len() > MAX_COMPONENT_BYTES {
        return Err(NameRejection::TooLong {
            maximum: MAX_COMPONENT_BYTES,
        });
    }
    if component.ends_with(['.', ' ']) {
        return Err(NameRejection::TrailingDotOrSpace);
    }
    if is_reserved_device(component) {
        return Err(NameRejection::ReservedDeviceName);
    }
    Ok(())
}

impl TryFrom<&str> for SkillFilePath {
    type Error = ProjectionError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        validate(value).map_err(|reason| ProjectionError::InvalidSkillPath { reason })?;
        Ok(Self(value.to_owned()))
    }
}

impl<'a> From<&'a SkillFilePath> for &'a str {
    fn from(value: &'a SkillFilePath) -> Self {
        &value.0
    }
}

#[cfg(test)]
#[path = "skill_path_test.rs"]
mod tests;
