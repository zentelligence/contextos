//! Test support: the parts of a package manifest that follow from the files a release holds.
//!
//! A release is read by checking every file it holds against the list its manifest keeps and the whole against the
//! digest the manifest declares, so a release written into a store by hand, or built into a bundle, has to carry both.
//! Unit tests reach this as a module of the crate; integration tests include the same file by path.

use std::fmt::Write as _;
use std::path::Path;

use contextos_packages::{ReleaseFilePath, ReleaseFiles, ReleaseRules, StoreRules};
use sha2::{Digest, Sha256};

/// What went wrong building a fixture.
pub type FixtureError = Box<dyn std::error::Error>;

/// One file of a release laid out: its path and its bytes.
pub type LaidFile = (String, Vec<u8>);

/// A release laid out from a package sketch: a manifest with no workflow in it, the package file, and the files with each
/// workflow's folder.
pub struct LaidOut {
    /// The manifest text, before its `[[files]]` tables.
    pub manifest: String,
    /// Every file of the release but its manifest, by path.
    pub files: Vec<LaidFile>,
}

/// Lays a package sketch out as a release. A sketch is a manifest that still describes its package the short way a test
/// writes it: `[[workflow]]` tables with `slug`, `entry`, `inputs`, `outputs`, `required_tool_contracts`, `log_level`,
/// and `permitted_effects` (the exact calls the workflow may make, none when absent), and the package-level `modules`,
/// `templates`, `exports`, and `[[dependencies]]`. Each workflow becomes a folder `{slug}/` holding its `workflow.toml`
/// and its entry script, taken from where the sketch's files put it. The package-level keys become the package file the
/// release lists: `library.toml` for a sketch that exports and `workflows.toml` for one that holds a workflow, each
/// keeping a file the sketch provides. The manifest keeps the release only.
///
/// # Errors
///
/// Returns the failure when the sketch does not parse, which is a mistake in the test that wrote it.
pub fn lay_out(manifest: &str, files: Vec<LaidFile>) -> Result<LaidOut, FixtureError> {
    let head = manifest.split("\n[[files]]").next().unwrap_or(manifest);
    let as_is = |laid_files| LaidOut {
        manifest: manifest.to_owned(),
        files: laid_files,
    };
    // A manifest that is not TOML is some test's deliberate damage, and is left as it is.
    let Ok(mut table) = toml::from_str::<toml::Table>(head) else {
        return Ok(as_is(files));
    };
    let workflows = match table.remove("workflow") {
        Some(toml::Value::Array(workflows)) => workflows,
        _ => Vec::new(),
    };
    let kept_at_root: std::collections::BTreeSet<String> = ["modules", "templates"]
        .iter()
        .flat_map(|key| strings_of(&table, key))
        .collect();
    let mut files = files;
    let mut moved = std::collections::BTreeSet::new();
    for workflow in workflows.iter().filter_map(toml::Value::as_table) {
        let slug = text_of(workflow, "slug").unwrap_or("workflow").to_owned();
        let entry = text_of(workflow, "entry").unwrap_or("main.rhai").to_owned();
        // A release laid out already keeps its workflow.toml, as when a test lays one out again after writing more into it.
        if !files.iter().any(|(path, _)| *path == format!("{slug}/workflow.toml")) {
            let declared = declaration_of(workflow, &slug, &entry);
            files.push((
                format!("{slug}/workflow.toml"),
                toml::to_string(&declared)?.into_bytes(),
            ));
        }
        if let Some((_, bytes)) = files.iter().find(|(path, _)| *path == entry).cloned() {
            // The entry written beside the manifest is the newer one, so it replaces a copy already in the folder.
            files.retain(|(path, _)| *path != format!("{slug}/{entry}"));
            files.push((format!("{slug}/{entry}"), bytes));
            moved.insert(entry);
        }
    }
    files.retain(|(path, _)| !moved.contains(path) || kept_at_root.contains(path));
    // A sketch that states package-level keys is the newer word on them, so it replaces a workflows.toml laid out before;
    // a library.toml a sketch provides itself, to declare tools, is kept.
    let states_package_keys = !workflows.is_empty()
        || ["modules", "templates", "dependencies"]
            .iter()
            .any(|key| table.contains_key(*key));
    if let Some(package_file) = package_file_of(&table, &workflows, &files) {
        let already_laid = files.iter().any(|(path, _)| *path == package_file.0);
        if !already_laid || (states_package_keys && package_file.0 == "workflows.toml") {
            files.retain(|(path, _)| *path != package_file.0);
            files.push(package_file);
        }
    }
    files.sort();
    let mut kept = toml::Table::new();
    for key in ["name", "version", "digest"] {
        if let Some(value) = table.get(key) {
            kept.insert(key.to_owned(), value.clone());
        }
    }
    Ok(LaidOut {
        manifest: toml::to_string(&kept)?,
        files,
    })
}

/// The package file a sketch's package-level keys make: `library.toml` when it exports, `workflows.toml` when it holds a
/// workflow, which is to say a `[[workflow]]` or a `{folder}/workflow.toml` among its files.
fn package_file_of(table: &toml::Table, workflows: &[toml::Value], files: &[LaidFile]) -> Option<LaidFile> {
    let required = |key: &str| {
        table
            .get(key)
            .and_then(toml::Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(toml::Value::as_table)
                    .fold(String::new(), |mut text, entry| {
                        let _ = write!(
                            text,
                            "[[libraries]]\nname = {:?}\nrequirement = {:?}\n",
                            text_of(entry, "name").unwrap_or_default(),
                            text_of(entry, "requirement").unwrap_or("*"),
                        );
                        text
                    })
            })
            .unwrap_or_default()
    };
    let exports = strings_of(table, "exports");
    if !exports.is_empty() {
        let text = format!(
            "modules = {:?}\nexports = {exports:?}\n{}",
            strings_of(table, "modules"),
            required("dependencies"),
        );
        return Some(("library.toml".to_owned(), text.into_bytes()));
    }
    let mut folders: Vec<String> = workflows
        .iter()
        .filter_map(toml::Value::as_table)
        .map(|workflow| text_of(workflow, "slug").unwrap_or("workflow").to_owned())
        .collect();
    for (path, _) in files {
        if let Some(folder) = path.strip_suffix("/workflow.toml")
            && !folder.contains('/')
            && !folders.iter().any(|known| known == folder)
        {
            folders.push(folder.to_owned());
        }
    }
    if folders.is_empty() {
        return None;
    }
    let text = format!(
        "workflows = {folders:?}\nmodules = {:?}\ntemplates = {:?}\n{}",
        strings_of(table, "modules"),
        strings_of(table, "templates"),
        required("dependencies"),
    );
    Some(("workflows.toml".to_owned(), text.into_bytes()))
}

/// The string at `key` in `table`.
fn text_of<'a>(table: &'a toml::Table, key: &str) -> Option<&'a str> {
    table.get(key).and_then(toml::Value::as_str)
}

/// A sketched workflow's `workflow.toml`: its fields under the names the file uses, and `permitted_effects` as the server
/// allow list and tool allow list that permit exactly those calls (no calls at all when it names none).
fn declaration_of(workflow: &toml::Table, slug: &str, entry: &str) -> toml::Table {
    let string = |text: &str| toml::Value::String(text.to_owned());
    let mut declared = toml::Table::new();
    declared.insert("name".to_owned(), string(slug));
    declared.insert("entry".to_owned(), string(entry));
    for (from, to) in [
        ("inputs", "input"),
        ("outputs", "output"),
        ("required_tool_contracts", "required_tool_contracts"),
        ("log_level", "log_level"),
    ] {
        if let Some(value) = workflow.get(from) {
            declared.insert(to.to_owned(), value.clone());
        }
    }
    let mut servers: Vec<String> = Vec::new();
    let mut tools = Vec::new();
    let effects = workflow.get("permitted_effects").and_then(toml::Value::as_array);
    for effect in effects.into_iter().flatten().filter_map(toml::Value::as_table) {
        let server = text_of(effect, "mcp_server_name").unwrap_or_default();
        if !servers.iter().any(|known| known == server) {
            servers.push(server.to_owned());
        }
        let mut pair = toml::Table::new();
        pair.insert("server".to_owned(), string(server));
        pair.insert("tool".to_owned(), string(text_of(effect, "tool").unwrap_or_default()));
        tools.push(toml::Value::Table(pair));
    }
    let mut servers_table = toml::Table::new();
    servers_table.insert(
        "allow".to_owned(),
        toml::Value::Array(servers.iter().map(|name| string(name)).collect()),
    );
    declared.insert("mcp_servers".to_owned(), toml::Value::Table(servers_table));
    if !tools.is_empty() {
        let mut tools_table = toml::Table::new();
        tools_table.insert("allow".to_owned(), toml::Value::Array(tools));
        declared.insert("mcp_tools".to_owned(), toml::Value::Table(tools_table));
    }
    declared
}

/// A whole release from a package sketch (see [`lay_out`]) and the files it is described with: the complete manifest, with
/// its `digest` declared over the laid-out files and the table of those files, and the files, each with its path.
///
/// # Errors
///
/// Returns the failure when the sketch does not parse or a path is not a valid release file path.
pub fn sketch_release(sketch: &str, files: &[(&str, &[u8])]) -> Result<(String, Vec<LaidFile>), FixtureError> {
    let laid = lay_out(
        sketch,
        files
            .iter()
            .map(|(path, bytes)| ((*path).to_owned(), bytes.to_vec()))
            .collect(),
    )?;
    let held: Vec<(&str, &[u8])> = laid
        .files
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    let digest = digest_of(&held)?;
    let before_files = laid.manifest.split("\n[[files]]").next().unwrap_or(&laid.manifest);
    let mut manifest = String::new();
    let mut has_digest = false;
    for line in before_files.lines() {
        if line.starts_with("digest = ") {
            has_digest = true;
            let _ = writeln!(manifest, "digest = {digest:?}");
        } else {
            let _ = writeln!(manifest, "{line}");
        }
    }
    if !has_digest {
        manifest = format!("digest = {digest:?}\n{manifest}");
    }
    manifest.push_str(&files_table(&held));
    Ok((manifest, laid.files))
}

/// The `sha256:` digest of `files`, which are every file of a release but its manifest, in any order.
///
/// # Errors
///
/// Returns the refusal when a path is not a valid release file path, which is a mistake in the test that named it.
pub fn digest_of(files: &[(&str, &[u8])]) -> Result<String, FixtureError> {
    let mut held = std::collections::BTreeMap::new();
    for (path, bytes) in files {
        held.insert(ReleaseFilePath::try_from(*path)?, bytes.to_vec());
    }
    Ok(StoreRules.digest(&ReleaseFiles(held)).as_str().to_owned())
}

/// The `[[files]]` tables a manifest keeps for `files`, to append to the end of a manifest's text.
#[must_use]
pub fn files_table(files: &[(&str, &[u8])]) -> String {
    let mut table = String::new();
    for (path, bytes) in files {
        let hash = Sha256::digest(bytes).iter().fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
        let _ = write!(table, "\n[[files]]\npath = {path:?}\nhash = {hash:?}\n");
    }
    table
}

/// Writes a release folder at `dir`: its files, and `manifest` with the table of those files appended. The manifest's own
/// `digest` is the caller's to declare, with [`digest_of`] for a release that is whole.
///
/// # Errors
///
/// Returns the failure when the folder cannot be written.
pub fn write_release(dir: &Path, manifest: &str, files: &[(&str, &[u8])]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (path, bytes) in files {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, bytes)?;
    }
    std::fs::write(dir.join("manifest.toml"), format!("{manifest}{}", files_table(files)))
}

/// Writes a whole release at `dir`: a manifest naming `name` and `version`, declaring the digest of `files`, followed by
/// `body` (the rest of the manifest, from its top-level keys to its tables), and the table of `files`, then the files.
///
/// # Errors
///
/// Returns the failure when a path is not a valid release file path or the folder cannot be written.
pub fn write_whole_release(
    dir: &Path,
    (name, version): (&str, &str),
    body: &str,
    files: &[(&str, &[u8])],
) -> Result<(), FixtureError> {
    let sketch = format!("name = {name:?}\nversion = {version:?}\ndigest = \"\"\n{body}");
    let laid = lay_out(
        &sketch,
        files
            .iter()
            .map(|(path, bytes)| ((*path).to_owned(), bytes.to_vec()))
            .collect(),
    )?;
    let held: Vec<(&str, &[u8])> = laid
        .files
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    let digest = digest_of(&held)?;
    let manifest = laid.manifest.replace("digest = \"\"", &format!("digest = {digest:?}"));
    write_release(dir, &manifest, &held)?;
    Ok(())
}

/// Every file below `dir` but its manifest, as `/`-separated paths and bytes.
fn files_below(dir: &Path, base: &Path, found: &mut Vec<(String, Vec<u8>)>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            files_below(&path, base, found)?;
        } else if let Ok(relative) = path.strip_prefix(base) {
            let name = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if name != "manifest.toml" {
                found.push((name, std::fs::read(&path)?));
            }
        }
    }
    Ok(())
}

/// Makes the release at `dir` whole after its files were written by hand: the manifest keeps the table of the files that
/// are there and declares their digest. Everything in the manifest before its `[[files]]` tables stays as written, except
/// the `digest` line.
///
/// # Errors
///
/// Returns the failure when the folder cannot be read or written, or holds a path that is not a valid release file path.
pub fn seal(dir: &Path) -> Result<(), FixtureError> {
    let mut found = Vec::new();
    files_below(dir, dir, &mut found)?;
    found.sort();
    let text = std::fs::read_to_string(dir.join("manifest.toml"))?;
    let laid = lay_out(&text, found)?;
    // The folder becomes the release: files the layout moved are gone from where they were.
    let mut present = Vec::new();
    files_below(dir, dir, &mut present)?;
    for (path, _) in present {
        if !laid.files.iter().any(|(held, _)| *held == path) {
            std::fs::remove_file(dir.join(&path))?;
        }
    }
    for (path, bytes) in &laid.files {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, bytes)?;
    }
    std::fs::write(dir.join("manifest.toml"), &laid.manifest)?;
    let found = laid.files;
    let files: Vec<(&str, &[u8])> = found
        .iter()
        .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
        .collect();
    let digest = digest_of(&files)?;
    let text = std::fs::read_to_string(dir.join("manifest.toml"))?;
    let head = text.split("\n[[files]]").next().unwrap_or(&text);
    let mut sealed = String::new();
    for line in head.lines() {
        if line.starts_with("digest = ") {
            let _ = writeln!(sealed, "digest = {digest:?}");
        } else {
            let _ = writeln!(sealed, "{line}");
        }
    }
    sealed.push_str(&files_table(&files));
    std::fs::write(dir.join("manifest.toml"), sealed)?;
    Ok(())
}

/// [`seal`] for a fixture whose functions return `std::io::Result`.
///
/// # Errors
///
/// As [`seal`], as an I/O error.
pub fn seal_io(dir: &Path) -> std::io::Result<()> {
    seal(dir).map_err(|error| std::io::Error::other(error.to_string()))
}

/// The strings a manifest's `key` array holds, or none when it has no such array.
fn strings_of(table: &toml::Table, key: &str) -> Vec<String> {
    table
        .get(key)
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn folders_in(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries {
            let entry = entry?;
            if entry.path().is_dir() {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Installs, as the vault's package store holds them, the source in `registry/libraries/` and `registry/workflows/`
/// of the vault at `root`: a library release `1.0.0` of each library, and a release of each workflow package at every
/// `version` its workflows' job files name (`1.0.0` when it has none). An existing release of the same version is
/// replaced, so a test that edits source installs it again to make the change a release. Each release lists the package
/// file, the listed workflows' `workflow.toml` and entry scripts, and the modules and templates the package declares.
///
/// # Errors
///
/// Returns the failure when the source cannot be read or a release cannot be written, which is a mistake in the test.
pub fn install_vault_releases(root: &Path) -> Result<(), FixtureError> {
    let store = root.join(".contextos/packages");
    for library in folders_in(&root.join("registry/libraries"))? {
        let folder = root.join("registry/libraries").join(&library);
        let manifest_text = std::fs::read_to_string(folder.join("library.toml"))?;
        let manifest: toml::Table = toml::from_str(&manifest_text)?;
        let mut files = vec![("library.toml".to_owned(), manifest_text.into_bytes())];
        for module in strings_of(&manifest, "modules") {
            let bytes = std::fs::read(folder.join(&module))?;
            files.push((module, bytes));
        }
        let held: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
            .collect();
        let target = store.join(&library).join("1.0.0");
        let _ = std::fs::remove_dir_all(&target);
        write_whole_release(&target, (&library, "1.0.0"), "", &held)?;
    }
    for package in folders_in(&root.join("registry/workflows"))? {
        let folder = root.join("registry/workflows").join(&package);
        let Ok(package_text) = std::fs::read_to_string(folder.join("workflows.toml")) else {
            continue;
        };
        let declared: toml::Table = toml::from_str(&package_text)?;
        let mut files = vec![("workflows.toml".to_owned(), package_text.clone().into_bytes())];
        let mut versions = std::collections::BTreeSet::new();
        for workflow in strings_of(&declared, "workflows") {
            let workflow_folder = folder.join(&workflow);
            let Ok(workflow_text) = std::fs::read_to_string(workflow_folder.join("workflow.toml")) else {
                continue;
            };
            let described: toml::Table = toml::from_str(&workflow_text)?;
            let entry = text_of(&described, "entry").unwrap_or("main.rhai").to_owned();
            files.push((format!("{workflow}/workflow.toml"), workflow_text.into_bytes()));
            if let Ok(bytes) = std::fs::read(workflow_folder.join(&entry)) {
                files.push((format!("{workflow}/{entry}"), bytes));
            }
            for job in std::fs::read_dir(workflow_folder.join("jobs"))
                .into_iter()
                .flatten()
                .flatten()
            {
                if let Ok(text) = std::fs::read_to_string(job.path())
                    && let Ok(table) = toml::from_str::<toml::Table>(&text)
                    && let Some(version) = table.get("version").and_then(toml::Value::as_str)
                {
                    versions.insert(version.to_owned());
                }
            }
        }
        for module in strings_of(&declared, "modules") {
            if let Ok(bytes) = std::fs::read(folder.join(&module)) {
                files.push((module, bytes));
            }
        }
        for template in strings_of(&declared, "templates") {
            // A template that names a path outside the package is some test's deliberate damage, and is left uncopied.
            if ReleaseFilePath::try_from(template.as_str()).is_err() {
                continue;
            }
            let path = folder.join(&template);
            if path.is_dir() {
                let mut found = Vec::new();
                files_below(&path, &path, &mut found)?;
                files.extend(
                    found
                        .into_iter()
                        .map(|(name, bytes)| (format!("{template}/{name}"), bytes)),
                );
            } else if let Ok(bytes) = std::fs::read(&path) {
                files.push((template, bytes));
            }
        }
        if versions.is_empty() {
            versions.insert("1.0.0".to_owned());
        }
        let held: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice()))
            .collect();
        for version in versions {
            let target = store.join(&package).join(&version);
            let _ = std::fs::remove_dir_all(&target);
            write_whole_release(&target, (&package, &version), "", &held)?;
        }
    }
    Ok(())
}

/// The version of the vault job whose job file is `job_toml`, in the workflow folder `directory` of the vault at `root`,
/// as validation works it out: the release the job file names read from the vault's store, with no library lock.
///
/// # Errors
///
/// Returns the failure when the job file names no `version`, or the release is not installed.
#[allow(dead_code)] // used by the integration tests that include this file, not by the unit tests
pub fn vault_version(
    root: &Path,
    directory: &str,
    job_toml: &str,
) -> Result<contextos_workflows::JobFingerprint, FixtureError> {
    let job: toml::Table = toml::from_str(job_toml)?;
    let version = job
        .get("version")
        .and_then(toml::Value::as_str)
        .ok_or("the job file has no version")?;
    let release_dir = root.join(".contextos/packages").join(directory).join(version);
    let manifest_text = std::fs::read_to_string(release_dir.join("manifest.toml"))?;
    let manifest: toml::Table = toml::from_str(&manifest_text)?;
    let digest = manifest
        .get("digest")
        .and_then(toml::Value::as_str)
        .ok_or("the release manifest has no digest")?;
    Ok(contextos_workflows::JobFingerprint::from(
        &contextos_workflows::ReleaseFingerprintInput {
            store: contextos_workflows::StoreKind::Vault,
            package: directory,
            release: version,
            digest,
            job_toml,
            release_manifest: &manifest_text,
            libraries: &[],
        },
    ))
}

/// The version of the vault job described by `workflow_toml`, `script` and `job_toml`, as it would have once installed:
/// the source is installed into a scratch vault as the package `ops` holding the workflow `sync`, and read back, so the
/// version is of the release and not of the source.
///
/// # Errors
///
/// As [`vault_version`], or the failure to write the scratch vault.
#[allow(dead_code)] // used by the integration tests that include this file, not by the unit tests
pub fn version_of_source(
    workflow_toml: &str,
    entry: &str,
    script: &str,
    job_toml: &str,
) -> Result<contextos_workflows::JobFingerprint, FixtureError> {
    let scratch = tempfile::tempdir()?;
    let package = scratch.path().join("registry/workflows/ops");
    let folder = package.join("sync");
    std::fs::create_dir_all(folder.join("jobs"))?;
    std::fs::write(package.join("workflows.toml"), "workflows = [\"sync\"]\n")?;
    std::fs::write(folder.join("workflow.toml"), workflow_toml)?;
    std::fs::write(folder.join(entry), script)?;
    std::fs::write(folder.join("jobs/job.toml"), job_toml)?;
    install_vault_releases(scratch.path())?;
    vault_version(scratch.path(), "ops", job_toml)
}
