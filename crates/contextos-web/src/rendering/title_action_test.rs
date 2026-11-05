use super::*;

#[test]
fn a_new_task_action_links_to_the_vaults_new_task_form() {
    let html = render(&TitleAction::NewTask("vault"));
    assert!(html.contains("href=\"/vault/tasks/new\""));
    assert!(html.contains("New task"));
}

#[test]
fn a_new_note_action_loads_the_form_into_the_notes_screen_by_selector() {
    let html = render(&TitleAction::NewNote("vault"));
    assert!(html.contains("hx-get=\"/vault/notes/new\""));
    assert!(html.contains("hx-target=\".notes-screen\""));
}

#[test]
fn a_rescan_action_posts_to_the_vaults_apps_rescan_route() {
    let html = render(&TitleAction::RescanApps("vault"));
    assert!(html.contains("method=\"post\" action=\"/vault/apps/rescan\""));
}

#[test]
fn an_upload_action_posts_to_the_given_target_and_swaps_the_inbox_screen_by_selector() {
    let html = render(&TitleAction::UploadToInbox("/vault/inbox/execute/upload"));
    assert!(html.contains("hx-post=\"/vault/inbox/execute/upload\""));
    assert!(html.contains("hx-target=\".inbox-screen\""));
    assert!(html.contains("hx-encoding=\"multipart/form-data\""));
}

#[test]
fn an_action_target_is_html_escaped() {
    let html = render(&TitleAction::UploadToInbox("/v/\"><script>"));
    assert!(!html.contains("\"><script>"));
}

#[test]
fn the_upload_form_resets_after_each_request_so_the_same_file_can_be_chosen_again() {
    let html = render(&TitleAction::UploadToInbox("/vault/inbox/execute/upload"));
    assert!(html.contains("hx-on::after-request=\"this.reset()\""));
}

#[test]
fn a_download_action_links_to_the_file_as_stored_with_an_escaped_name() {
    let html = render(&TitleAction::Download {
        href: "/vault/wiki/a%20b.md?raw=1&x=\"y\"",
        name: "<b>a b</b>.md",
    });
    assert!(html.contains("class=\"icon-button title-download\""), "{html}");
    assert!(
        html.contains(" download=\"&#60;b&#62;a b&#60;/b&#62;.md\"")
            || html.contains(" download=\"&lt;b&gt;a b&lt;/b&gt;.md\""),
        "{html}"
    );
    assert!(
        html.contains("aria-label=\"Download &#60;b&#62;a b&#60;/b&#62;.md\"")
            || html.contains("aria-label=\"Download &lt;b&gt;a b&lt;/b&gt;.md\""),
        "{html}"
    );
    assert!(!html.contains("<b>"), "{html}");
    assert!(!html.contains("x=\"y\""), "{html}");
}
