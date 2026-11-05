//! Sidecar `<name>.md` maintenance for `base_create`/`base_apply`: the
//! paired file's key scheme and record-activity counters
//! ([`contextos_obsidian::BaseSidecar`]). A problem here never fails the
//! call that owns the primary `.base` write; it surfaces as a warning
//! instead, matching this server's own "the completed write is the
//! contract" discipline for every other secondary effect.

use contextos_core::{Origin, VaultPath, VaultPathInput, VaultSet, WriteMutation};
use contextos_fs::Filesystem;
use contextos_obsidian::{BaseSidecar, BaseSidecarPatch, FrontmatterDocument, FrontmatterInput};

use crate::server::{RoutedMutationService, WarningMessages};
use crate::tool_error::ToolError;

/// Derives a `.base` file's paired sidecar path: same directory and file
/// stem, `.md` extension.
pub(crate) fn sidecar_path(roots: &VaultSet, base_path: &VaultPath) -> Result<VaultPath, ToolError> {
    let root = roots
        .root(base_path.root_id())
        .ok_or(ToolError::Invalid("base vault root is not configured"))?;
    let relative = base_path.relative().with_extension("md");
    let absolute = root.path().join(&relative);
    let raw = absolute
        .to_str()
        .ok_or(ToolError::Invalid("sidecar path must be valid UTF-8"))?;
    Ok(VaultPath::try_from(VaultPathInput { roots, raw })?)
}

/// `base_create`'s sidecar step: creates the sidecar when absent (always,
/// regardless of whether any key prefix was supplied); leaves an existing one
/// exactly as is, warning only when its declared key prefixes conflict with
/// those requested. `requested` is already validated, so nothing here can
/// fail the caller's own `.base` write. Returns the sidecar's vault-relative
/// path text and any warnings.
pub(crate) fn ensure_sidecar_on_create(
    filesystem: &Filesystem,
    mutations: &RoutedMutationService,
    sidecar_path: &VaultPath,
    base_stem: &str,
    requested: &BaseSidecar,
) -> (String, Vec<String>) {
    let mut warnings = Vec::new();
    let path_text = sidecar_path.portable_relative();

    let existing = match contextos_core::ReadsVault::read_optional_text(filesystem, sidecar_path) {
        Ok(existing) => existing,
        Err(error) => {
            warnings.push(format!("could not check for an existing base sidecar: {error}"));
            return (path_text, warnings);
        }
    };

    match existing {
        None => {
            let document = FrontmatterDocument::from(FrontmatterInput {
                frontmatter: serde_json::Map::from(requested),
                body: format!("![[{base_stem}.base|{base_stem}]]\n"),
            });
            let content = match String::try_from(&document) {
                Ok(content) => content,
                Err(error) => {
                    warnings.push(format!("could not render the base sidecar: {error}"));
                    return (path_text, warnings);
                }
            };
            match mutations.write_file(&WriteMutation {
                path: sidecar_path.clone(),
                content: content.into(),
                expected_hash: None,
                force: false,
                origin: Origin::Tool("base_create".to_owned()),
            }) {
                Ok(result) => {
                    let WarningMessages(secondary) = WarningMessages::from(result.warnings);
                    warnings.extend(secondary);
                }
                Err(error) => warnings.push(format!("could not create the base sidecar: {error}")),
            }
        }
        Some(existing_text) => {
            if let Some(warning) = key_prefix_conflict(&existing_text.content, requested) {
                warnings.push(warning);
            }
        }
    }

    (path_text, warnings)
}

/// `base_apply`'s sidecar step: refreshes an existing sidecar's
/// `last_update` only. Never creates one: `base_apply` mutates a `.base`'s
/// schema, it does not establish a new key scheme.
pub(crate) fn touch_sidecar_on_apply(
    filesystem: &Filesystem,
    mutations: &RoutedMutationService,
    sidecar_path: &VaultPath,
    timestamp: &str,
) -> Vec<String> {
    let mut warnings = Vec::new();

    let existing = match contextos_core::ReadsVault::read_optional_text(filesystem, sidecar_path) {
        Ok(Some(existing)) => existing,
        Ok(None) => return warnings,
        Err(error) => {
            warnings.push(format!("could not check for an existing base sidecar: {error}"));
            return warnings;
        }
    };

    let mut document = match FrontmatterDocument::try_from(existing.content.as_str()) {
        Ok(document) => document,
        Err(error) => {
            warnings.push(format!("existing base sidecar frontmatter is invalid: {error}"));
            return warnings;
        }
    };
    let sidecar = match BaseSidecar::try_from(document.frontmatter()) {
        Ok(sidecar) => sidecar.touch(timestamp),
        Err(error) => {
            warnings.push(format!("existing base sidecar frontmatter is invalid: {error}"));
            return warnings;
        }
    };
    let BaseSidecarPatch(patch) = BaseSidecarPatch::from(&sidecar);
    document.merge_frontmatter(patch);
    let content = match String::try_from(&document) {
        Ok(content) => content,
        Err(error) => {
            warnings.push(format!("could not render the base sidecar: {error}"));
            return warnings;
        }
    };
    match mutations.write_file(&WriteMutation {
        path: sidecar_path.clone(),
        content: content.into(),
        expected_hash: Some(existing.content_hash),
        force: false,
        origin: Origin::Tool("base_apply".to_owned()),
    }) {
        Ok(result) => {
            let WarningMessages(secondary) = WarningMessages::from(result.warnings);
            warnings.extend(secondary);
        }
        Err(error) => warnings.push(format!("could not update the base sidecar: {error}")),
    }

    warnings
}

/// Reports a conflict when an existing sidecar's declared key prefixes do
/// not match those requested: the same set of prefixes (order and counters
/// aside) and the same default. Returns `None` (never a conflict) when no
/// prefix was requested at all, when the existing sidecar cannot be read
/// (already reported by the caller as its own warning path), or when they
/// already agree.
fn key_prefix_conflict(existing_content: &str, requested: &BaseSidecar) -> Option<String> {
    if requested.series().is_empty() {
        return None;
    }
    let document = FrontmatterDocument::try_from(existing_content).ok()?;
    let existing = BaseSidecar::try_from(document.frontmatter()).ok()?;
    let prefixes = |sidecar: &BaseSidecar| {
        let mut prefixes: Vec<String> = sidecar
            .series()
            .iter()
            .map(|series| series.prefix().to_owned())
            .collect();
        prefixes.sort();
        prefixes
    };
    if prefixes(&existing) == prefixes(requested) && existing.default_prefix() == requested.default_prefix() {
        return None;
    }
    let requested_prefixes: Vec<&str> = requested
        .series()
        .iter()
        .map(contextos_obsidian::KeySeries::prefix)
        .collect();
    Some(format!(
        "an existing base sidecar already has a different key scheme; leaving it untouched (requested key prefixes {requested_prefixes:?})"
    ))
}
