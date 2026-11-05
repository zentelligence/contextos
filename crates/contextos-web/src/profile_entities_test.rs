use super::*;

fn file(relative_path: &str) -> ProfileFileLink {
    ProfileFileLink {
        label: relative_path.strip_suffix(".md").unwrap_or(relative_path).to_owned(),
        relative_path: relative_path.to_owned(),
    }
}

#[test]
fn orphans_excludes_files_matching_a_configured_slug() {
    let files = vec![file("personal.md"), file("zenith.md"), file("stray-note.md")];
    let orphans = partition_orphans(&["personal".to_owned(), "zenith".to_owned()], files);
    let labels: Vec<&str> = orphans.iter().map(|f| f.label.as_str()).collect();
    assert_eq!(labels, vec!["stray-note"]);
}

#[test]
fn orphans_is_empty_when_every_file_matches_a_configured_slug() {
    let files = vec![file("personal.md")];
    let orphans = partition_orphans(&["personal".to_owned()], files);
    assert!(orphans.is_empty(), "`orphans` should be empty, got {orphans:?}");
}
