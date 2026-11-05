//! Reading a byte part of a text file by seeking to it: the part holds the file's bytes from its offset, ends on a
//! whole character, says where the next part starts (-1 at the end), reads none of the file outside itself, and lets
//! a file of any size, or a single line of any length, be read whole part by part.

mod read_support;

use contextos_fs::FsError;
use proptest::prelude::*;
use read_support::{TestResult, Vault};

fn is_invalid_range<T: std::fmt::Debug>(result: &Result<T, FsError>) -> bool {
    matches!(result, Err(error) if error.code() == "io/invalid-range")
}

#[test]
fn a_part_returns_the_bytes_from_its_offset_and_the_offset_of_the_next_part() -> TestResult {
    let vault = Vault::new()?;
    let path = vault.write("note.md", b"alpha\nbeta\ngamma")?;

    let part = vault.part(&path, 6, 4)?;

    assert_eq!(part.content, "beta");
    assert_eq!((part.offset, part.length, part.next_offset), (6, 4, 10));
    assert_eq!(part.size, 16);
    Ok(())
}

#[test]
fn a_part_ends_on_the_last_whole_character_within_its_length() -> TestResult {
    let vault = Vault::new()?;
    // "a" is one byte and each "é" is two, so a four-byte part would end inside the second "é".
    let path = vault.write("accents.md", "aééé".as_bytes())?;

    let part = vault.part(&path, 0, 4)?;

    assert_eq!(part.content, "aé");
    assert_eq!((part.offset, part.length, part.next_offset), (0, 3, 3));
    Ok(())
}

#[test]
fn a_part_reaching_the_end_of_the_file_has_next_offset_minus_one() -> TestResult {
    let vault = Vault::new()?;
    let path = vault.write("note.md", b"alpha\nbeta")?;

    let part = vault.part(&path, 6, 100)?;

    assert_eq!(part.content, "beta");
    assert_eq!((part.offset, part.length, part.next_offset), (6, 4, -1));
    Ok(())
}

#[test]
fn a_part_starting_at_the_end_of_the_file_is_empty_with_next_offset_minus_one() -> TestResult {
    let vault = Vault::new()?;
    let path = vault.write("note.md", b"abc")?;

    let part = vault.part(&path, 3, 4)?;

    assert_eq!(part.content, "");
    assert_eq!((part.offset, part.length, part.next_offset), (3, 0, -1));
    assert!(
        part.truncated,
        "an empty part of a non-empty file is not the whole file"
    );
    Ok(())
}

#[test]
fn an_empty_file_read_as_a_part_is_whole_and_ends_at_once() -> TestResult {
    let vault = Vault::new()?;
    let path = vault.write("empty.md", b"")?;

    let part = vault.part(&path, 0, 4)?;

    assert_eq!(part.content, "");
    assert_eq!((part.offset, part.length, part.size, part.next_offset), (0, 0, 0, -1));
    assert!(!part.truncated);
    Ok(())
}

#[test]
fn only_a_part_covering_the_whole_file_is_not_truncated() -> TestResult {
    let vault = Vault::new()?;
    let small = vault.write("small.md", b"abc")?;
    let larger = vault.write("larger.md", b"abcdefgh")?;

    let whole = vault.part(&small, 0, 100)?;
    let first = vault.part(&larger, 0, 4)?;
    let last = vault.part(&larger, 4, 100)?;

    assert!(!whole.truncated);
    assert_eq!(whole.next_offset, -1);
    assert!(first.truncated);
    assert!(
        last.truncated,
        "the last part ends the file but does not hold all of it"
    );
    assert_eq!(last.next_offset, -1);
    Ok(())
}

#[test]
fn parts_of_a_single_multi_megabyte_line_join_into_the_whole_file() -> TestResult {
    let vault = Vault::with_max_read_bytes(64 * 1024)?;
    // Nine bytes per repetition, mixing one-, three-, and four-byte characters, with no line break anywhere.
    let content = "€ab🙂".repeat(300_000);
    let path = vault.write("transcript.md", content.as_bytes())?;

    let joined = vault.read_in_parts(&path, 65_537)?;

    assert_eq!(joined, content);
    Ok(())
}

#[test]
fn a_part_keeps_line_endings_exactly_as_the_file_has_them() -> TestResult {
    let vault = Vault::new()?;
    let content = "one\r\ntwo\nthree\r\n\r\nfour\r";
    let path = vault.write("mixed.md", content.as_bytes())?;

    assert_eq!(vault.read_in_parts(&path, 5)?, content);
    Ok(())
}

#[test]
fn a_part_does_not_read_bytes_outside_itself() -> TestResult {
    let vault = Vault::new()?;
    // A whole read refuses this file as binary; a part that holds only "hello" never sees the NUL bytes around it.
    let path = vault.write("framed.dat", b"\0\0\0\0hello\0\0\0\0")?;

    let part = vault.part(&path, 4, 5)?;

    assert_eq!(part.content, "hello");
    assert_eq!((part.offset, part.length, part.next_offset), (4, 5, 9));
    assert_eq!(part.size, 13);
    assert!(matches!(vault.read(&path, None), Err(FsError::Binary { .. })));
    Ok(())
}

#[test]
fn a_part_holding_a_nul_byte_or_invalid_utf8_is_refused_as_binary() -> TestResult {
    let vault = Vault::new()?;
    let nul = vault.write("zero.dat", b"ab\0d")?;
    let invalid = vault.write("invalid.dat", b"ab\xffd")?;

    for path in [nul, invalid] {
        let result = vault.part(&path, 0, 4);
        assert!(matches!(result, Err(FsError::Binary { .. })), "{result:?}");
    }
    Ok(())
}

#[test]
fn a_part_starting_beyond_the_end_of_the_file_is_refused_as_an_invalid_range() -> TestResult {
    let vault = Vault::new()?;
    let path = vault.write("note.md", b"abc")?;

    let result = vault.part(&path, 4, 4);

    assert!(is_invalid_range(&result), "{result:?}");
    Ok(())
}

#[test]
fn a_part_starting_inside_a_character_is_refused_as_an_invalid_range() -> TestResult {
    let vault = Vault::new()?;
    let two_bytes = vault.write("accent.md", "aé".as_bytes())?;
    let four_bytes = vault.write("emoji.md", "🙂".as_bytes())?;

    for (path, offset) in [(&two_bytes, 2), (&four_bytes, 1), (&four_bytes, 2), (&four_bytes, 3)] {
        let result = vault.part(path, offset, 4);
        assert!(is_invalid_range(&result), "offset {offset}: {result:?}");
    }
    Ok(())
}

#[test]
fn a_part_never_holds_more_than_the_read_maximum() -> TestResult {
    let vault = Vault::with_max_read_bytes(8)?;
    let path = vault.write("large.md", b"0123456789abcdef")?;

    let part = vault.part(&path, 0, 100)?;

    assert_eq!(part.content, "01234567");
    assert_eq!((part.length, part.next_offset), (8, 8));
    Ok(())
}

#[test]
fn a_file_larger_than_the_read_maximum_can_still_be_read_in_parts() -> TestResult {
    let vault = Vault::with_max_read_bytes(8)?;
    let content = "0123456789abcdef€";
    let path = vault.write("large.md", content.as_bytes())?;

    assert!(matches!(vault.read(&path, None), Err(FsError::TooLarge { .. })));
    assert_eq!(vault.read_in_parts(&path, 4)?, content);
    Ok(())
}

#[test]
fn a_whole_read_above_the_read_maximum_names_reading_in_parts_in_its_remediation() -> TestResult {
    let vault = Vault::with_max_read_bytes(8)?;
    let path = vault.write("large.md", b"0123456789abcdef")?;

    let error = vault
        .read(&path, None)
        .err()
        .ok_or("a file above the read maximum was read whole")?;

    assert_eq!(error.code(), "io/too-large");
    let remediation = error.remediation();
    assert!(
        remediation.contains("offset") && remediation.contains("length"),
        "{remediation}"
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn parts_of_any_text_join_into_exactly_that_text(
        content in "(?s)[a-zé€🙂\r\n ]{0,400}",
        length in 4_u64..40,
    ) {
        let fail = |error: Box<dyn std::error::Error>| TestCaseError::fail(error.to_string());
        let vault = Vault::new().map_err(fail)?;
        let path = vault.write("note.md", content.as_bytes()).map_err(fail)?;
        let joined = vault.read_in_parts(&path, length).map_err(fail)?;
        prop_assert_eq!(joined, content);
    }
}

#[test]
fn a_part_too_short_for_the_longest_character_is_refused_once_the_version_matches() -> TestResult {
    let vault = Vault::new()?;
    let path = vault.write("note.md", b"abcdefgh")?;
    let version = vault.read(&path, None)?.version;

    for length in 0..4 {
        let result = vault.try_part(&path, 0, length, Some(version.clone()));
        assert!(
            matches!(result, Err(FsError::PartTooShort { minimum: 4, .. })),
            "length {length}: {result:?}"
        );
    }
    Ok(())
}

#[test]
fn a_lead_byte_followed_by_too_many_continuation_bytes_is_refused_as_binary_rather_than_stalling() -> TestResult {
    let vault = Vault::new()?;
    let path = vault.write("bad.md", &[0xE2, 0x80, 0x80, 0x80, 0x80])?;

    let result = vault.part(&path, 0, 4);

    assert!(
        matches!(result, Err(ref error) if error.code() == "io/binary"),
        "a part that cannot advance must be refused: {result:?}"
    );
    Ok(())
}

#[test]
fn a_part_that_cuts_a_character_at_its_end_drops_only_that_incomplete_character() -> TestResult {
    let vault = Vault::new()?;
    // "ab" then a three-byte character: a 4-byte part ends after its first two bytes.
    let path = vault.write("cut.md", "ab€cd".as_bytes())?;

    let part = vault.part(&path, 0, 4)?;

    assert_eq!((part.content.as_str(), part.next_offset), ("ab", 2));
    Ok(())
}

#[test]
fn a_file_ending_in_an_incomplete_character_is_refused_as_binary() -> TestResult {
    let vault = Vault::new()?;
    let path = vault.write("truncated.md", &[b'a', 0xE2, 0x80])?;

    let result = vault.part(&path, 0, 8);

    assert!(
        matches!(result, Err(ref error) if error.code() == "io/binary"),
        "{result:?}"
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn a_part_of_any_bytes_either_fails_or_advances_and_holds_the_files_bytes(
        bytes in proptest::collection::vec(any::<u8>(), 0..48),
        offset_seed in any::<u64>(),
        length in 4_u64..12,
    ) {
        let fail = |error: Box<dyn std::error::Error>| TestCaseError::fail(error.to_string());
        let vault = Vault::new().map_err(fail)?;
        let path = vault.write("any.md", &bytes).map_err(fail)?;
        let size = u64::try_from(bytes.len()).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let offset = offset_seed % (size + 1);

        if let Ok(part) = vault.part(&path, offset, length) {
            prop_assert!(part.length <= length);
            let start = usize::try_from(offset).map_err(|error| TestCaseError::fail(error.to_string()))?;
            let end = start + part.content.len();
            prop_assert_eq!(part.content.as_bytes(), &bytes[start..end]);
            if part.next_offset != -1 {
                prop_assert!(part.next_offset > i64::try_from(offset).map_err(|error| TestCaseError::fail(error.to_string()))?);
            }
        }
    }
}
