use super::*;

fn request(offset: Option<i64>, limit: Option<i64>) -> Result<LinkReportRequest, SearchError> {
    LinkReportRequest::try_from(LinkReportArgs {
        operation: "unresolved",
        offset,
        limit,
    })
}

#[test]
fn an_empty_request_pages_from_the_start_with_the_default_limit() -> Result<(), SearchError> {
    let request = request(None, None)?;

    assert_eq!((request.offset, request.limit), (0, 200));
    Ok(())
}

#[test]
fn a_zero_offset_and_the_largest_limit_are_accepted() -> Result<(), SearchError> {
    let request = request(Some(0), Some(1000))?;

    assert_eq!((request.offset, request.limit), (0, 1000));
    Ok(())
}

#[test]
fn a_limit_above_one_thousand_or_below_zero_is_refused() {
    for limit in [1001, -1] {
        assert!(matches!(
            request(None, Some(limit)),
            Err(SearchError::InvalidLinkReport { .. })
        ));
    }
}

#[test]
fn a_negative_offset_is_refused() {
    assert!(matches!(
        request(Some(-1), None),
        Err(SearchError::InvalidLinkReport { .. })
    ));
}

#[test]
fn a_page_is_the_slice_at_the_offset_with_the_total_of_the_whole() -> Result<(), SearchError> {
    let request = request(Some(2), Some(2))?;

    let (items, total) = request.page((0..5).collect());

    assert_eq!((items, total), (vec![2, 3], 5));
    Ok(())
}

#[test]
fn an_offset_beyond_the_end_gives_an_empty_page_with_the_total() -> Result<(), SearchError> {
    let request = request(Some(9), None)?;

    let (items, total) = request.page(vec![1, 2]);

    assert_eq!((items, total), (Vec::<i32>::new(), 2));
    Ok(())
}

#[test]
fn a_zero_limit_reports_only_the_total() -> Result<(), SearchError> {
    let request = request(None, Some(0))?;

    let (items, total) = request.page(vec![1, 2, 3]);

    assert_eq!((items, total), (Vec::<i32>::new(), 3));
    Ok(())
}
