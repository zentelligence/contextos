use proptest::collection::vec;
use proptest::prelude::*;

use super::*;

fn paths(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn approx(left: f32, right: f32) -> bool {
    (left - right).abs() < 1e-6
}

fn order(fused: &[FusedFile]) -> Vec<&str> {
    fused.iter().map(|file| file.path.as_str()).collect()
}

#[test]
fn a_worked_example_scores_and_orders_files_by_summed_reciprocal_ranks() {
    let text = paths(&["a", "b", "c"]);
    let semantic = paths(&["b", "d", "a"]);

    let fused = fuse_by_rank(&[&text, &semantic]);

    // b = 1/62 + 1/61, a = 1/61 + 1/63, d = 1/62, c = 1/63; best attainable = 2/61.
    assert_eq!(order(&fused), ["b", "a", "d", "c"]);
    let best_attainable = 2.0 / 61.0;
    assert!(approx(fused[0].relevance, (1.0 / 62.0 + 1.0 / 61.0) / best_attainable));
    assert!(approx(fused[1].relevance, (1.0 / 61.0 + 1.0 / 63.0) / best_attainable));
    assert!(approx(fused[2].relevance, (1.0 / 62.0) / best_attainable));
    assert!(approx(fused[3].relevance, (1.0 / 63.0) / best_attainable));
}

#[test]
fn a_file_ranked_first_by_every_mode_has_relevance_exactly_one() {
    let text = paths(&["a", "b"]);
    let semantic = paths(&["a", "c"]);

    let fused = fuse_by_rank(&[&text, &semantic]);

    assert!(approx(fused[0].relevance, 1.0));
    assert_eq!(fused[0].path, "a");
}

#[test]
fn a_file_first_in_only_one_of_two_modes_scores_one_half() {
    let text = paths(&["a"]);
    let semantic = paths(&["b"]);

    let fused = fuse_by_rank(&[&text, &semantic]);

    assert!(approx(fused[0].relevance, 0.5));
    assert!(approx(fused[1].relevance, 0.5));
}

#[test]
fn a_mode_that_ran_but_found_nothing_still_counts_towards_what_was_attainable() {
    let text = paths(&["a"]);
    let semantic: Vec<String> = Vec::new();

    let fused = fuse_by_rank(&[&text, &semantic]);

    assert_eq!(fused.len(), 1);
    assert!(approx(fused[0].relevance, 0.5));
}

#[test]
fn a_single_mode_scores_its_first_hit_at_one() {
    let text = paths(&["a", "b"]);

    let fused = fuse_by_rank(&[&text]);

    assert!(approx(fused[0].relevance, 1.0));
    assert!(approx(fused[1].relevance, 61.0 / 62.0));
}

#[test]
fn each_file_reports_its_rank_in_every_list() {
    let text = paths(&["a", "b"]);
    let semantic = paths(&["b", "c"]);

    let fused = fuse_by_rank(&[&text, &semantic]);

    let by_path = |name: &str| {
        fused
            .iter()
            .find(|file| file.path == name)
            .map(|file| file.ranks.clone())
    };
    assert_eq!(by_path("a"), Some(vec![Some(1), None]));
    assert_eq!(by_path("b"), Some(vec![Some(2), Some(1)]));
    assert_eq!(by_path("c"), Some(vec![None, Some(2)]));
}

#[test]
fn equal_scores_are_ordered_by_ascending_path_whichever_list_they_came_from() {
    let text = paths(&["y"]);
    let semantic = paths(&["x"]);
    assert_eq!(order(&fuse_by_rank(&[&text, &semantic])), ["x", "y"]);
    assert_eq!(order(&fuse_by_rank(&[&semantic, &text])), ["x", "y"]);
}

#[test]
fn a_path_repeated_within_one_list_counts_once_at_its_first_position() {
    let text = paths(&["a", "b", "a"]);

    let fused = fuse_by_rank(&[&text]);

    assert_eq!(order(&fused), ["a", "b"]);
    assert_eq!(fused[0].ranks, vec![Some(1)]);
}

#[test]
fn no_lists_or_only_empty_lists_fuse_to_nothing() {
    assert!(
        fuse_by_rank(&[]).is_empty(),
        "`fuse_by_rank(&[])` should be empty, got {:?}",
        fuse_by_rank(&[])
    );
    let empty: Vec<String> = Vec::new();
    assert!(
        fuse_by_rank(&[&empty, &empty]).is_empty(),
        "`fuse_by_rank(&[&empty, &empty])` should be empty, got {:?}",
        fuse_by_rank(&[&empty, &empty])
    );
}

proptest! {
    #[test]
    fn fused_output_is_unique_bounded_and_totally_ordered(
        first in vec("[a-f]", 0..10),
        second in vec("[a-f]", 0..10),
    ) {
        let fused = fuse_by_rank(&[&first, &second]);

        let mut seen = std::collections::BTreeSet::new();
        for file in &fused {
            prop_assert!(seen.insert(file.path.clone()), "path {} appears twice", file.path);
            prop_assert!(file.relevance > 0.0 && file.relevance <= 1.0 + 1e-6);
        }
        for pair in fused.windows(2) {
            let ordered = pair[0].relevance > pair[1].relevance
                || ((pair[0].relevance - pair[1].relevance).abs() < f32::EPSILON && pair[0].path < pair[1].path);
            prop_assert!(ordered, "{:?} must not precede {:?}", pair[0], pair[1]);
        }
        let mut expected: std::collections::BTreeSet<&String> = first.iter().collect();
        expected.extend(second.iter());
        prop_assert_eq!(seen.len(), expected.len());
    }
}
