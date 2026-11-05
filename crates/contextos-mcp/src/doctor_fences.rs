//! Whether a vault's fence vocabulary (`.contextos/fences.toml`) can be used. The renderer falls back to the
//! vocabulary it carries when it cannot, so a broken manifest never breaks a page; this check is where it is reported.

use contextos_core::{VaultPath, VaultPathInput, VaultSet};
use contextos_fs::{Filesystem, FsError, ReadTextRequest};
use contextos_markdown::FenceVocabulary;

use crate::doctor::{DoctorCheck, DoctorError};
use crate::doctor_kind::DoctorKind;

/// Where a vault declares its fence vocabulary.
const FENCES_PATH: &str = ".contextos/fences.toml";

/// Passes when the vault has no manifest (the built-in vocabulary applies) or one that can be used; fails, never
/// auto-fixably, when the manifest is malformed or newer than this build reads, naming why. The file is the operator's
/// to correct: rewriting it could discard a vocabulary they chose.
pub(crate) fn fence_vocabulary_check(
    root: &contextos_core::VaultRoot,
    roots: &VaultSet,
    filesystem: &Filesystem,
) -> Result<DoctorCheck, DoctorError> {
    let absolute = root.path().join(FENCES_PATH);
    let path = VaultPath::try_from(VaultPathInput {
        roots,
        raw: absolute
            .to_str()
            .ok_or_else(|| DoctorError::NonUtf8VaultPath(absolute.clone()))?,
    })?;
    let manifest = match filesystem.read_text(&ReadTextRequest { path, limit: None }) {
        Ok(read) => read.content,
        Err(FsError::NotFound { .. }) => {
            return Ok(DoctorCheck::pass(
                DoctorKind::FenceVocabulary,
                "no .contextos/fences.toml; the built-in vocabulary applies",
            ));
        }
        Err(other) => return Err(DoctorError::from(other)),
    };
    Ok(match FenceVocabulary::try_from(manifest.as_str()) {
        Ok(vocabulary) => DoctorCheck::pass(
            DoctorKind::FenceVocabulary,
            match vocabulary.standard() {
                Some(standard) => format!("the vault's vocabulary ({standard})"),
                None => "the vault's vocabulary".to_owned(),
            },
        ),
        Err(error) => DoctorCheck::fail(
            DoctorKind::FenceVocabulary,
            format!("{error}; pages use the built-in vocabulary until it is corrected"),
            "Correct .contextos/fences.toml against the fence conventions standard, or remove it to use the \
             built-in vocabulary, then rerun the doctor.",
        ),
    })
}

#[cfg(test)]
#[path = "doctor_fences_test.rs"]
mod tests;
