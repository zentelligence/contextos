use super::*;

#[test]
fn natural_direction_is_most_relevant_and_most_recent_first_and_titles_alphabetical() {
    assert_eq!(SortSpec::from(SortKey::Relevance).order(), SortOrder::Descending);
    assert_eq!(SortSpec::from(SortKey::Modified).order(), SortOrder::Descending);
    assert_eq!(SortSpec::from(SortKey::Title).order(), SortOrder::Ascending);
}

#[test]
fn an_explicit_direction_overrides_the_natural_one() {
    let spec = SortSpec::from((SortKey::Title, Some(SortOrder::Descending)));
    assert_eq!(spec.key(), SortKey::Title);
    assert_eq!(spec.order(), SortOrder::Descending);
}

#[test]
fn default_sort_is_relevance_descending() {
    let spec = SortSpec::default();
    assert_eq!(spec.key(), SortKey::Relevance);
    assert_eq!(spec.order(), SortOrder::Descending);
}

#[test]
fn min_relevance_accepts_the_closed_unit_interval() -> Result<(), SearchError> {
    assert!((f32::from(MinRelevance::try_from(0.0)?) - 0.0).abs() < f32::EPSILON);
    assert!((f32::from(MinRelevance::try_from(0.5)?) - 0.5).abs() < f32::EPSILON);
    assert!((f32::from(MinRelevance::try_from(1.0)?) - 1.0).abs() < f32::EPSILON);
    Ok(())
}

#[test]
fn min_relevance_rejects_values_outside_the_unit_interval_and_nan() {
    for value in [-0.001, 1.001, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let error = MinRelevance::try_from(value);
        assert!(
            matches!(error, Err(SearchError::InvalidRelevance { .. })),
            "{value} must be rejected"
        );
    }
}

#[test]
fn default_min_relevance_admits_everything() {
    assert!(f32::from(MinRelevance::default()).abs() < f32::EPSILON);
}

fn controls_at(offset: usize) -> ResultControls<'static> {
    ResultControls {
        offset,
        ..ResultControls::default()
    }
}

#[test]
fn the_largest_permitted_page_at_the_start_of_the_window_is_accepted() {
    assert!(controls_at(0).ensure_within(MAX_PAGE_LIMIT).is_ok());
}

#[test]
fn a_limit_above_the_maximum_page_is_rejected() {
    let error = controls_at(0).ensure_within(MAX_PAGE_LIMIT + 1);
    assert!(matches!(
        error,
        Err(SearchError::PageLimitExceeded {
            limit: 101,
            maximum: 100
        })
    ));
}

#[test]
fn a_page_may_end_exactly_at_the_window_edge() {
    assert!(controls_at(CANDIDATE_WINDOW - 10).ensure_within(10).is_ok());
}

#[test]
fn a_page_ending_beyond_the_window_is_rejected() {
    let error = controls_at(CANDIDATE_WINDOW - 9).ensure_within(10);
    assert!(matches!(
        error,
        Err(SearchError::PageBeyondWindow { end: 201, window: 200 })
    ));
}

#[test]
fn an_offset_far_beyond_the_window_is_rejected_without_overflow() {
    let error = controls_at(usize::MAX).ensure_within(MAX_PAGE_LIMIT);
    assert!(matches!(error, Err(SearchError::PageBeyondWindow { .. })));
}

#[test]
fn a_zero_limit_is_a_count_only_page_at_any_offset_inside_the_window() {
    assert!(controls_at(0).ensure_within(0).is_ok());
    assert!(controls_at(CANDIDATE_WINDOW).ensure_within(0).is_ok());
}
