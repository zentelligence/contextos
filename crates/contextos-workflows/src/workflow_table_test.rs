use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn a_recorded_future_version_is_written_beside_current_and_previous() -> TestResult {
    let mut table = WorkflowTable::default();
    table.versions.current = Some(Version::parse("1.0.0")?);
    table.versions.future = Some(Version::parse("1.3.0")?);

    let text = toml::to_string(&table)?;

    assert!(text.contains("future = \"1.3.0\""), "the table was written as: {text}");
    Ok(())
}

#[test]
fn a_recorded_future_version_is_read_back_and_is_not_taken_for_a_job() -> TestResult {
    let table = toml::from_str::<WorkflowTable>("current = \"1.0.0\"\nfuture = \"1.3.0\"\n").ok();

    assert_eq!(
        table.as_ref().and_then(|table| table.versions.future.clone()),
        Some(Version::parse("1.3.0")?)
    );
    assert!(table.is_some_and(|table| table.jobs.is_empty()));
    Ok(())
}

#[test]
fn a_workflow_with_no_future_writes_no_future_key() -> TestResult {
    let mut table = WorkflowTable::default();
    table.versions.current = Some(Version::parse("1.0.0")?);

    assert!(!toml::to_string(&table)?.contains("future"));
    Ok(())
}
