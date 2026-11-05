use super::*;

const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH_B: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

fn manifest_with(files: &str) -> String {
    format!(
        r#"
            name    = "sync-crm"
            version = "1.2.0"
            digest  = "sha256:{HASH_A}"

            [[files]]
            path = "library.toml"
            hash = "{HASH_A}"

            {files}
        "#
    )
}

#[test]
fn a_library_manifest_that_lists_only_its_library_toml_lists_that_one_file() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = PackageManifest::parse(&manifest_with(""))?;

    let listed: Vec<&str> = manifest.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(listed, ["library.toml"]);
    Ok(())
}

#[test]
fn each_listed_file_keeps_its_path_and_its_lower_cased_hash_in_order() -> Result<(), Box<dyn std::error::Error>> {
    let files = format!(
        r#"
            [[files]]
            path = "sync.rhai"
            hash = "{HASH_A}"

            [[files]]
            path = "templates/note.md"
            hash = "{HASH_B}"
        "#
    );

    let manifest = PackageManifest::parse(&manifest_with(&files))?;

    let listed: Vec<(&str, &str)> = manifest
        .files
        .iter()
        .map(|file| (file.path.as_str(), <&str>::from(&file.hash)))
        .collect();
    assert_eq!(
        listed,
        [
            ("library.toml", HASH_A),
            ("sync.rhai", HASH_A),
            ("templates/note.md", HASH_B.to_ascii_lowercase().as_str())
        ]
    );
    Ok(())
}

#[test]
fn a_listed_path_that_is_not_a_plain_relative_path_is_refused() {
    for bad in ["../escape", "/absolute", "a//b", "a\\b", "c:evil", "nul", "trailing."] {
        let files = format!("[[files]]\npath = {bad:?}\nhash = \"{HASH_A}\"\n");

        let result = PackageManifest::parse(&manifest_with(&files));

        assert!(
            matches!(result, Err(PackageError::InvalidFilePath(ref path)) if path == bad),
            "{bad:?} gave {result:?}"
        );
    }
}

#[test]
fn the_manifest_cannot_list_itself() {
    let files = format!("[[files]]\npath = \"manifest.toml\"\nhash = \"{HASH_A}\"\n");

    let result = PackageManifest::parse(&manifest_with(&files));

    assert!(matches!(result, Err(PackageError::ManifestListedAsFile)), "{result:?}");
}

#[test]
fn a_hash_that_is_not_sixty_four_hexadecimal_characters_is_refused() {
    for bad in ["", "abc", "sha256:aaaa", &"g".repeat(64), &"a".repeat(65)] {
        let files = format!("[[files]]\npath = \"sync.rhai\"\nhash = {bad:?}\n");

        let result = PackageManifest::parse(&manifest_with(&files));

        assert!(
            matches!(result, Err(PackageError::InvalidFileHash { ref path }) if path == "sync.rhai"),
            "{bad:?} gave {result:?}"
        );
    }
}

#[test]
fn a_file_listed_twice_is_refused_even_with_the_same_hash() {
    let entry = format!("[[files]]\npath = \"sync.rhai\"\nhash = \"{HASH_A}\"\n");

    let result = PackageManifest::parse(&manifest_with(&format!("{entry}\n{entry}")));

    assert!(
        matches!(result, Err(PackageError::DuplicateFile(ref path)) if path == "sync.rhai"),
        "{result:?}"
    );
}

#[test]
fn an_unknown_key_in_a_files_entry_is_refused() {
    let files = format!("[[files]]\npath = \"sync.rhai\"\nhash = \"{HASH_A}\"\nsize = 3\n");

    let result = PackageManifest::parse(&manifest_with(&files));

    assert!(matches!(result, Err(PackageError::Malformed(_))), "{result:?}");
}
