use super::*;

fn entry(path: &str, ordinal: usize, score: f32) -> SemanticEntry {
    SemanticEntry {
        hit: SemanticHit {
            path: path.to_owned(),
            chunk: String::new(),
            score,
            relevance: score.clamp(0.0, 1.0),
            heading_context: Vec::new(),
        },
        ordinal,
        metadata: None,
    }
}

#[test]
fn hits_whose_similarity_is_negative_tie_on_relevance_and_fall_back_to_path_then_chunk() {
    let mut entries = [
        entry("b.md", 0, -0.2),
        entry("a.md", 1, -0.9),
        entry("a.md", 0, -0.5),
        entry("c.md", 0, 0.4),
    ];

    entries.sort_by(|left, right| compare_entries(SortSpec::default(), left, right));

    let order: Vec<(&str, usize)> = entries.iter().map(|e| (e.hit.path.as_str(), e.ordinal)).collect();
    assert_eq!(order, [("c.md", 0), ("a.md", 0), ("a.md", 1), ("b.md", 0)]);
}

#[test]
fn ascending_relevance_reverses_the_primary_key_but_not_the_tie_break() {
    let mut entries = [entry("b.md", 0, 0.5), entry("a.md", 0, 0.5), entry("c.md", 0, 0.1)];
    let ascending = SortSpec::from((SortKey::Relevance, Some(SortOrder::Ascending)));

    entries.sort_by(|left, right| compare_entries(ascending, left, right));

    let order: Vec<&str> = entries.iter().map(|e| e.hit.path.as_str()).collect();
    assert_eq!(order, ["c.md", "a.md", "b.md"]);
}
