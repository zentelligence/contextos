use super::*;

#[test]
fn the_feed_query_keeps_the_filters_and_names_the_page_only_past_the_first() {
    let filters = FeedFilters {
        kind: Some("task.created".to_owned()),
        severity: Some("warning".to_owned()),
        entity: Some("acme corp".to_owned()),
    };

    assert_eq!(
        feed_query(&filters, 1),
        "entity=acme%20corp&kind=task.created&severity=warning"
    );
    assert_eq!(
        feed_query(&filters, 3),
        "entity=acme%20corp&kind=task.created&severity=warning&page=3"
    );
    assert_eq!(feed_query(&FeedFilters::default(), 1), "");
}
