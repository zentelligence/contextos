use super::*;

#[test]
fn the_four_levels_are_read_by_name_and_ordered_from_debug_to_error() -> Result<(), LogLevelError> {
    assert!(LogLevel::try_from("debug")? < LogLevel::try_from("info")?);
    assert!(LogLevel::try_from("info")? < LogLevel::try_from("warn")?);
    assert!(LogLevel::try_from("warn")? < LogLevel::try_from("error")?);
    Ok(())
}

#[test]
fn an_unknown_level_is_refused_naming_it() {
    let error = LogLevel::try_from("verbose").err();

    assert!(
        matches!(&error, Some(LogLevelError { value }) if value == "verbose"),
        "{error:?}"
    );
    assert!(
        error
            .map(|error| error.to_string())
            .unwrap_or_default()
            .contains("verbose")
    );
}

#[test]
fn level_names_are_case_sensitive_and_never_trimmed() {
    assert!(LogLevel::try_from("INFO").is_err());
    assert!(LogLevel::try_from(" info").is_err());
}

#[test]
fn the_job_wins_over_the_workflow_which_wins_over_the_host() {
    use LogLevel::{Debug, Error, Info, Warn};

    assert_eq!(effective_log_level(Some(Warn), Some(Debug), Some(Error)), Error);
    assert_eq!(effective_log_level(Some(Warn), Some(Debug), None), Debug);
    assert_eq!(effective_log_level(Some(Warn), None, None), Warn);
    assert_eq!(effective_log_level(None, None, None), Info);
}

#[test]
fn a_message_is_kept_only_at_or_above_the_minimum() {
    assert!(LogLevel::Warn.is_kept_at(LogLevel::Warn));
    assert!(LogLevel::Error.is_kept_at(LogLevel::Warn));
    assert!(!LogLevel::Info.is_kept_at(LogLevel::Warn));
}

#[test]
fn text_within_the_limit_is_unchanged_and_longer_text_ends_with_the_cut_marker() {
    assert_eq!(cut_to_bytes("short", 10), "short");

    let cut = cut_to_bytes(&"a".repeat(50), 10);

    assert!(cut.ends_with(CUT_MARKER));
    assert!(cut.len() <= 10 + CUT_MARKER.len());
    assert!(cut.starts_with("aaaaaaaaaa"));
}

#[test]
fn a_cut_never_splits_a_multi_byte_character() {
    let cut = cut_to_bytes(&"é".repeat(20), 5);

    assert!(cut.ends_with(CUT_MARKER));
    assert!(cut.trim_end_matches(CUT_MARKER).chars().all(|c| c == 'é'));
}

#[test]
fn reasons_are_kept_up_to_the_cap_and_the_rest_are_only_counted() {
    let mut report = RunReport::default();
    for index in 0..25 {
        report.note(&format!("reason {index}"));
    }

    let reasons = report.into_reasons();

    assert_eq!(reasons.as_ref().map(|r| r.kept.len()), Some(MAX_REASONS));
    assert_eq!(reasons.as_ref().map(|r| r.omitted), Some(5));
    assert_eq!(reasons.map(|r| r.kept[0].clone()), Some("reason 0".to_owned()));
}

#[test]
fn a_long_reason_is_cut_and_a_report_with_none_has_no_reasons() {
    let mut report = RunReport::default();
    assert!(RunReport::default().into_reasons().is_none());

    report.note(&"x".repeat(MAX_REASON_BYTES * 2));

    let kept = report.into_reasons().map(|r| r.kept).unwrap_or_default();
    assert!(kept[0].ends_with(CUT_MARKER));
}

#[derive(serde::Deserialize)]
struct Manifest {
    log_level: Option<LogLevel>,
}

#[test]
fn a_manifest_reads_its_log_level_by_name_or_leaves_it_unset() -> Result<(), toml::de::Error> {
    let set: Manifest = toml::from_str("log_level = \"warn\"")?;
    let unset: Manifest = toml::from_str("")?;

    assert_eq!(set.log_level, Some(LogLevel::Warn));
    assert_eq!(unset.log_level, None);
    Ok(())
}

#[test]
fn a_manifest_with_an_unknown_level_fails_naming_the_value() {
    let message = toml::from_str::<Manifest>("log_level = \"loud\"")
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();

    assert!(message.contains("loud"), "{message}");
}
