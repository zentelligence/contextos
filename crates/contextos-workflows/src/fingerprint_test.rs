use super::*;

const JOB: &str = "name = \"Nightly\"\n[inputs]\nsource = \"a\"\n";

mod release {
    use super::*;

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER_DIGEST: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn shared(digest: &str, version: &str, job: &str) -> JobFingerprint {
        shared_with(digest, version, job, MANIFEST, &[])
    }

    fn vault_with(digest: &str, version: &str, job: &str, libraries: &[FingerprintLibrary<'_>]) -> JobFingerprint {
        JobFingerprint::from(&ReleaseFingerprintInput {
            store: StoreKind::Vault,
            package: "acme",
            release: version,
            digest,
            job_toml: job,
            release_manifest: MANIFEST,
            libraries,
        })
    }

    const MANIFEST: &str = "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n";
    const LIBRARY_MANIFEST: &str = "name = \"acme-text\"\nversion = \"1.4.2\"\ndigest = \"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"\n";

    fn shared_with(
        digest: &str,
        version: &str,
        job: &str,
        manifest: &str,
        libraries: &[FingerprintLibrary<'_>],
    ) -> JobFingerprint {
        JobFingerprint::from(&ReleaseFingerprintInput {
            store: StoreKind::Host,
            package: "acme",
            release: version,
            digest,
            job_toml: job,
            release_manifest: manifest,
            libraries,
        })
    }

    fn text_library<'a>(version: &'a str, digest: &'a str, manifest: &'a str) -> FingerprintLibrary<'a> {
        FingerprintLibrary {
            name: "acme-text",
            version,
            digest,
            manifest,
        }
    }

    #[test]
    fn the_same_package_and_job_file_give_the_same_fingerprint() {
        assert_eq!(shared(DIGEST, "1.0.0", JOB), shared(DIGEST, "1.0.0", JOB));
    }

    #[test]
    fn a_change_to_the_package_content_the_resolved_version_or_the_job_file_changes_the_fingerprint() {
        let base = shared(DIGEST, "1.0.0", JOB);

        assert_ne!(base, shared(OTHER_DIGEST, "1.0.0", JOB));
        assert_ne!(base, shared(DIGEST, "1.0.1", JOB));
        assert_ne!(
            base,
            shared(DIGEST, "1.0.0", "name = \"Nightly\"\n[inputs]\nsource = \"b\"\n")
        );
    }

    #[test]
    fn the_ready_key_is_not_part_of_a_shared_jobs_version() {
        let without = shared(DIGEST, "1.0.0", JOB);
        let with = shared(DIGEST, "1.0.0", &format!("ready = true\n{JOB}"));

        assert_eq!(without, with);
    }

    #[test]
    fn the_store_kind_and_the_package_name_are_part_of_the_version() {
        let host = shared(DIGEST, "1.0.0", JOB);

        assert_ne!(host, vault_with(DIGEST, "1.0.0", JOB, &[]));
        assert_ne!(
            host,
            JobFingerprint::from(&ReleaseFingerprintInput {
                store: StoreKind::Host,
                package: "other",
                release: "1.0.0",
                digest: DIGEST,
                job_toml: JOB,
                release_manifest: MANIFEST,
                libraries: &[],
            })
        );
        assert_ne!(
            shared("", "", ""),
            vault_with("", "", "", &[]),
            "the two kinds are hashed apart even when every part is empty"
        );
    }

    #[test]
    fn a_vault_job_and_a_host_job_on_the_same_release_differ_only_by_their_store_kind() {
        assert_eq!(
            vault_with(DIGEST, "1.0.0", "name = \"Nightly\"\n", &[]).as_str(),
            "sha256:f9319601a8d464f2c9099619a516c6d57be5f963bc857e6182c2fdcbd4b17fee"
        );
        let libraries = [text_library("1.4.2", OTHER_DIGEST, LIBRARY_MANIFEST)];
        assert_eq!(
            vault_with(DIGEST, "1.0.0", "name = \"Nightly\"\n", &libraries).as_str(),
            "sha256:b36decb2e914fcd97bb37c3d49a046840638a8b6251f74b349f853628f5f11bb"
        );
    }

    #[test]
    fn a_host_job_without_libraries_has_the_pinned_version_of_its_parts_and_manifest() {
        // Fixes the form independently of the code: each part length-prefixed with eight big-endian bytes, in the
        // order store kind, package name, release, release digest, job file without `ready`, release manifest
        // without `digest`.
        assert_eq!(
            shared(DIGEST, "1.0.0", "name = \"Nightly\"\n").as_str(),
            "sha256:b00785ba3981bab126f8c8bfb5a3b50e044cd08f10156d73dd4a9da731fbe554"
        );
    }

    #[test]
    fn a_host_job_with_libraries_has_the_pinned_version_of_its_parts_and_each_library() {
        // As above, followed by a `libraries` tag part and each library's name, release, digest, and manifest without
        // `digest`.
        let libraries = [text_library("1.4.2", OTHER_DIGEST, LIBRARY_MANIFEST)];
        assert_eq!(
            shared_with(DIGEST, "1.0.0", "name = \"Nightly\"\n", MANIFEST, &libraries).as_str(),
            "sha256:63b8d3a64bf9c2e5bd2c0ce32540b45fbcd4b50ca1abcb53ef20578f04d95ee1"
        );
    }

    #[test]
    fn a_reinstalled_release_with_another_manifest_is_a_new_version_but_formatting_and_the_digest_key_are_not() {
        let base = shared(DIGEST, "1.0.0", JOB);
        let widened = format!("{MANIFEST}[[permitted_effects]]\nmcp_server_name = \"x\"\ntool = \"y\"\n");
        let reformatted = "# a comment\nversion = \"1.0.0\"\n\nname   =   \"acme\"\ndigest = \"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\"\n";

        assert_ne!(base, shared_with(DIGEST, "1.0.0", JOB, &widened, &[]));
        assert_eq!(base, shared_with(DIGEST, "1.0.0", JOB, reformatted, &[]));
    }

    #[test]
    fn a_library_release_digest_or_manifest_or_the_set_of_libraries_is_part_of_the_version() {
        let one = [text_library("1.4.2", OTHER_DIGEST, LIBRARY_MANIFEST)];
        let base = shared_with(DIGEST, "1.0.0", JOB, MANIFEST, &one);
        let widened = format!("{LIBRARY_MANIFEST}exports = [\"a.rhai\"]\n");

        assert_ne!(
            base,
            shared(DIGEST, "1.0.0", JOB),
            "resolving a library is a new version"
        );
        assert_ne!(
            base,
            shared_with(
                DIGEST,
                "1.0.0",
                JOB,
                MANIFEST,
                &[text_library("1.4.3", OTHER_DIGEST, LIBRARY_MANIFEST)]
            )
        );
        assert_ne!(
            base,
            shared_with(
                DIGEST,
                "1.0.0",
                JOB,
                MANIFEST,
                &[text_library("1.4.2", DIGEST, LIBRARY_MANIFEST)]
            )
        );
        assert_ne!(
            base,
            shared_with(
                DIGEST,
                "1.0.0",
                JOB,
                MANIFEST,
                &[text_library("1.4.2", OTHER_DIGEST, &widened)]
            )
        );
    }

    #[test]
    fn libraries_are_hashed_in_name_order_whatever_order_they_are_given_in() {
        let first = FingerprintLibrary {
            name: "acme-core",
            version: "1.0.0",
            digest: DIGEST,
            manifest: "name = \"acme-core\"\n",
        };
        let second = text_library("1.4.2", OTHER_DIGEST, LIBRARY_MANIFEST);
        let forward = [first, second];
        let backward = [
            text_library("1.4.2", OTHER_DIGEST, LIBRARY_MANIFEST),
            FingerprintLibrary {
                name: "acme-core",
                version: "1.0.0",
                digest: DIGEST,
                manifest: "name = \"acme-core\"\n",
            },
        ];

        assert_eq!(
            shared_with(DIGEST, "1.0.0", JOB, MANIFEST, &forward),
            shared_with(DIGEST, "1.0.0", JOB, MANIFEST, &backward)
        );
    }

    #[test]
    fn text_cannot_be_shifted_between_a_library_name_and_its_release_to_keep_the_same_version() {
        let left = [FingerprintLibrary {
            name: "acme",
            version: "1x",
            digest: DIGEST,
            manifest: "",
        }];
        let right = [FingerprintLibrary {
            name: "acme1",
            version: "x",
            digest: DIGEST,
            manifest: "",
        }];

        assert_ne!(
            shared_with(DIGEST, "1.0.0", JOB, MANIFEST, &left),
            shared_with(DIGEST, "1.0.0", JOB, MANIFEST, &right)
        );
    }

    #[test]
    fn a_byte_moved_between_the_digest_and_the_version_is_a_new_version() {
        assert_ne!(shared("sha256:ab", "c", JOB), shared("sha256:a", "bc", JOB));
    }

    #[test]
    fn a_manifest_that_does_not_parse_still_has_a_fingerprint_of_its_text() {
        let first = shared_with(DIGEST, "1.0.0", JOB, "not [ toml", &[]);
        let second = shared_with(DIGEST, "1.0.0", JOB, "not [ toml!", &[]);

        assert_ne!(first, second);
    }

    #[test]
    fn a_fingerprint_is_a_sha256_digest_and_reads_back_from_its_text() -> Result<(), FingerprintError> {
        let value = shared(DIGEST, "1.0.0", JOB);

        assert!(value.as_str().starts_with("sha256:"));
        assert_eq!(JobFingerprint::try_from(value.as_str().to_owned())?, value);
        assert!(JobFingerprint::try_from("sha256:short".to_owned()).is_err());
        assert!(JobFingerprint::try_from("md5:abc".to_owned()).is_err());
        Ok(())
    }
}
