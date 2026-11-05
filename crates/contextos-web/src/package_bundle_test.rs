use sha2::{Digest, Sha256};
use tempfile::tempdir;

use super::*;
use crate::release_fixture::files_table;

type BoxError = Box<dyn std::error::Error>;

/// The package file every bundle here ships, listing its one workflow.
const PACKAGE: (&str, &[u8]) = ("workflows.toml", b"workflows = [\"sync\"]\n");

/// The workflow every bundle here ships: a folder with its `workflow.toml` and its entry script.
const WORKFLOW: (&str, &[u8]) = ("sync/workflow.toml", b"name = \"sync\"\nentry = \"sync.rhai\"\n");

/// Builds a minimal, valid `tar.gz` bundle in memory: a `manifest.toml`
/// whose `digest` is the content digest over every *other*
/// file (mirroring [`compute_content_digest`] exactly, since that is the
/// scheme this module's own documentation settles on - no circularity,
/// so no multi-pass build is needed), and one `sync.rhai` entry script.
fn build_bundle(entry_contents: &[u8]) -> Result<Vec<u8>, BoxError> {
    let digest = digest_of_listed_files(&[PACKAGE, WORKFLOW, ("sync/sync.rhai", entry_contents)]);
    build_archive(&digest, entry_contents)
}

fn digest_of_listed_files(files: &[(&str, &[u8])]) -> String {
    let mut sorted = files.to_vec();
    sorted.sort_by_key(|(name, _)| *name);
    let mut hasher = Sha256::new();
    for (name, contents) in sorted {
        hasher.update(name.as_bytes());
        hasher.update([0u8]);
        hasher.update(contents);
        hasher.update([0u8]);
    }
    let hash = hasher.finalize();
    hash.iter().fold(String::from("sha256:"), |mut key, byte| {
        let _ = std::fmt::Write::write_fmt(&mut key, format_args!("{byte:02x}"));
        key
    })
}

fn build_archive(digest: &str, entry_contents: &[u8]) -> Result<Vec<u8>, BoxError> {
    let manifest = manifest_with_digest(digest, &[PACKAGE, WORKFLOW, ("sync/sync.rhai", entry_contents)]);
    let gzip = Vec::new();
    let encoder = flate2::write::GzEncoder::new(gzip, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append_file(&mut builder, "manifest.toml", manifest.as_bytes())?;
    append_file(&mut builder, PACKAGE.0, PACKAGE.1)?;
    append_file(&mut builder, WORKFLOW.0, WORKFLOW.1)?;
    append_file(&mut builder, "sync/sync.rhai", entry_contents)?;
    let encoder = builder.into_inner()?;
    Ok(encoder.finish()?)
}

fn append_file<W: std::io::Write>(builder: &mut tar::Builder<W>, name: &str, contents: &[u8]) -> Result<(), BoxError> {
    let mut header = tar::Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, name, contents)?;
    Ok(())
}

/// Writes a tar member's raw header `name` bytes directly, bypassing
/// `Builder::append_data`'s own path validation (which itself refuses a
/// `..`-containing path) - the only way to construct a genuinely
/// malicious archive proving `verify_and_extract` rejects one a
/// well-behaved tar writer could never produce, but a hand-crafted
/// upload could.
fn append_file_with_unchecked_path<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    raw_name: &str,
    contents: &[u8],
) -> Result<(), BoxError> {
    let mut header = tar::Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o644);
    {
        let Some(gnu) = header.as_gnu_mut() else {
            return Err("expected a GNU tar header".into());
        };
        let name_bytes = raw_name.as_bytes();
        if name_bytes.len() >= gnu.name.len() {
            return Err("test fixture name too long for a raw tar header field".into());
        }
        gnu.name[..name_bytes.len()].copy_from_slice(name_bytes);
    }
    header.set_cksum();
    builder.append(&header, contents)?;
    Ok(())
}

/// The bytes the extraction holds for `path`.
fn held<'a>(extracted: &'a ExtractedBundle, path: &str) -> Option<&'a [u8]> {
    let path = contextos_packages::ReleaseFilePath::try_from(path).ok()?;
    extracted.files.0.get(&path).map(Vec::as_slice)
}

fn manifest_with_digest(digest: &str, files: &[(&str, &[u8])]) -> String {
    format!(
        "name = \"sync-crm\"\nversion = \"1.0.0\"\ndigest = {digest:?}\n{}",
        files_table(files)
    )
}

#[test]
fn a_well_formed_bundle_extracts_and_parses_its_own_manifest() -> Result<(), BoxError> {
    let bundle = build_bundle(b"// sync workflow")?;

    let extracted = verify_and_extract(&bundle)?;

    assert_eq!(extracted.manifest.identity.name, "sync-crm");
    assert!(extracted.manifest_text.starts_with("name = \"sync-crm\""));
    assert_eq!(held(&extracted, "sync/sync.rhai"), Some(b"// sync workflow".as_slice()));
    assert!(extracted.files.0.len() == 3, "{:?}", extracted.files);
    Ok(())
}

#[test]
fn a_bundle_whose_declared_digest_does_not_match_its_own_content_is_rejected() -> Result<(), BoxError> {
    let wrong_digest = format!("sha256:{}", "a".repeat(64));
    let bundle = build_archive(&wrong_digest, b"// sync workflow")?;

    let result = verify_and_extract(&bundle);

    assert!(matches!(result, Err(BundleError::DigestMismatch { .. })), "{result:?}");
    Ok(())
}

#[test]
fn changing_the_entry_scripts_content_after_the_digest_was_computed_is_detected() -> Result<(), BoxError> {
    // Build with a digest computed over one script body, but ship a
    // different body under the identical bundle - proves the digest
    // genuinely covers file content, not just file names/sizes.
    let digest = digest_of_listed_files(&[PACKAGE, WORKFLOW, ("sync/sync.rhai", b"// original")]);
    let bundle = build_archive(&digest, b"// tampered after the fact")?;

    let result = verify_and_extract(&bundle);

    assert!(matches!(result, Err(BundleError::DigestMismatch { .. })), "{result:?}");
    Ok(())
}

#[test]
fn a_bundle_with_no_manifest_toml_is_rejected() -> Result<(), BoxError> {
    let gzip = Vec::new();
    let encoder = flate2::write::GzEncoder::new(gzip, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append_file(&mut builder, "sync/sync.rhai", b"// no manifest")?;
    let encoder = builder.into_inner()?;
    let bundle = encoder.finish()?;

    let result = verify_and_extract(&bundle);

    assert!(matches!(result, Err(BundleError::MissingManifest)), "{result:?}");
    Ok(())
}

#[test]
fn a_bundle_member_path_escaping_the_release_is_rejected() -> Result<(), BoxError> {
    let digest = digest_of_listed_files(&[("../escape.rhai", b"// traversal")]);
    let manifest = manifest_with_digest(&digest, &[]);
    let gzip = Vec::new();
    let encoder = flate2::write::GzEncoder::new(gzip, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append_file(&mut builder, "manifest.toml", manifest.as_bytes())?;
    append_file_with_unchecked_path(&mut builder, "../escape.rhai", b"// traversal")?;
    let encoder = builder.into_inner()?;
    let bundle = encoder.finish()?;

    let result = verify_and_extract(&bundle);

    assert!(matches!(result, Err(BundleError::UnsafeMemberPath(_))), "{result:?}");
    Ok(())
}

#[test]
fn a_bundle_member_with_an_absolute_path_is_rejected() -> Result<(), BoxError> {
    let digest = digest_of_listed_files(&[("etc/passwd", b"// absolute")]);
    let manifest = manifest_with_digest(&digest, &[]);
    let gzip = Vec::new();
    let encoder = flate2::write::GzEncoder::new(gzip, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append_file(&mut builder, "manifest.toml", manifest.as_bytes())?;
    append_file_with_unchecked_path(&mut builder, "/etc/passwd", b"// absolute")?;
    let encoder = builder.into_inner()?;
    let bundle = encoder.finish()?;

    let result = verify_and_extract(&bundle);

    assert!(matches!(result, Err(BundleError::UnsafeMemberPath(_))), "{result:?}");
    Ok(())
}

#[test]
fn a_malformed_gzip_stream_is_rejected_as_malformed() {
    let not_gzip = b"this is not a gzip stream at all".to_vec();

    let result = verify_and_extract(&not_gzip);

    assert!(matches!(result, Err(BundleError::Malformed(_))), "{result:?}");
}

/// A bundle with `sync.rhai` and the given extra files, whose manifest declares `templates`.
fn build_bundle_with_templates(templates: &str, files: &[(&str, &[u8])]) -> Result<Vec<u8>, BoxError> {
    let package = format!("workflows = [\"sync\"]\ntemplates = {templates}\n");
    let mut all: Vec<(&str, &[u8])> = vec![
        ("workflows.toml", package.as_bytes()),
        WORKFLOW,
        ("sync/sync.rhai", b"log_info(\"hi\");"),
    ];
    all.extend_from_slice(files);
    let digest = digest_of_listed_files(&all);
    let manifest = format!(
        "name = \"sync-crm\"\nversion = \"1.0.0\"\ndigest = {digest:?}\n{}",
        files_table(&all)
    );
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append_file(&mut builder, "manifest.toml", manifest.as_bytes())?;
    for (name, contents) in all {
        append_file(&mut builder, name, contents)?;
    }
    Ok(builder.into_inner()?.finish()?)
}

#[test]
fn a_bundle_holding_every_template_it_declares_is_accepted() -> Result<(), BoxError> {
    let bundle = build_bundle_with_templates(
        "[\"scaffold\", \"rules/lint.toml\"]",
        &[("scaffold/AGENTS.md", b"# Agents\n"), ("rules/lint.toml", b"")],
    )?;

    let extracted = verify_and_extract(&bundle)?;

    assert_eq!(
        extracted.kind.templates().map(|templates| templates.entries().len()),
        Some(2)
    );
    Ok(())
}

#[test]
fn a_bundle_declaring_a_template_it_does_not_hold_is_refused_and_leaves_nothing_behind() -> Result<(), BoxError> {
    let bundle = build_bundle_with_templates("[\"scaffold\"]", &[("other/AGENTS.md", b"# Agents\n")])?;

    let refused = verify_and_extract(&bundle);

    assert!(
        matches!(refused, Err(BundleError::TemplateMissing(ref path)) if path == "scaffold"),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_bundle_holding_a_link_is_refused_whatever_it_points_at() -> Result<(), BoxError> {
    for kind in [tar::EntryType::Symlink, tar::EntryType::Link] {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        append_file(
            &mut builder,
            "manifest.toml",
            manifest_with_digest(&digest_of_listed_files(&[]), &[]).as_bytes(),
        )?;
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(kind);
        header.set_size(0);
        header.set_mode(0o777);
        builder.append_link(&mut header, "scaffold/AGENTS.md", "manifest.toml")?;
        let bundle = builder.into_inner()?.finish()?;

        let refused = verify_and_extract(&bundle);

        assert!(
            matches!(refused, Err(BundleError::UnsupportedMember(ref path)) if path == "scaffold/AGENTS.md"),
            "{kind:?}: {refused:?}"
        );
    }
    Ok(())
}

/// Appends the PAX global header `git archive` writes at the start of every archive, carrying the commit id.
fn append_pax_global_header<W: std::io::Write>(builder: &mut tar::Builder<W>) -> Result<(), BoxError> {
    let record = b"52 comment=0123456789abcdef0123456789abcdef01234567\n";
    let mut header = tar::Header::new_ustar();
    header.set_entry_type(tar::EntryType::XGlobalHeader);
    header.set_size(record.len() as u64);
    header.set_mode(0o666);
    header.set_path("pax_global_header")?;
    header.set_cksum();
    builder.append(&header, &record[..])?;
    Ok(())
}

#[test]
fn a_bundle_made_by_git_archive_with_its_global_header_installs() -> Result<(), BoxError> {
    let entry: &[u8] = b"log_info(\"hi\");";
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append_pax_global_header(&mut builder)?;
    append_file(
        &mut builder,
        "manifest.toml",
        manifest_with_digest(
            &digest_of_listed_files(&[PACKAGE, WORKFLOW, ("sync/sync.rhai", entry)]),
            &[PACKAGE, WORKFLOW, ("sync/sync.rhai", entry)],
        )
        .as_bytes(),
    )?;
    append_file(&mut builder, PACKAGE.0, PACKAGE.1)?;
    append_file(&mut builder, WORKFLOW.0, WORKFLOW.1)?;
    append_file(&mut builder, "sync/sync.rhai", entry)?;
    let bundle = builder.into_inner()?.finish()?;

    let extracted = verify_and_extract(&bundle)?;

    assert_eq!(extracted.files.0.len(), 3, "{:?}", extracted.files);
    assert_eq!(held(&extracted, "sync/sync.rhai"), Some(entry));
    Ok(())
}

#[test]
fn a_bundle_member_whose_path_is_longer_than_a_tar_header_holds_installs() -> Result<(), BoxError> {
    let long = format!("scaffold/{}/AGENTS.md", "deeply-nested-folder".repeat(8));
    let bundle = build_bundle_with_templates("[\"scaffold\"]", &[(long.as_str(), b"# Agents\n")])?;

    let extracted = verify_and_extract(&bundle)?;

    assert_eq!(held(&extracted, &long), Some(b"# Agents\n".as_slice()));
    Ok(())
}

#[test]
fn a_bundle_listing_a_file_whose_name_no_script_could_address_is_refused_at_install() -> Result<(), BoxError> {
    let bundle = build_bundle_with_templates(
        "[\"scaffold\"]",
        &[("scaffold/AGENTS.md", b""), ("scaffold/notes:2024.md", b"")],
    )?;

    let refused = verify_and_extract(&bundle);

    assert!(
        matches!(refused, Err(BundleError::InvalidManifest(PackageError::InvalidFilePath(ref path))) if path.contains("notes:2024.md")),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_portable_name_joins_path_components_with_a_forward_slash_on_every_platform() {
    let relative: std::path::PathBuf = ["templates", "shipped", "nested.md"].iter().collect();

    assert_eq!(portable_name(&relative), "templates/shipped/nested.md");
}

#[test]
fn a_nested_bundle_verifies_against_the_digest_package_authors_tooling_computes() -> Result<(), BoxError> {
    // The digest the plugin's `tools/package_digest.py` computes over exactly these four files: `/`-joined paths,
    // with `templates/shipped/nested.md` ordered before `templates-index.md` because `/` sorts before every other
    // character. A native `\` separator, or a plain string sort, gives a different digest.
    let digest = "sha256:5a179847f6ec1efe0582a5eb80868532bd8a4cf0181259d0d4d88cae048c0517";
    let manifest = manifest_with_digest(
        digest,
        &[
            ("templates-index.md", b"# Sibling\n"),
            ("templates/shipped/nested.md", b"# Nested\n"),
            ("sync/sync.rhai", b"log_info(\"hi\");"),
            PACKAGE,
            WORKFLOW,
        ],
    );
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append_file(&mut builder, "manifest.toml", manifest.as_bytes())?;
    append_file(&mut builder, "templates-index.md", b"# Sibling\n")?;
    append_file(&mut builder, "templates/shipped/nested.md", b"# Nested\n")?;
    append_file(&mut builder, "sync/sync.rhai", b"log_info(\"hi\");")?;
    append_file(&mut builder, PACKAGE.0, PACKAGE.1)?;
    append_file(&mut builder, WORKFLOW.0, WORKFLOW.1)?;
    let bundle = builder.into_inner()?.finish()?;

    let extracted = verify_and_extract(&bundle)?;

    assert_eq!(
        held(&extracted, "templates/shipped/nested.md"),
        Some(b"# Nested\n".as_slice())
    );
    Ok(())
}

/// A bundle with `sync.rhai` and the given extra files, whose manifest declares `modules`.
fn build_bundle_with_modules(modules: &str, files: &[(&str, &[u8])]) -> Result<Vec<u8>, BoxError> {
    let package = format!("workflows = [\"sync\"]\nmodules = {modules}\n");
    let mut all: Vec<(&str, &[u8])> = vec![
        ("workflows.toml", package.as_bytes()),
        WORKFLOW,
        ("sync/sync.rhai", b"log_info(\"hi\");"),
    ];
    all.extend_from_slice(files);
    let digest = digest_of_listed_files(&all);
    let manifest = format!(
        "name = \"sync-crm\"\nversion = \"1.0.0\"\ndigest = {digest:?}\n{}",
        files_table(&all)
    );
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append_file(&mut builder, "manifest.toml", manifest.as_bytes())?;
    for (name, contents) in all {
        append_file(&mut builder, name, contents)?;
    }
    Ok(builder.into_inner()?.finish()?)
}

#[test]
fn a_bundle_holding_every_module_it_declares_is_accepted() -> Result<(), BoxError> {
    let bundle = build_bundle_with_modules("[\"lib/vault.rhai\"]", &[("lib/vault.rhai", b"fn info() { 1 }")])?;

    let extracted = verify_and_extract(&bundle)?;

    assert_eq!(extracted.kind.modules().entries().len(), 1);
    Ok(())
}

#[test]
fn a_bundle_missing_a_declared_module_or_holding_a_folder_in_its_place_is_refused_and_nothing_is_kept()
-> Result<(), BoxError> {
    for files in [
        Vec::<(&str, &[u8])>::new(),
        vec![("lib/vault.rhai/inner.rhai", b"fn info() { 1 }".as_slice())],
    ] {
        let bundle = build_bundle_with_modules("[\"lib/vault.rhai\"]", &files)?;

        let refused = verify_and_extract(&bundle);

        assert!(
            matches!(refused, Err(BundleError::ModuleMissing(ref path)) if path == "lib/vault.rhai"),
            "{refused:?}"
        );
    }
    Ok(())
}

/// The files of the pinned digest below, written to a folder and as a release's files in memory.
const PINNED_FILES: [(&str, &[u8]); 5] = [("a-b.txt", b"q"), ("a/b.txt", b"p"), WORKFLOW, PACKAGE, ("z.txt", b"r")];

/// Computed outside this code base as SHA-256 of `a/b.txt\0p\0a-b.txt\0q\0sync/workflow.toml\0{its text}\0workflows.toml\0{its text}\0z.txt\0r\0`: a folder's files in path order,
/// segment by segment, each as its path, a zero byte, its bytes, and a zero byte. Every release installed before the
/// package engine existed carries a digest made this way.
const PINNED_DIGEST: &str = "sha256:d186ca78928dbc17f6eaf953214bca2c85fdad51f9649144b8fec34de59613e5";

#[test]
fn a_package_folder_declaring_the_digest_every_installed_release_already_carries_for_its_files_verifies()
-> Result<(), BoxError> {
    let folder = tempdir()?;
    for (path, bytes) in PINNED_FILES {
        let target = folder.path().join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(target, bytes)?;
    }
    fs::write(
        folder.path().join("manifest.toml"),
        manifest_with_digest(PINNED_DIGEST, &PINNED_FILES),
    )?;

    let extracted = verify_and_copy_folder(folder.path())?;

    assert_eq!(extracted.files.0.len(), PINNED_FILES.len());
    Ok(())
}

#[test]
fn the_engine_digests_files_exactly_as_the_digest_installed_releases_carry() -> Result<(), BoxError> {
    use contextos_packages::{ReleaseFilePath, ReleaseFiles, ReleaseRules, StoreRules};

    let files = ReleaseFiles(
        PINNED_FILES
            .iter()
            .map(|(path, bytes)| Ok((ReleaseFilePath::try_from(*path)?, bytes.to_vec())))
            .collect::<Result<_, contextos_packages::PackagesError>>()?,
    );
    assert_eq!(StoreRules.digest(&files).as_str(), PINNED_DIGEST);
    Ok(())
}

/// A bundle whose manifest lists `listed` and whose archive holds `held`, declaring the digest of `held`.
fn build_bundle_listing(listed: &[(&str, &[u8])], held: &[(&str, &[u8])]) -> Result<Vec<u8>, BoxError> {
    let manifest = manifest_with_digest(&digest_of_listed_files(held), listed);
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append_file(&mut builder, "manifest.toml", manifest.as_bytes())?;
    for (name, contents) in held {
        append_file(&mut builder, name, contents)?;
    }
    Ok(builder.into_inner()?.finish()?)
}

#[test]
fn a_bundle_whose_manifest_lists_no_files_is_refused_naming_the_first_file_it_leaves_out() -> Result<(), BoxError> {
    let bundle = build_bundle_listing(
        &[PACKAGE, WORKFLOW],
        &[PACKAGE, WORKFLOW, ("sync/sync.rhai", b"// sync workflow")],
    )?;

    let refused = verify_and_extract(&bundle);

    assert!(
        matches!(refused, Err(BundleError::FileNotListed(ref path)) if path == "sync/sync.rhai"),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_bundle_holding_a_file_its_manifest_does_not_list_is_refused() -> Result<(), BoxError> {
    let bundle = build_bundle_listing(
        &[PACKAGE, WORKFLOW, ("sync/sync.rhai", b"// sync workflow")],
        &[
            PACKAGE,
            WORKFLOW,
            ("sync/sync.rhai", b"// sync workflow"),
            ("extra/notes.md", b"# extra"),
        ],
    )?;

    let refused = verify_and_extract(&bundle);

    assert!(
        matches!(refused, Err(BundleError::FileNotListed(ref path)) if path == "extra/notes.md"),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_bundle_missing_a_file_its_manifest_lists_is_refused() -> Result<(), BoxError> {
    let bundle = build_bundle_listing(
        &[
            PACKAGE,
            WORKFLOW,
            ("sync/sync.rhai", b"// sync workflow"),
            ("lib/helper.rhai", b"fn a() { 1 }"),
        ],
        &[PACKAGE, WORKFLOW, ("sync/sync.rhai", b"// sync workflow")],
    )?;

    let refused = verify_and_extract(&bundle);

    assert!(
        matches!(refused, Err(BundleError::ListedFileMissing(ref path)) if path == "lib/helper.rhai"),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_bundle_whose_file_differs_from_the_hash_its_manifest_lists_is_refused() -> Result<(), BoxError> {
    let bundle = build_bundle_listing(
        &[PACKAGE, WORKFLOW, ("sync/sync.rhai", b"// as listed")],
        &[PACKAGE, WORKFLOW, ("sync/sync.rhai", b"// as shipped")],
    )?;

    let refused = verify_and_extract(&bundle);

    assert!(
        matches!(refused, Err(BundleError::FileHashMismatch(ref path)) if path == "sync/sync.rhai"),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_package_folder_is_read_into_the_files_a_bundle_of_it_would_hold() -> Result<(), BoxError> {
    let folder = tempdir()?;
    let entry: &[u8] = b"// sync workflow";
    let manifest = manifest_with_digest(
        &digest_of_listed_files(&[PACKAGE, WORKFLOW, ("sync/sync.rhai", entry)]),
        &[PACKAGE, WORKFLOW, ("sync/sync.rhai", entry)],
    );
    std::fs::create_dir_all(folder.path().join("sync"))?;
    std::fs::write(folder.path().join("manifest.toml"), &manifest)?;
    std::fs::write(folder.path().join(PACKAGE.0), PACKAGE.1)?;
    std::fs::write(folder.path().join(WORKFLOW.0), WORKFLOW.1)?;
    std::fs::write(folder.path().join("sync/sync.rhai"), entry)?;

    let extracted = verify_and_copy_folder(folder.path())?;

    assert_eq!(extracted.manifest_text, manifest);
    assert_eq!(held(&extracted, "sync/sync.rhai"), Some(entry));
    Ok(())
}

#[test]
fn a_package_folder_missing_a_file_its_manifest_lists_is_refused() -> Result<(), BoxError> {
    let folder = tempdir()?;
    let entry: &[u8] = b"// sync workflow";
    let manifest = manifest_with_digest(
        &digest_of_listed_files(&[PACKAGE, WORKFLOW, ("sync/sync.rhai", entry)]),
        &[PACKAGE, WORKFLOW, ("sync/sync.rhai", entry)],
    );
    std::fs::create_dir_all(folder.path().join("sync"))?;
    std::fs::write(folder.path().join("manifest.toml"), manifest)?;
    std::fs::write(folder.path().join(PACKAGE.0), PACKAGE.1)?;
    std::fs::write(folder.path().join(WORKFLOW.0), WORKFLOW.1)?;

    let refused = verify_and_copy_folder(folder.path());

    assert!(
        matches!(refused, Err(BundleError::ListedFileMissing(ref path)) if path == "sync/sync.rhai"),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_bundle_that_expands_past_the_size_limit_is_refused_before_it_is_held() -> Result<(), BoxError> {
    let limit = crate::package_store::MAX_BUNDLE_BYTES;
    let entry = vec![0u8; limit + 1];
    let bundle = build_bundle_listing(
        &[PACKAGE, WORKFLOW, ("sync/sync.rhai", &entry)],
        &[PACKAGE, WORKFLOW, ("sync/sync.rhai", &entry)],
    )?;
    assert!(bundle.len() < limit, "zeros compress far below the limit");

    let refused = verify_and_extract(&bundle);

    assert!(
        matches!(refused, Err(BundleError::ExpandsPastLimit { limit: refused_at }) if refused_at == limit),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_package_file_that_is_invalid_is_reported_against_that_file_and_not_the_manifest() -> Result<(), BoxError> {
    let broken = ("workflows.toml", b"workflows = \"sync\"\n".as_slice());
    let all = [broken, WORKFLOW, ("sync/sync.rhai", b"()".as_slice())];
    let digest = digest_of_listed_files(&all);
    let manifest = format!(
        "name = \"sync-crm\"\nversion = \"1.0.0\"\ndigest = {digest:?}\n{}",
        files_table(&all)
    );
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    append_file(&mut builder, "manifest.toml", manifest.as_bytes())?;
    for (name, contents) in all {
        append_file(&mut builder, name, contents)?;
    }
    let bundle = builder.into_inner()?.finish()?;

    let refused = verify_and_extract(&bundle);

    assert!(
        matches!(
            refused,
            Err(BundleError::InvalidPackageFile {
                file: "workflows.toml",
                ..
            })
        ),
        "{refused:?}"
    );
    Ok(())
}
