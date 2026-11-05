use super::*;

type BoxError = Box<dyn std::error::Error>;

fn summary(relative_path: &str, title: &str) -> NoteSummary {
    NoteSummary {
        relative_path: relative_path.to_owned(),
        title: title.to_owned(),
        has_custom_title: true,
        date: Some("2026-09-14".to_owned()),
        time: Some("07:05".to_owned()),
        status: Some("draft".to_owned()),
        tags: vec!["note".to_owned()],
    }
}

fn folder<'a>(root: &'a TreeFolder, label: &str) -> Option<&'a TreeFolder> {
    root.folders.iter().find(|folder| folder.label == label)
}

#[test]
fn build_tree_follows_the_default_year_month_partition_when_that_is_what_is_really_there() -> Result<(), BoxError> {
    let summaries = vec![
        summary("2026/09/newest.md", "Newest"),
        summary("2025/12/oldest.md", "Oldest"),
    ];
    let root = build_tree("vault", &summaries, None);
    let year_2026 = folder(&root, "2026").ok_or("missing 2026")?;
    let month_09 = folder(year_2026, "09").ok_or("missing 09")?;
    assert_eq!(month_09.notes[0].title, "Newest");
    let year_2025 = folder(&root, "2025").ok_or("missing 2025")?;
    let month_12 = folder(year_2025, "12").ok_or("missing 12")?;
    assert_eq!(month_12.notes[0].title, "Oldest");
    Ok(())
}

#[test]
fn build_tree_follows_an_entirely_arbitrary_operator_defined_folder_structure() -> Result<(), BoxError> {
    // No date partition anywhere: a project-named folder, three levels
    // deep, is exactly as real a shape as the default.
    let summaries = vec![summary("projects/website-redesign/notes/kickoff.md", "Kickoff")];
    let root = build_tree("vault", &summaries, None);
    let projects = folder(&root, "projects").ok_or("missing projects")?;
    let website = folder(projects, "website-redesign").ok_or("missing website-redesign")?;
    let notes = folder(website, "notes").ok_or("missing notes")?;
    assert_eq!(notes.notes[0].title, "Kickoff");
    Ok(())
}

#[test]
fn build_tree_places_a_note_directly_under_notes_dir_as_a_root_leaf_not_a_special_case() {
    let summaries = vec![summary("flat.md", "Flat")];
    let root = build_tree("vault", &summaries, None);
    assert!(
        root.folders.is_empty(),
        "`root.folders` should be empty, got {:?}",
        root.folders
    );
    assert_eq!(root.notes.len(), 1);
    assert_eq!(root.notes[0].title, "Flat");
}

#[test]
fn build_tree_lets_folders_of_wildly_different_depth_coexist() -> Result<(), BoxError> {
    let summaries = vec![summary("flat.md", "Flat"), summary("deep/nested/note.md", "Deep")];
    let root = build_tree("vault", &summaries, None);
    assert_eq!(root.notes.len(), 1);
    assert_eq!(root.notes[0].title, "Flat");
    let deep = folder(&root, "deep").ok_or("missing deep")?;
    let nested = folder(deep, "nested").ok_or("missing nested")?;
    assert_eq!(nested.notes[0].title, "Deep");
    Ok(())
}

#[test]
fn build_tree_orders_folders_by_first_seen_order_which_tracks_most_recent_first() {
    // `summaries` arrives newest-first; the folder holding the newest note
    // (September) should sort before the one holding the older note
    // (July), with no separate sort pass over folder labels.
    let summaries = vec![
        summary("2026/09/newer.md", "Newer"),
        summary("2026/07/older.md", "Older"),
    ];
    let root = build_tree("vault", &summaries, None);
    let year = &root.folders[0];
    assert_eq!(year.folders[0].label, "09");
    assert_eq!(year.folders[1].label, "07");
}

#[test]
fn build_tree_opens_every_folder_on_the_selected_notes_own_path_and_no_others() -> Result<(), BoxError> {
    let summaries = vec![summary("projects/a/note.md", "A"), summary("projects/b/note.md", "B")];
    let root = build_tree("vault", &summaries, Some("projects/a/note.md"));
    assert!(root.is_open);
    let projects = folder(&root, "projects").ok_or("missing projects")?;
    assert!(projects.is_open);
    let a = folder(projects, "a").ok_or("missing a")?;
    assert!(a.is_open);
    let b = folder(projects, "b").ok_or("missing b")?;
    assert!(!b.is_open);
    Ok(())
}

#[test]
fn build_tree_anchors_on_the_most_recent_note_when_nothing_is_selected() -> Result<(), BoxError> {
    let summaries = vec![
        summary("2026/09/newest.md", "Newest"),
        summary("2025/12/oldest.md", "Oldest"),
    ];
    let root = build_tree("vault", &summaries, None);
    let year_2026 = folder(&root, "2026").ok_or("missing 2026")?;
    assert!(year_2026.is_open);
    let year_2025 = folder(&root, "2025").ok_or("missing 2025")?;
    assert!(!year_2025.is_open);
    Ok(())
}

#[test]
fn build_tree_marks_exactly_the_selected_note_active_with_a_path_encoded_href() -> Result<(), BoxError> {
    let summaries = vec![summary("2026/09/a.md", "A"), summary("2026/09/b.md", "B")];
    let root = build_tree("vault", &summaries, Some("2026/09/b.md"));
    let month = folder(folder(&root, "2026").ok_or("missing 2026")?, "09").ok_or("missing 09")?;
    assert!(!month.notes[0].active);
    assert!(month.notes[1].active);
    assert_eq!(month.notes[1].href, "/vault/notes/2026/09/b.md");
    Ok(())
}

#[test]
fn build_tree_returns_an_empty_root_for_no_notes() {
    let root = build_tree("vault", &[], None);
    assert!(
        root.folders.is_empty(),
        "`root.folders` should be empty, got {:?}",
        root.folders
    );
    assert!(
        root.notes.is_empty(),
        "`root.notes` should be empty, got {:?}",
        root.notes
    );
}

#[test]
fn flatten_pairs_every_open_folder_with_exactly_one_later_close_in_order() {
    let summaries = vec![summary("flat.md", "Flat"), summary("deep/nested/note.md", "Deep")];
    let root = build_tree("vault", &summaries, None);
    let events = flatten(&root);
    // A root-level note comes before any folder is opened, matching the
    // physical fact that it sits directly under `notes_dir`.
    assert!(matches!(&events[0], TreeEvent::Note(note) if note.title == "Flat"));
    let opens = events
        .iter()
        .filter(|event| matches!(event, TreeEvent::OpenFolder(..)))
        .count();
    let closes = events
        .iter()
        .filter(|event| matches!(event, TreeEvent::CloseFolder))
        .count();
    assert_eq!(opens, 2, "{events:?}");
    assert_eq!(closes, 2, "{events:?}");
    // Well-formed nesting: at no prefix of the sequence does a close
    // outnumber the opens seen so far (never closes something not open).
    let mut depth = 0i32;
    for event in &events {
        match event {
            TreeEvent::OpenFolder(..) => depth += 1,
            TreeEvent::CloseFolder => {
                depth -= 1;
                assert!(depth >= 0, "closed a folder that was never opened: {events:?}");
            }
            TreeEvent::Note(_) => {}
        }
    }
    assert_eq!(depth, 0, "every opened folder must be closed: {events:?}");
}

#[test]
fn flatten_carries_each_folders_own_label_and_open_state() {
    let summaries = vec![summary("projects/a/note.md", "A")];
    let root = build_tree("vault", &summaries, Some("projects/a/note.md"));
    let events = flatten(&root);
    let labels: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            TreeEvent::OpenFolder(label, is_open) => is_open.then_some(label.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(labels, vec!["projects", "a"]);
}

#[test]
fn flatten_of_an_empty_tree_is_empty() {
    assert!(
        flatten(&build_tree("vault", &[], None)).is_empty(),
        "`flatten(&build_tree(\"vault\", &[], None))` should be empty, got {:?}",
        flatten(&build_tree("vault", &[], None))
    );
}

// ---------------------------------------------------------------------
// build_split_view
// ---------------------------------------------------------------------

fn active_config() -> NotesConfig {
    NotesConfig {
        date_structure_active: true,
        date_format: "YYYY/MM/YYYY-MM-DD-HHmm".to_owned(),
        ..NotesConfig::default()
    }
}

#[test]
fn build_split_view_with_the_split_inactive_puts_everything_in_regular_unsorted() {
    let summaries = vec![
        summary("2026/09/newer.md", "Newer"),
        summary("2026/07/older.md", "Older"),
    ];
    let config = NotesConfig {
        date_structure_active: false,
        ..NotesConfig::default()
    };
    let split = build_split_view("vault", &summaries, None, &config);
    assert!(split.dated.is_none());
    // First-seen (caller) order preserved, exactly as `build_tree` alone
    // already behaved before this split existed.
    assert_eq!(split.regular.folders[0].label, "2026");
}

#[test]
fn build_split_view_separates_dated_notes_from_operator_named_folders() -> Result<(), BoxError> {
    let summaries = vec![
        summary("2026/09/2026-09-14-1858.md", "Dated"),
        summary("projects/website/kickoff.md", "Kickoff"),
    ];
    let config = active_config();
    let split = build_split_view("vault", &summaries, None, &config);
    let dated = split.dated.ok_or("expected a dated tree")?;
    let year = folder(&dated, "2026").ok_or("missing 2026 in dated tree")?;
    assert_eq!(folder(year, "09").ok_or("missing 09")?.notes[0].title, "Dated");
    assert!(folder(&dated, "projects").is_none(), "{dated:?}");

    let projects = folder(&split.regular, "projects").ok_or("missing projects in regular tree")?;
    assert_eq!(
        folder(projects, "website").ok_or("missing website")?.notes[0].title,
        "Kickoff"
    );
    assert!(folder(&split.regular, "2026").is_none(), "{:?}", split.regular);
    Ok(())
}

#[test]
fn build_split_view_orders_the_dated_pane_by_real_calendar_value_not_insertion_order() -> Result<(), BoxError> {
    // Deliberately handed newest-last, unlike `list_notes`'s own real
    // contract, to prove the dated pane sorts by the path's own extracted
    // date rather than trusting caller order the way the inactive-split
    // case (and the regular pane) still do.
    let summaries = vec![
        summary("2026/01/2026-01-05-0800.md", "January"),
        summary("2026/09/2026-09-14-1858.md", "September"),
    ];
    let split = build_split_view("vault", &summaries, None, &active_config());
    let dated = split.dated.ok_or("expected a dated tree")?;
    assert_eq!(dated.folders[0].label, "2026");
    assert_eq!(dated.folders[0].folders[0].label, "09");
    assert_eq!(dated.folders[0].folders[1].label, "01");
    Ok(())
}

#[test]
fn build_split_view_treats_a_note_without_the_optional_time_suffix_as_still_dated() -> Result<(), BoxError> {
    // A real, reported case: `2026/05/2026-05-11.md` lives in a genuinely
    // dated folder, but its own filename lacks the default format's
    // trailing `-HHmm` - a dictated or hand-renamed note commonly does.
    // The folder alone is enough; the leaf need not fully match.
    let summaries = vec![summary("2026/05/2026-05-11.md", "No time suffix")];
    let split = build_split_view("vault", &summaries, None, &active_config());
    let dated = split.dated.ok_or("expected a dated tree")?;
    let month = folder(&dated, "2026")
        .and_then(|year| folder(year, "05"))
        .ok_or("missing 2026/05")?;
    assert_eq!(month.notes[0].title, "No time suffix");
    assert!(folder(&split.regular, "2026").is_none(), "{:?}", split.regular);
    Ok(())
}

#[test]
fn build_split_view_orders_the_regular_pane_alphabetically_ascending() -> Result<(), BoxError> {
    let summaries = vec![
        summary("projects/zebra.md", "Zebra"),
        summary("projects/apple.md", "Apple"),
    ];
    let split = build_split_view("vault", &summaries, None, &active_config());
    let projects = folder(&split.regular, "projects").ok_or("missing projects")?;
    assert_eq!(projects.notes[0].title, "Apple");
    assert_eq!(projects.notes[1].title, "Zebra");
    Ok(())
}

// ---------------------------------------------------------------------
// most_recent_dated_note
// ---------------------------------------------------------------------

#[test]
fn most_recent_dated_note_picks_the_calendar_latest_path_not_the_caller_order() {
    let summaries = vec![
        summary("2026/01/2026-01-05-0800.md", "January"),
        summary("2026/09/2026-09-14-1858.md", "September"),
    ];
    let found = most_recent_dated_note(&summaries, &active_config());
    assert_eq!(found.map(|summary| summary.title.as_str()), Some("September"));
}

#[test]
fn most_recent_dated_note_ignores_a_regular_note_that_would_otherwise_sort_first() {
    // An operator-named folder sorts ahead of any digit-led date folder in
    // plain path order (`p` > `2` lexicographically has nothing to do with
    // it here - `list_notes`'s own frontmatter-based fallback is the one
    // this guards against), so this proves the date-hierarchy lookup never
    // lets an unrelated regular note stand in for "most recent".
    let summaries = vec![
        summary("zzz-unrelated/notes-about-anything.md", "Unrelated"),
        summary("2026/01/2026-01-05-0800.md", "January"),
    ];
    let found = most_recent_dated_note(&summaries, &active_config());
    assert_eq!(found.map(|summary| summary.title.as_str()), Some("January"));
}

#[test]
fn most_recent_dated_note_is_none_when_the_date_structure_is_inactive() {
    let summaries = vec![summary("2026/01/2026-01-05-0800.md", "January")];
    let config = NotesConfig {
        date_structure_active: false,
        ..NotesConfig::default()
    };
    assert!(most_recent_dated_note(&summaries, &config).is_none());
}

#[test]
fn most_recent_dated_note_is_none_when_no_note_actually_matches_the_configured_format() {
    let summaries = vec![summary("projects/kickoff.md", "Kickoff")];
    assert!(most_recent_dated_note(&summaries, &active_config()).is_none());
}
