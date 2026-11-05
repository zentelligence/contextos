//! Notes screen: first-note creation, default and explicit selection, and the year/month sidebar grouping.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod notes_common;
mod support;

use notes_common::*;

// ---------------------------------------------------------------------
// GET /{{vault_name}}/notes/
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_vault_with_no_notes_auto_creates_and_selects_a_first_one_through_note_create() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_template(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The mock's own empty-state prompt is never shown: a first note was
    // created and selected instead.
    assert!(!body.contains("No unique notes yet"), "{body}");
    assert!(body.contains("notes-body-textarea"), "{body}");

    // `[vault] managed = true`'s own per-directory `index.md` stub
    // (`notes/index.md`, `notes/2026/index.md`, `notes/2026/09/index.md`)
    // is not the note this assertion is about.
    let created: Vec<String> = files_under(&vault_dir.path().join("notes"))?
        .into_iter()
        .filter(|path| !path.ends_with("index.md"))
        .collect();
    assert_eq!(created.len(), 1, "{created:?}");
    let content = std::fs::read_to_string(vault_dir.path().join("notes").join(&created[0]))?;
    // `note_create`'s own guaranteed default frontmatter schema, proving
    // this went through `note_create` and not a direct `fs_write_file`.
    assert!(content.contains("type: note"), "{content}");
    assert!(content.contains("status: draft"), "{content}");
    assert!(content.contains("- note"), "{content}");
    assert!(content.contains("created:"), "{content}");
    Ok(())
}

#[tokio::test]
async fn get_selects_the_most_recently_dated_note_by_default() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/older.md",
        "---\ntitle: Older note\ndate: 2026-09-10\ntime: 07:00\nstatus: draft\ntags: []\n---\n\nOlder body.\n",
    )?;
    write(
        vault_dir.path(),
        "notes/2026/09/newer.md",
        "---\ntitle: Newer note\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nNewer body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Newer note"), "{body}");
    assert!(body.contains("Newer body."), "{body}");
    assert!(!body.contains("Older body."), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_prefers_the_most_recent_dated_note_over_a_regular_note_lacking_frontmatter_dates() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/newer.md",
        "---\ntitle: Newer note\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nNewer body.\n",
    )?;
    // A regular, operator-named note with no `date`/`time` frontmatter at
    // all: its own sort key (`notes::sort_key`) then falls back to an empty
    // string for both fields, which - compared as plain text - sorts ahead
    // of any real date string, so a naive "first of the frontmatter-sorted
    // list" fallback would wrongly open this note instead of the actual
    // most recent one in the date hierarchy.
    write(
        vault_dir.path(),
        "notes/zzz-project/notes.md",
        "---\ntitle: Undated note\nstatus: draft\ntags: []\n---\n\nUndated body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Newer body."), "{body}");
    assert!(!body.contains("Undated body."), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_with_a_note_path_selects_that_specific_note() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/older.md",
        "---\ntitle: Older note\ndate: 2026-09-10\ntime: 07:00\nstatus: draft\ntags: []\n---\n\nOlder body.\n",
    )?;
    write(
        vault_dir.path(),
        "notes/2026/09/newer.md",
        "---\ntitle: Newer note\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nNewer body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/2026/09/older.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Older body."), "{body}");
    assert!(!body.contains("Newer body."), "{body}");
    // The header row carries only Delete: Save lives exclusively in the
    // right-aligned `.save-bar` pair framing the body textarea itself (top
    // and bottom), each a small, non-full-width button rather than a
    // `<textarea>`'s flex-column parent stretching a bare button to fill
    // the panel - and never a second, redundant Save control stacked
    // above it in the header, which a first pass at this left in place.
    assert!(!body.contains("form=\"notes-body-form\""), "{body}");
    assert!(body.contains("id=\"notes-body-form\""), "{body}");
    assert_eq!(body.matches("class=\"save-bar save-bar-top\"").count(), 1, "{body}");
    assert_eq!(body.matches("class=\"save-bar\"").count(), 1, "{body}");
    Ok(())
}

#[tokio::test]
async fn get_with_an_unknown_note_path_falls_back_to_the_most_recent() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/newer.md",
        "---\ntitle: Newer note\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nNewer body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/2026/09/missing.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Newer body."), "{body}");
    Ok(())
}

// ---------------------------------------------------------------------
// The sidebar's own collapsible year/month tree
// (`routes::notes_view::build_tree`), replacing what used to be one flat,
// ever-growing list regardless of how many years of notes existed: a flat
// list is fine for a handful of notes but does not scale, and
// `routes::journal`'s own sidebar already established the "collapsible
// year/month tree" pattern for an identical fixed date-partition scheme.
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_sidebar_groups_notes_by_year_and_month_instead_of_one_flat_list() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/07/july-note.md",
        "---\ntitle: July note\ndate: 2026-07-02\ntime: 07:00\nstatus: draft\ntags: []\n---\n\nJuly body.\n",
    )?;
    write(
        vault_dir.path(),
        "notes/2026/09/september-note.md",
        "---\ntitle: September note\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nSeptember body.\n",
    )?;
    write(
        vault_dir.path(),
        "notes/2025/12/december-note.md",
        "---\ntitle: December note\ndate: 2025-12-31\ntime: 08:00\nstatus: draft\ntags: []\n---\n\nDecember body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Both years, and every month within them, are present as their own
    // real folder-name group - not silently collapsed into one list, and
    // never reinterpreted (a `"09"` folder is shown as itself, not
    // translated to "September" - see `notes_view`'s own module doc on why
    // this tree never assumes a date-partition shape).
    assert!(body.contains(">2026<"), "{body}");
    assert!(body.contains(">2025<"), "{body}");
    assert!(body.contains(">07<"), "{body}");
    assert!(body.contains(">09<"), "{body}");
    assert!(body.contains(">12<"), "{body}");
    // Every note title still appears somewhere in the tree - grouping never
    // drops a note, it only nests it.
    assert!(body.contains("July note"), "{body}");
    assert!(body.contains("September note"), "{body}");
    assert!(body.contains("December note"), "{body}");
    Ok(())
}
