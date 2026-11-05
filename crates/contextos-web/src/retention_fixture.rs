//! Test support: releases of the package `crm` with the one workflow `sync`, built as a bundle holds them.

type BoxError = Box<dyn std::error::Error>;

fn append_file<W: std::io::Write>(builder: &mut tar::Builder<W>, name: &str, contents: &[u8]) -> Result<(), BoxError> {
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(contents.len())?);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, name, contents)?;
    Ok(())
}

/// The archive a bundle is uploaded as, for a package sketch and the files it ships beside its manifest.
fn archive_of(sketch: &str, shipped: &[(&str, &[u8])]) -> Result<Vec<u8>, BoxError> {
    let (manifest, files) = crate::release_fixture::sketch_release(sketch, shipped)?;
    let mut archive = Vec::new();
    {
        let encoder = flate2::write::GzEncoder::new(&mut archive, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        append_file(&mut builder, "manifest.toml", manifest.as_bytes())?;
        for (path, bytes) in &files {
            append_file(&mut builder, path, bytes)?;
        }
        builder.into_inner()?.finish()?;
    }
    Ok(archive)
}

/// A release of the package `crm` with the one workflow `sync`, as the archive a bundle is uploaded as.
pub(crate) fn archive(version: &str) -> Result<Vec<u8>, BoxError> {
    archive_with_script(version, version)
}

/// As [`archive`], with `script` as the workflow's code, so two bundles of one version can differ in content.
pub(crate) fn archive_with_script(version: &str, script: &str) -> Result<Vec<u8>, BoxError> {
    archive_of(
        &format!(
            "name = \"crm\"\nversion = {version:?}\ndigest = \"\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n"
        ),
        &[("sync.rhai", script.as_bytes())],
    )
}

/// A release of the package `crm` whose workflow `sync` also states `declarations`, such as an `inputs` list, so two
/// releases can differ in the interface a job sees.
pub(crate) fn archive_declaring(version: &str, declarations: &str) -> Result<Vec<u8>, BoxError> {
    archive_of(
        &format!(
            "name = \"crm\"\nversion = {version:?}\ndigest = \"\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n{declarations}\n"
        ),
        &[("sync.rhai", b"let x = 1;")],
    )
}

/// A release of the package `crm` with the one workflow `sync`, which needs `library` at `requirement`.
pub(crate) fn archive_needing(version: &str, library: &str, requirement: &str) -> Result<Vec<u8>, BoxError> {
    archive_of(
        &format!(
            "name = \"crm\"\nversion = {version:?}\ndigest = \"\"\n\n[[dependencies]]\nname = {library:?}\nrequirement = \
             {requirement:?}\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n"
        ),
        &[("sync.rhai", version.as_bytes())],
    )
}

/// A release of the library `name` exporting the module `m`, as the archive a bundle is uploaded as.
pub(crate) fn library_archive(name: &str, version: &str) -> Result<Vec<u8>, BoxError> {
    archive_of(
        &format!(
            "name = {name:?}\nversion = {version:?}\ndigest = \"\"\nmodules = [\"m.rhai\"]\nexports = [\"m.rhai\"]\n"
        ),
        &[("m.rhai", b"fn m() { 1 }")],
    )
}

/// A release of the package `crm` with the one workflow `sync`, built as a bundle holds it.
pub(crate) fn bundle(version: &str) -> Result<crate::package_bundle::ExtractedBundle, BoxError> {
    Ok(crate::package_bundle::verify_and_extract(&archive(version)?)?)
}

/// Puts the release in the archive into the host store at `store_root` as a store written before retention would hold it,
/// without running retention or recording any state, so a test can start from a store holding several releases.
pub(crate) fn put_without_retention(store_root: &std::path::Path, archive: &[u8]) -> Result<(), BoxError> {
    let extracted = crate::package_bundle::verify_and_extract(archive)?;
    let store = crate::host_store::HostStore::new(store_root);
    let lock = store.lock()?;
    store.install(
        &lock,
        contextos_packages::InstallRequest {
            manifest_text: extracted.manifest_text,
            files: extracted.files,
            origin: contextos_packages::ReleaseOrigin::Bundle,
        },
    )?;
    Ok(())
}

/// A home whose state and job files are fixed: what a store's home holds that names releases, as a test writes it.
pub(crate) struct FixedHome {
    state: contextos_workflows::JobsState,
    files: Vec<contextos_workflows::JobFile>,
}

impl FixedHome {
    pub(crate) fn new(state: &str, files: Vec<contextos_workflows::JobFile>) -> Result<Self, BoxError> {
        Ok(Self {
            state: toml::from_str(state)?,
            files,
        })
    }
}

/// The job file of `job` on the workflow `sync` of `crm`, as read.
pub(crate) fn job_file(job: &str, reading: contextos_workflows::JobFileReading) -> contextos_workflows::JobFile {
    contextos_workflows::JobFile {
        package: "crm".to_owned(),
        workflow: "sync".to_owned(),
        job: job.to_owned(),
        reading,
    }
}

/// A job file that names the release `version`.
pub(crate) fn names(version: &str) -> contextos_workflows::JobFileReading {
    contextos_workflows::JobFileReading::Names(
        semver::Version::parse(version).unwrap_or_else(|_| semver::Version::new(0, 0, 0)),
    )
}

impl crate::package_retention::RetentionHome for FixedHome {
    fn state(
        &self,
    ) -> impl std::future::Future<
        Output = Result<crate::package_retention::StateReading, crate::package_retention::RetentionError>,
    > + Send {
        std::future::ready(Ok(crate::package_retention::StateReading {
            state: self.state.clone(),
            recorded: true,
        }))
    }

    fn job_files(
        &self,
    ) -> impl std::future::Future<
        Output = Result<Vec<contextos_workflows::JobFile>, crate::package_retention::RetentionError>,
    > + Send {
        std::future::ready(Ok(self.files.clone()))
    }
}
