use super::*;

#[tokio::test(start_paused = true)]
async fn a_stored_count_is_used_until_it_expires() {
    let cache = OpenCountCache::default();
    let miss = cache.lookup("v");
    assert_eq!(miss.count, None);
    cache.store("v", miss.generation, 3);

    assert_eq!(cache.lookup("v").count, Some(3));
    tokio::time::advance(OPEN_COUNT_TTL + Duration::from_millis(1)).await;
    assert_eq!(cache.lookup("v").count, None, "an expired count is read again");
}

#[tokio::test(start_paused = true)]
async fn counts_are_kept_per_vault() {
    let cache = OpenCountCache::default();
    let miss = cache.lookup("a");
    cache.store("a", miss.generation, 1);

    assert_eq!(cache.lookup("a").count, Some(1));
    assert_eq!(cache.lookup("b").count, None);
}

#[tokio::test(start_paused = true)]
async fn invalidating_forgets_the_count() {
    let cache = OpenCountCache::default();
    let miss = cache.lookup("v");
    cache.store("v", miss.generation, 3);

    cache.invalidate("v");

    assert_eq!(cache.lookup("v").count, None);
}

#[tokio::test(start_paused = true)]
async fn a_count_worked_out_before_an_invalidation_is_not_stored_after_it() {
    let cache = OpenCountCache::default();
    let miss = cache.lookup("v");

    cache.invalidate("v");
    cache.store("v", miss.generation, 3);

    assert_eq!(cache.lookup("v").count, None, "the count may predate the event");
}
