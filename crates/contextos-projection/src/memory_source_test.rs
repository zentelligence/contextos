use super::*;

type TestResult = Result<(), ProjectionError>;

#[test]
fn definitions_are_kept_per_vault_and_listed_by_raw_name() -> TestResult {
    let source = MemorySource::new(&["one", "two"]);
    source.set_agent("one", "reviewer", b"x");
    source.set_agent("one", "Bad Name", b"y");
    source.set_skill("two", "writing", &[("SKILL.md", b"z")]);
    let vaults = source.vaults();
    assert_eq!(vaults.len(), 2);
    assert_eq!(source.agent_names(&vaults[0])?.len(), 2);
    assert_eq!(source.agent_names(&vaults[1])?, vec![]);
    assert_eq!(
        source.read_agent(&vaults[0], &Slug::try_from("reviewer")?)?,
        Some(b"x".to_vec())
    );
    assert_eq!(source.read_agent(&vaults[1], &Slug::try_from("reviewer")?)?, None);
    assert!(source.read_skill(&vaults[1], &Slug::try_from("writing")?)?.is_some());
    source.remove_agent("one", "reviewer");
    source.remove_skill("two", "writing");
    assert_eq!(source.read_agent(&vaults[0], &Slug::try_from("reviewer")?)?, None);
    assert!(source.read_skill(&vaults[1], &Slug::try_from("writing")?)?.is_none());
    Ok(())
}

#[test]
fn an_unreadable_vault_fails_every_read_with_a_typed_error() -> TestResult {
    let source = MemorySource::new(&["one"]);
    source.fail_reads_in("one");
    let vault = &source.vaults()[0];
    assert!(matches!(
        source.agent_names(vault),
        Err(ProjectionError::SourceRead { .. })
    ));
    assert!(matches!(
        source.read_skill(vault, &Slug::try_from("writing")?),
        Err(ProjectionError::SourceRead { .. })
    ));
    Ok(())
}

#[test]
fn the_global_instructions_are_kept_per_vault() -> TestResult {
    let source = MemorySource::new(&["one", "two"]);
    source.set_orientation("two", b"Orient.\n");
    let vaults = source.vaults();
    assert_eq!(source.read_orientation(&vaults[0])?, None);
    assert_eq!(source.read_orientation(&vaults[1])?, Some(b"Orient.\n".to_vec()));
    source.remove_orientation("two");
    assert_eq!(source.read_orientation(&vaults[1])?, None);
    Ok(())
}

#[test]
fn an_unreadable_vault_fails_the_global_instructions_read_too() {
    let source = MemorySource::new(&["one"]);
    source.fail_reads_in("one");
    assert!(matches!(
        source.read_orientation(&source.vaults()[0]),
        Err(ProjectionError::SourceRead { .. })
    ));
}
