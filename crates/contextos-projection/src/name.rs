use crate::{NameRejection, ProjectionError};

/// The longest file extension, in bytes.
const MAX_BYTES: usize = 16;

/// A file extension without its dot: lowercase ASCII letters and digits, at most 16 bytes (for example `md`).
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FileExtension(String);

impl TryFrom<&str> for FileExtension {
    type Error = ProjectionError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let reason = if value.is_empty() {
            Some(NameRejection::Empty)
        } else if value.len() > MAX_BYTES {
            Some(NameRejection::TooLong { maximum: MAX_BYTES })
        } else if !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            Some(NameRejection::InvalidCharacter)
        } else {
            None
        };
        match reason {
            Some(reason) => Err(ProjectionError::InvalidExtension { reason }),
            None => Ok(Self(value.to_owned())),
        }
    }
}

impl<'a> From<&'a FileExtension> for &'a str {
    fn from(value: &'a FileExtension) -> Self {
        &value.0
    }
}

#[cfg(test)]
#[path = "name_test.rs"]
mod tests;
