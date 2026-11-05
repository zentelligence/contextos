use super::*;

type BoxError = Box<dyn std::error::Error>;

#[test]
fn match_date_format_extracts_a_sort_key_for_a_note_that_fits_the_default_shape() {
    // Fixed significance order (YYYY, MMMM, MM, ww, Do, DD, HH, mm,
    // dddd), not the format's own token order - see `SIGNIFICANCE_ORDER`.
    let key = match_date_format("YYYY/MM/YYYY-MM-DD-HHmm", "2026/09/2026-09-14-1858.md");
    assert_eq!(key, Some(vec![2026, 0, 9, 0, 0, 14, 18, 58, 0]));
}

#[test]
fn match_date_format_returns_none_for_a_path_with_the_wrong_number_of_segments() {
    assert!(match_date_format("YYYY/MM/YYYY-MM-DD-HHmm", "2026/2026-09-14-1858.md").is_none());
    assert!(match_date_format("YYYY/MM/YYYY-MM-DD-HHmm", "2026/09/deep/2026-09-14-1858.md").is_none());
}

#[test]
fn match_date_format_returns_none_for_an_operator_named_folder() {
    assert!(match_date_format("YYYY/MM/YYYY-MM-DD-HHmm", "projects/website/kickoff.md").is_none());
}

#[test]
fn match_date_format_sorts_chronologically_even_when_the_format_names_day_before_year() {
    // A lexicographic key in the format's own token order would rank
    // "15-01-2025" (day 15) after "01-06-2025" (day 01), treating
    // mid-January as later than mid-year - wrong. The
    // significance-ordered key gets this right.
    let mid_january = match_date_format("DD-MM-YYYY", "15-01-2025.md");
    let mid_june = match_date_format("DD-MM-YYYY", "01-06-2025.md");
    assert!(
        mid_january < mid_june,
        "{mid_january:?} should sort before {mid_june:?}"
    );
}

#[test]
fn match_date_format_agrees_with_itself_regardless_of_the_configured_token_order() {
    let day_first = match_date_format("DD-MM-YYYY", "31-12-2025.md");
    let year_first = match_date_format("YYYY-MM-DD", "2025-12-31.md");
    assert_eq!(day_first, year_first);
}

#[test]
fn date_structure_side_defaults_to_right() {
    assert_eq!(DateStructureSide::default(), DateStructureSide::Right);
}

#[test]
fn matched_fields_still_matches_a_dated_folder_when_the_filename_lacks_the_leaf_formats_own_time_suffix()
-> Result<(), BoxError> {
    // A real, reported case: the folder alone (`YYYY`/`MM`) is enough to
    // call this dated; the filename need not also spell out `-HHmm`.
    let matched = matched_fields("YYYY/MM/YYYY-MM-DD-HHmm", "2026/05/2026-05-11.md")
        .ok_or("a dated folder with a shorter filename must still match")?;
    assert_eq!(templater::matched_field(&matched, "YYYY"), Some(2026));
    assert_eq!(templater::matched_field(&matched, "MM"), Some(5));
    // No time-of-day tokens were actually present in this filename.
    assert_eq!(templater::matched_field(&matched, "HH"), None);
    Ok(())
}

#[test]
fn matched_fields_still_extracts_the_leaf_when_it_does_fully_match() -> Result<(), BoxError> {
    let matched = matched_fields("YYYY/MM/YYYY-MM-DD-HHmm", "2026/08/2026-08-30-1858.md").ok_or("expected a match")?;
    assert_eq!(templater::matched_field(&matched, "HH"), Some(18));
    assert_eq!(templater::matched_field(&matched, "mm"), Some(58));
    Ok(())
}

#[test]
fn matched_fields_requires_the_leaf_in_full_when_the_format_has_no_folder_component() {
    // With nothing to check but the leaf, an unrelated file must not
    // trivially "match" just because there was no folder to fail on.
    assert!(matched_fields("YYYY-MM-DD", "not-a-date.md").is_none());
    assert!(matched_fields("YYYY-MM-DD", "2026-09-14.md").is_some());
}

#[test]
fn matched_fields_still_rejects_a_folder_that_genuinely_does_not_match() {
    let matched = matched_fields("YYYY/MM/YYYY-MM-DD-HHmm", "projects/website/kickoff.md");
    assert!(
        matched.is_none(),
        "an unrelated folder must not be rescued by leaf leniency"
    );
}
