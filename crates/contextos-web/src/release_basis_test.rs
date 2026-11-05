use contextos_workflows::{ContentDigest, FingerprintLibrary, PackageName, ReleaseFingerprintInput, StoreKind};

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const LIBRARY_DIGEST: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const MANIFEST: &str = "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n";
const LIBRARY_MANIFEST: &str = "name = \"acme-text\"\nversion = \"1.4.2\"\n";
const JOB: &str = "name = \"Nightly\"\n";

fn identity(name: &str, version: &str, digest: &str) -> Result<PackageIdentity, Box<dyn std::error::Error>> {
    Ok(PackageIdentity {
        name: PackageName::try_from(name)?,
        version: version.parse()?,
        digest: ContentDigest::try_new(digest.to_owned())?,
    })
}

fn basis(with_library: bool) -> Result<ReleaseBasis, Box<dyn std::error::Error>> {
    let libraries = if with_library {
        vec![BasisRelease::from((
            identity("acme-text", "1.4.2", LIBRARY_DIGEST)?,
            LIBRARY_MANIFEST.to_owned(),
        ))]
    } else {
        Vec::new()
    };
    Ok(ReleaseBasis {
        package: BasisRelease::from((identity("acme", "1.0.0", DIGEST)?, MANIFEST.to_owned())),
        libraries,
    })
}

#[test]
fn a_basis_makes_the_one_version_form_for_the_store_kind_it_is_asked_for() -> TestResult {
    let libraries = [FingerprintLibrary {
        name: "acme-text",
        version: "1.4.2",
        digest: LIBRARY_DIGEST,
        manifest: LIBRARY_MANIFEST,
    }];
    for store in [StoreKind::Host, StoreKind::Vault] {
        let expected = JobFingerprint::from(&ReleaseFingerprintInput {
            store,
            package: "acme",
            release: "1.0.0",
            digest: DIGEST,
            job_toml: JOB,
            release_manifest: MANIFEST,
            libraries: &libraries,
        });

        assert_eq!(basis(true)?.version_in(store, JOB), expected, "{store:?}");
    }
    Ok(())
}

#[test]
fn a_vault_basis_and_a_host_basis_of_the_same_release_are_different_versions() -> TestResult {
    let basis = basis(false)?;

    assert_ne!(
        basis.version_in(StoreKind::Vault, JOB),
        basis.version_in(StoreKind::Host, JOB)
    );
    Ok(())
}

#[test]
fn the_host_version_of_a_basis_is_its_version_in_the_host_store() -> TestResult {
    let basis = basis(true)?;

    assert_eq!(basis.version(JOB), basis.version_in(StoreKind::Host, JOB));
    Ok(())
}
