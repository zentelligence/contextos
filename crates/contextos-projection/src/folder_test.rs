use sha2::{Digest, Sha256};

use super::*;

fn path(value: &str) -> Result<SkillFilePath, ProjectionError> {
    SkillFilePath::try_from(value)
}

fn folder(files: &[(&str, &str)]) -> Result<SkillFolder, ProjectionError> {
    let mut entries = Vec::new();
    for (name, content) in files {
        entries.push((path(name)?, content.as_bytes().to_vec()));
    }
    SkillFolder::try_from(entries)
}

#[test]
fn the_hash_covers_sorted_paths_and_bytes_independent_of_input_order() -> Result<(), ProjectionError> {
    let one = folder(&[("b.txt", "2"), ("SKILL.md", "1")])?;
    let two = folder(&[("SKILL.md", "1"), ("b.txt", "2")])?;
    assert_eq!(ContentHash::from(&one), ContentHash::from(&two));
    Ok(())
}

#[test]
fn the_hash_is_the_documented_length_prefixed_digest() -> Result<(), ProjectionError> {
    let value = folder(&[("a", "xy")])?;
    let mut hasher = Sha256::new();
    hasher.update(1_u64.to_be_bytes());
    hasher.update(b"a");
    hasher.update(2_u64.to_be_bytes());
    hasher.update(b"xy");
    let expected: [u8; 32] = hasher.finalize().into();
    assert_eq!(ContentHash::from(&value), ContentHash::from(expected));
    Ok(())
}

#[test]
fn the_hash_changes_with_content_path_or_file_set() -> Result<(), ProjectionError> {
    let base = ContentHash::from(&folder(&[("a", "1")])?);
    assert_ne!(base, ContentHash::from(&folder(&[("a", "2")])?));
    assert_ne!(base, ContentHash::from(&folder(&[("b", "1")])?));
    assert_ne!(base, ContentHash::from(&folder(&[("a", "1"), ("b", "")])?));
    assert_ne!(
        ContentHash::from(&folder(&[("a", "bc"), ("d", "")])?),
        ContentHash::from(&folder(&[("a", "b"), ("cd", "")])?),
        "length prefixes keep path and content boundaries unambiguous"
    );
    Ok(())
}

#[test]
fn rejects_an_empty_folder_a_duplicate_path_and_a_file_that_is_also_a_folder() -> Result<(), ProjectionError> {
    assert!(matches!(
        SkillFolder::try_from(Vec::new()),
        Err(ProjectionError::InvalidSkillFolder { .. })
    ));
    assert!(matches!(
        SkillFolder::try_from(vec![(path("a")?, vec![]), (path("a")?, vec![1])]),
        Err(ProjectionError::InvalidSkillFolder { .. })
    ));
    assert!(matches!(
        folder(&[("a", ""), ("a/b", "")]),
        Err(ProjectionError::InvalidSkillFolder { .. })
    ));
    Ok(())
}

#[test]
fn rejects_two_paths_that_differ_only_by_letter_case() {
    assert!(matches!(
        folder(&[("SKILL.md", "1"), ("skill.md", "2")]),
        Err(ProjectionError::InvalidSkillFolder { .. })
    ));
}
