use serde::{Serialize, Serializer};

use crate::device_name::is_reserved_device;
use crate::{NameRejection, ProjectionError};

/// The longest slug, in bytes.
const MAX_BYTES: usize = 64;

/// A definition name safe to use as a file or folder name on every platform: lowercase ASCII letters and digits, with
/// single hyphens between them, at most 64 bytes (`^[a-z0-9]+(-[a-z0-9]+)*$`), and not a Windows device name.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Slug(String);

fn validate(value: &str) -> Result<(), NameRejection> {
    if value.is_empty() {
        return Err(NameRejection::Empty);
    }
    if value.len() > MAX_BYTES {
        return Err(NameRejection::TooLong { maximum: MAX_BYTES });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(NameRejection::InvalidCharacter);
    }
    if value.starts_with('-') || value.ends_with('-') || value.contains("--") {
        return Err(NameRejection::MisplacedHyphen);
    }
    if is_reserved_device(value) {
        return Err(NameRejection::ReservedDeviceName);
    }
    Ok(())
}

impl TryFrom<&str> for Slug {
    type Error = ProjectionError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        validate(value).map_err(|reason| ProjectionError::InvalidSlug { reason })?;
        Ok(Self(value.to_owned()))
    }
}

impl<'a> From<&'a Slug> for &'a str {
    fn from(value: &'a Slug) -> Self {
        &value.0
    }
}

impl Serialize for Slug {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

#[cfg(test)]
#[path = "slug_test.rs"]
mod tests;
