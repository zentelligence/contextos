//! The releases a shared job resolved, as far as its version is concerned.
//!
//! A job's version is made from the package release it runs, the manifest that release was installed with, and each
//! library release it resolved with theirs. The basis holds exactly that, owned, so a version can be made from it
//! without reading the store again: the snapshot a validation, a version, and a run share is read once.

use contextos_workflows::{FingerprintLibrary, JobFingerprint, PackageIdentity, ReleaseFingerprintInput, StoreKind};

use crate::package_store::InstalledRelease;

/// One resolved release: who it is and the manifest text it was installed with.
#[derive(Clone, Debug)]
pub struct BasisRelease {
    pub identity: PackageIdentity,
    manifest_text: String,
}

impl From<&InstalledRelease> for BasisRelease {
    fn from(release: &InstalledRelease) -> Self {
        Self {
            identity: release.manifest.identity.clone(),
            manifest_text: release.manifest_text.clone(),
        }
    }
}

impl From<(PackageIdentity, String)> for BasisRelease {
    fn from((identity, manifest_text): (PackageIdentity, String)) -> Self {
        Self {
            identity,
            manifest_text,
        }
    }
}

/// The package release a job runs and the library releases it resolved, in name order.
#[derive(Clone, Debug)]
pub struct ReleaseBasis {
    pub package: BasisRelease,
    pub libraries: Vec<BasisRelease>,
}

impl ReleaseBasis {
    /// The version of the job in `store` whose job file is `job_toml`, which runs this basis.
    ///
    /// It trusts the content digest each manifest records and does not hash the installed files again, so a file edited
    /// in place inside an installed release is not a new version.
    #[must_use]
    pub fn version_in(&self, store: StoreKind, job_toml: &str) -> JobFingerprint {
        let versions: Vec<String> = self
            .libraries
            .iter()
            .map(|library| library.identity.version.to_string())
            .collect();
        let libraries: Vec<FingerprintLibrary<'_>> = self
            .libraries
            .iter()
            .zip(&versions)
            .map(|(library, version)| FingerprintLibrary {
                name: library.identity.name.as_str(),
                version,
                digest: library.identity.digest.as_str(),
                manifest: &library.manifest_text,
            })
            .collect();
        JobFingerprint::from(&ReleaseFingerprintInput {
            store,
            package: self.package.identity.name.as_str(),
            release: &self.package.identity.version.to_string(),
            digest: self.package.identity.digest.as_str(),
            job_toml,
            release_manifest: &self.package.manifest_text,
            libraries: &libraries,
        })
    }

    /// The version of the shared job whose job file is `job_toml`, which runs this basis.
    #[must_use]
    pub fn version(&self, job_toml: &str) -> JobFingerprint {
        self.version_in(StoreKind::Host, job_toml)
    }
}

#[cfg(test)]
#[path = "release_basis_test.rs"]
mod tests;
