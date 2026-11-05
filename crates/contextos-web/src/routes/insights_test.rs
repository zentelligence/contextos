use super::*;

#[test]
fn sanitised_path_accepts_an_empty_string_as_the_root() {
    assert_eq!(sanitised_path(""), Some(String::new()));
}

#[test]
fn sanitised_path_rejects_traversal() {
    assert_eq!(sanitised_path(".."), None);
}

#[test]
fn as_note_path_requires_exactly_a_shard_and_a_markdown_filename() {
    assert_eq!(as_note_path("a/some-note.md"), Some("a/some-note.md"));
    assert_eq!(as_note_path("a"), None);
    assert_eq!(as_note_path("a/b/some-note.md"), None);
    assert_eq!(as_note_path("a/some-note.txt"), None);
}

#[test]
fn shard_href_encodes_the_shard_key() {
    assert_eq!(shard_href("vault", "a"), "/vault/insights/a");
}

#[test]
fn note_href_encodes_the_relative_path() {
    assert_eq!(note_href("vault", "a/some-note.md"), "/vault/insights/a/some-note.md");
}

#[test]
fn note_action_path_strips_a_trailing_action_suffix_from_a_real_note_path() {
    assert_eq!(note_action_path("a/some-note.md/edit", "/edit"), Some("a/some-note.md"));
    assert_eq!(
        note_action_path("a/some-note.md/confirm-delete", "/confirm-delete"),
        Some("a/some-note.md")
    );
    assert_eq!(
        note_action_path("a/some-note.md/rename", "/rename"),
        Some("a/some-note.md")
    );
}

#[test]
fn note_action_path_is_none_when_the_path_does_not_end_with_the_suffix_at_all() {
    assert_eq!(note_action_path("a/some-note.md", "/edit"), None);
}

#[test]
fn note_action_path_is_none_when_what_remains_after_stripping_is_not_a_real_note_path() {
    // A bare shard plus the suffix - the suffix is a real segment, but
    // nothing valid remains once it is stripped.
    assert_eq!(note_action_path("a/edit", "/edit"), None);
    // A note filename can never itself collide with a suffix, since every
    // real note ends in `.md` and no suffix this screen uses does - but a
    // path nested past the shard/note two-segment shape must still be
    // rejected the same way `as_note_path` already rejects it.
    assert_eq!(note_action_path("a/b/some-note.md/edit", "/edit"), None);
}

#[test]
fn status_options_marks_only_the_matching_status_as_selected() {
    let options = status_options("adopted");
    assert_eq!(options.len(), STATUSES.len());
    for option in &options {
        assert_eq!(option.selected, option.value == "adopted");
    }
}

#[test]
fn status_options_marks_nothing_selected_when_the_current_status_is_blank() {
    let options = status_options("");
    assert!(options.iter().all(|option| !option.selected));
}

#[test]
fn the_reserved_new_suffix_can_never_collide_with_a_real_shard_key() {
    // Every real shard key is exactly one character
    // (`insights::list_shards`'s own fixed `a`-`z`/`0`-`9` keyspace); the
    // reserved `path == "new"` check `get_dispatch` uses for `GET .../new`
    // is three characters, so it can never shadow a real shard the way a
    // multi-segment folder name can for Wiki - this is verified explicitly
    // rather than merely assumed, per the shard keyspace actually in force.
    let shards = insights::list_shards();
    assert!(shards.iter().all(|shard| shard.len() == 1));
    assert!(!shards.iter().any(|shard| shard == "new"));
}

#[test]
fn render_new_form_fragment_pre_fills_the_submitted_title_and_marks_the_matching_status_selected() {
    let config = InsightsConfig::default();
    let fragment = render_new_form_fragment(
        "vault",
        &config,
        "Structure Beats Willpower".to_owned(),
        "adopted",
        None,
    );
    assert!(fragment.contains("value=\"Structure Beats Willpower\""), "{fragment}");
    assert!(fragment.contains("value=\"adopted\" selected"), "{fragment}");
}

#[test]
fn render_new_form_fragment_shows_the_error_message_when_present() {
    let config = InsightsConfig::default();
    let fragment = render_new_form_fragment(
        "vault",
        &config,
        String::new(),
        "",
        Some("A note needs a title.".to_owned()),
    );
    assert!(fragment.contains("A note needs a title."), "{fragment}");
}

#[test]
fn insights_layout_class_ignores_the_metadata_side_when_no_note_is_selected() {
    assert_eq!(insights_layout_class(true, false, false), "");
    assert_eq!(insights_layout_class(false, true, false), "notes-layout-panel-right");
}

#[test]
fn insights_layout_class_is_the_plain_base_grid_when_both_panels_share_the_left() {
    assert_eq!(insights_layout_class(true, true, true), "");
}

#[test]
fn insights_layout_class_mirrors_the_base_grid_when_both_panels_share_the_right() {
    assert_eq!(insights_layout_class(false, false, true), "notes-layout-panel-right");
}

#[test]
fn insights_layout_class_dedicates_a_right_column_to_the_metadata_panel_when_the_shard_picker_stays_left() {
    assert_eq!(insights_layout_class(true, false, true), "notes-layout-metadata-right");
}

#[test]
fn insights_layout_class_dedicates_a_left_column_to_the_metadata_panel_when_the_shard_picker_moves_right() {
    assert_eq!(
        insights_layout_class(false, true, true),
        "notes-layout-metadata-left-panel-right"
    );
}
