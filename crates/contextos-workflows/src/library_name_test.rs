use super::*;

#[test]
fn a_library_name_is_lower_case_letters_digits_and_hyphens_starting_with_a_letter_or_digit() {
    for name in ["crm", "http-helpers", "a", "0day", "x1-y2-z3"] {
        assert!(LibraryName::try_from(name).is_ok(), "{name}");
    }
}

#[test]
fn a_name_that_could_leave_the_libraries_folder_or_name_two_folders_is_refused() {
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
        "CRM",
        "crm ",
        " crm",
        "crm.",
        "a_b",
        "a.b",
        "naïve",
        "con",
        "nul",
        "com1",
        "lpt9",
        "contextos",
    ] {
        assert!(
            matches!(LibraryName::try_from(name), Err(PackageError::InvalidLibraryName { name: ref refused, .. }) if refused == name),
            "{name:?} was accepted"
        );
    }
}

#[test]
fn a_name_longer_than_sixty_four_characters_is_refused_and_sixty_four_is_not() {
    assert!(LibraryName::try_from("a".repeat(64)).is_ok());
    assert!(LibraryName::try_from("a".repeat(65)).is_err());
}

#[test]
fn a_library_name_reads_back_as_its_text_and_orders_by_it() -> Result<(), PackageError> {
    let first = LibraryName::try_from("alpha")?;
    let second = LibraryName::try_from("beta")?;

    assert_eq!(first.as_str(), "alpha");
    assert_eq!(second.to_string(), "beta");
    assert!(first < second);
    Ok(())
}
