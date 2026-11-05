use super::*;
use crate::package::PackageManifest;

#[test]
fn a_package_name_is_lower_case_letters_digits_and_hyphens_starting_with_a_letter_or_digit() {
    for name in ["vault", "acme-text", "a", "0day", "x1-y2-z3"] {
        assert!(PackageName::try_from(name).is_ok(), "{name}");
    }
}

#[test]
fn a_name_that_could_leave_the_store_or_name_two_folders_is_refused() {
    for name in [
        "",
        "..",
        ".",
        "../escape",
        "/etc",
        "a/b",
        "a\\b",
        "C:",
        "c:x",
        "a\0b",
        "-lead",
        "Vault",
        "vault ",
        " vault",
        "vault.",
        "a_b",
        "a.b",
        "naïve",
    ] {
        assert!(
            matches!(PackageName::try_from(name), Err(PackageError::InvalidPackageName { name: ref refused, .. }) if refused == name),
            "{name:?} was accepted"
        );
    }
}

#[test]
fn a_name_longer_than_the_limit_is_refused() {
    assert!(PackageName::try_from("a".repeat(MAX_PACKAGE_NAME_LENGTH).as_str()).is_ok());
    assert!(PackageName::try_from("a".repeat(MAX_PACKAGE_NAME_LENGTH + 1).as_str()).is_err());
}

#[test]
fn windows_device_names_and_the_built_in_library_name_are_refused() {
    for name in ["con", "prn", "aux", "nul", "com1", "com9", "lpt1", "lpt9", "contextos"] {
        assert!(PackageName::try_from(name).is_err(), "{name}");
    }
    assert!(PackageName::try_from("com10").is_ok());
    assert!(PackageName::try_from("console").is_ok());
}

#[test]
fn a_manifest_naming_its_package_with_an_invalid_name_is_refused() {
    let digest = format!("sha256:{}", "0".repeat(64));
    for name in ["../escape", "/abs"] {
        let source = format!(
            "name = {name:?}\nversion = \"1.0.0\"\ndigest = {digest:?}\n[[files]]\npath = \"workflows.toml\"\nhash = \"0000000000000000000000000000000000000000000000000000000000000000\"\n"
        );

        let parsed = PackageManifest::parse(&source);

        assert!(
            matches!(parsed, Err(PackageError::InvalidPackageName { .. })),
            "{name}: {parsed:?}"
        );
    }
}

#[test]
fn a_package_file_naming_a_library_with_an_invalid_name_is_refused() {
    for library in ["../escape", "/abs"] {
        let source = format!("workflows = [\"sync\"]\n[[libraries]]\nname = {library:?}\nrequirement = \"^1\"\n");

        let parsed = crate::workflows_file::WorkflowsFile::try_from(source.as_str());

        assert!(
            matches!(parsed, Err(PackageError::InvalidWorkflowsFile { ref reason }) if reason.contains("not valid")),
            "{library}: {parsed:?}"
        );
    }
}

#[test]
fn a_librarys_name_is_always_a_valid_package_name_of_the_same_text() -> Result<(), PackageError> {
    let library = crate::library_name::LibraryName::try_from("http-helpers")?;

    let package = PackageName::from(&library);

    assert_eq!(package.as_str(), "http-helpers");
    Ok(())
}
