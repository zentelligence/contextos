use super::*;

fn urls() -> VaultUrls {
    VaultUrls::from("harbour")
}

#[test]
fn a_path_is_encoded_once_with_its_separators_kept() {
    assert_eq!(
        urls().file("misc/My Plan #2?.png"),
        "/harbour/misc/My%20Plan%20%232%3F.png"
    );
    assert_eq!(urls().wiki("a page/it's.md"), "/harbour/wiki/a%20page/it%27s.md");
    assert_eq!(urls().notes("/2026/one.md/"), "/harbour/notes/2026/one.md");
}

#[test]
fn an_empty_path_is_the_screen_itself_with_its_trailing_slash() {
    assert_eq!(urls().root(), "/harbour/");
    assert_eq!(urls().wiki(""), "/harbour/wiki/");
    assert_eq!(urls().notes(""), "/harbour/notes/");
    assert_eq!(urls().folder(""), "/harbour/");
    assert_eq!(urls().folder("a b"), "/harbour/a%20b/");
}

#[test]
fn an_action_follows_the_path_or_stands_alone_at_the_screen_root() {
    assert_eq!(urls().wiki_action("topic.md", "edit"), "/harbour/wiki/topic.md/edit");
    assert_eq!(urls().wiki_action("", "new"), "/harbour/wiki/new");
    assert_eq!(
        urls().entity_action("zenith", "projects/a b.md", "confirm-delete"),
        "/harbour/entity/zenith/projects/a%20b.md/confirm-delete"
    );
}

#[test]
fn a_task_is_addressed_by_its_lower_cased_id() {
    assert_eq!(urls().task("A-029"), "/harbour/tasks/a-029");
    assert_eq!(urls().task_edit("A-029"), "/harbour/tasks/a-029/edit");
    assert_eq!(
        urls().task_link_search("A-029", "depends_on"),
        "/harbour/tasks/a-029/links/search?field=depends_on"
    );
}

#[test]
fn query_values_and_views_are_encoded() {
    assert_eq!(
        urls().base_view("memory/tasks.base", "Open now"),
        "/harbour/memory/tasks.base?view=Open%20now"
    );
    assert_eq!(
        urls().agents_in_state("awaiting-input"),
        "/harbour/agents/?state=awaiting-input"
    );
    assert_eq!(
        VaultUrls::download(&urls().wiki("a b.md")),
        "/harbour/wiki/a%20b.md?raw=1"
    );
}

#[test]
fn a_tray_command_addresses_the_folder_or_the_tray_root() {
    assert_eq!(
        urls().tray_command("inbox", Some("drafts/x"), "/execute/ingest", "execute/ingest"),
        "/harbour/inbox/drafts/x/execute/ingest"
    );
    assert_eq!(
        urls().tray_command("inbox", Some(""), "/execute/ingest", "execute/ingest"),
        "/harbour/inbox/execute/ingest"
    );
}

#[test]
fn an_entity_folders_own_url_has_no_trailing_slash() {
    assert_eq!(urls().entity_file("zenith", ""), "/harbour/entity/zenith");
    assert_eq!(
        urls().entity_file("zenith", "projects/x.md"),
        "/harbour/entity/zenith/projects/x.md"
    );
}

#[test]
fn a_files_favourite_state_is_addressed_by_its_vault_path() {
    assert_eq!(
        urls().favourite("memory/wiki/a b.md"),
        "/harbour/favourites/memory/wiki/a%20b.md"
    );
}

#[test]
fn a_workflow_and_a_job_are_addressed_by_package_workflow_and_job() {
    assert_eq!(urls().workflow("ops", "sync"), "/harbour/workflows/ops/sync");
    assert_eq!(
        urls().job("ops", "sync", "nightly run"),
        "/harbour/workflows/ops/sync/jobs/nightly%20run"
    );
    assert_ne!(
        urls().job("ops", "sync", "nightly"),
        urls().job("crm", "sync", "nightly")
    );
}
