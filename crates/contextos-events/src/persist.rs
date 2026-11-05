use std::io::ErrorKind;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::EventsError;

/// Reads a JSON state file; `None` when it does not exist.
///
/// # Errors
///
/// Returns [`EventsError::TableUnreadable`] when the file cannot be read or is not the expected shape.
pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, EventsError> {
    let unreadable = |reason: String| EventsError::TableUnreadable { reason };
    let text = match contextos_fs::read_regular_text(path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unreadable(error.to_string())),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|error| unreadable(error.to_string()))
}

/// Writes a JSON state file atomically, creating its folder when missing.
///
/// # Errors
///
/// Returns [`EventsError::TableUnwritable`] when the value cannot be encoded or written; `path` is unchanged.
pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), EventsError> {
    let unwritable = |reason: String| EventsError::TableUnwritable { reason };
    let bytes = serde_json::to_vec(value).map_err(|error| unwritable(error.to_string()))?;
    contextos_fs::host_file::write_file_atomically(path, &bytes).map_err(|error| unwritable(error.to_string()))
}
