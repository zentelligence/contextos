//! Vault settings: the Decisions register, decisions and questions in one `memory/decisions` tree, told apart by each
//! record's `type` or, without one, its id prefix. Real `contextos-mcp` sessions against real temporary vault
//! fixtures.

mod support;
mod vault_settings_common;

use vault_settings_common::*;

async fn send(router: &Router, method: &str, path: &str, json_body: &str) -> Result<(StatusCode, String), BoxError> {
    let response = tower::ServiceExt::oneshot(
        router.clone(),
        Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("hx-request", "true")
            .body(Body::from(json_body.to_owned()))?,
    )
    .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

fn hidden_value(body: &str, name: &str) -> Result<String, BoxError> {
    let marker = format!("name=\"{name}\" value=\"");
    let start = body.find(&marker).ok_or_else(|| format!("no {name}"))? + marker.len();
    let end = start + body[start..].find('"').ok_or("unterminated value")?;
    Ok(body[start..end].to_owned())
}

const DECISIONS_BASE: &str = "filters: file.folder == \"memory/decisions\"\nviews:\n  - type: table\n    name: All\n";

fn sidecar(prefixes: &str) -> String {
    format!(
        "---\nbase: '[[memory/decisions.base]]'\nkey_prefix: VD\nkey_format: '{{key_prefix}}-{{key_id}}'\n\
         key_prefixes:\n{prefixes}last_create: 2026-09-13 05:06:05 +00:00:00\nlast_update: 2026-09-13 05:06:05 +00:00:00\n---\n"
    )
}

fn register_fixture(vault: &Path) -> std::io::Result<()> {
    write(vault, "index.md", "# Root\n")?;
    write(vault, "memory/decisions.base", DECISIONS_BASE)?;
    write(vault, "memory/decisions.md", &sidecar("  VD: 2\n  VQ: 1\n"))?;
    write(vault, "memory/decisions/index.md", "# Decisions\n")?;
    write(
        vault,
        "memory/decisions/vd-001.md",
        "---\nid: VD-001\ndate: 2026-08-07\nstatus: superseded\napplies_to:\n- memory/decisions/\nsuperseded_by: VD-002\nupdated: 2026-09-29 20:48:31 +00:00:00\n---\n\n# VD-001: One file per record\n\n## Decision\n\nKeep **one file**.\n",
    )?;
    write(
        vault,
        "memory/decisions/vd-002.md",
        "---\nid: VD-002\ntype: decision\ntitle: Records carry their own title\ndate: 2026-09-01\nstatus: adopted\napplies_to: []\nsuperseded_by:\n---\n\n# Records carry their own title\n\nBody.\n",
    )?;
    write(
        vault,
        "memory/decisions/vq-001.md",
        "---\nid: VQ-001\ntitle: Prioritisation methodology\nstatus: open\ncreated: 2026-10-01T03:12:00Z\nresolved: 2026-10-02T01:00:00Z\nblocks: nothing\n---\n\n# Prioritisation methodology\n\n## Question\n\nWhere did it go?\n",
    )?;
    write(
        vault,
        "templates/registry-vault-decision.md",
        "---\nid: VD-{{NNN}}\n---\n\n# VD-{{NNN}}: {{Decision title}}\n\n## Context\n\n[Why.]\n\n<!--\nstatus must be adopted or superseded.\n-->\n",
    )?;
    write(
        vault,
        "templates/registry-vault-question.md",
        "---\nid: VQ-{{NNN}}\n---\n\n# {{Short question title}}\n\n## Question\n\n[Ask it.]\n",
    )
}

#[tokio::test]
async fn one_list_holds_both_types_and_filters_by_type_and_status() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    register_fixture(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, page) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/")).await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    for id in [">VD-002<", ">VD-001<", ">VQ-001<"] {
        assert!(page.contains(id), "{id}: {page}");
    }
    assert!(
        page.contains("<td>Question</td>"),
        "a question without `type` is told by its prefix: {page}"
    );
    assert!(
        page.contains("One file per record"),
        "a record with no title takes its heading: {page}"
    );
    assert!(
        page.contains("/settings/decisions/vd-002\""),
        "the successor links to its record: {page}"
    );
    assert!(
        page.contains(">Decisions <span class=\"vm-tab-count\">2</span>"),
        "{page}"
    );
    assert!(
        page.contains(">Questions <span class=\"vm-tab-count\">1</span>"),
        "{page}"
    );
    assert!(!page.contains(">index<"), "the folder index is not a record");

    let (_, questions) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/?type=question")).await?;
    assert!(
        questions.contains(">VQ-001<") && !questions.contains(">VD-001<"),
        "{questions}"
    );
    assert!(
        questions.contains(">Open <span"),
        "the status filter offers the type's own statuses: {questions}"
    );
    assert!(!questions.contains(">Adopted <span"), "{questions}");
    let (_, adopted) = get(
        &router,
        &format!("/{VAULT_NAME}/settings/decisions/?type=decision&status=adopted"),
    )
    .await?;
    assert!(
        adopted.contains(">VD-002<") && !adopted.contains(">VD-001<"),
        "{adopted}"
    );
    Ok(())
}

#[tokio::test]
async fn a_record_reads_its_type_specific_fields_and_how_its_type_was_told() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    register_fixture(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, decision) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/vd-001")).await?;
    assert_eq!(status, StatusCode::OK, "{decision}");
    assert!(decision.contains("<strong>one file</strong>"), "{decision}");
    assert!(decision.contains("<dt>Superseded by</dt>"), "{decision}");
    assert!(decision.contains("told by its id prefix VD"), "{decision}");

    let (_, question) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/vq-001")).await?;
    assert!(question.contains("<dt>Resolution path</dt>"), "{question}");
    assert!(!question.contains("<dt>Superseded by</dt>"), "{question}");
    assert!(question.contains("<dt>Created</dt><dd>2026-10-01</dd>"), "{question}");

    let (status, _) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/vd-404")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn one_save_writes_fields_and_body_and_never_touches_the_id_or_type() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    register_fixture(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, form) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/vd-002/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{form}");
    let body = serde_json::json!({
        "expected_hash": hidden_value(&form, "expected_hash")?, "title": "Records carry their own title",
        "status": "superseded", "date": "2026-09-01", "superseded_by": "VD-001",
        "applies_to": "memory/decisions/", "description": "",
        "body": "# Records carry their own title\n\nRevised body.\n"
    });
    let (status, page) = send(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/settings/decisions/vd-002"),
        &body.to_string(),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(!page.contains("role=\"alert\""), "{page}");
    let saved = std::fs::read_to_string(vault_dir.path().join("memory/decisions/vd-002.md"))?;
    assert!(saved.contains("status: superseded"), "{saved}");
    assert!(saved.contains("superseded_by: VD-001"), "{saved}");
    assert!(saved.contains("Revised body."), "{saved}");
    assert!(
        saved.contains("id: VD-002") && saved.contains("type: decision"),
        "{saved}"
    );
    Ok(())
}

#[tokio::test]
async fn an_unchanged_save_writes_nothing() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    register_fixture(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;
    let before = std::fs::read(vault_dir.path().join("memory/decisions/vq-001.md"))?;

    let (_, form) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/vq-001/edit")).await?;
    let body = serde_json::json!({
        "expected_hash": hidden_value(&form, "expected_hash")?, "title": "Prioritisation methodology",
        "status": "open", "entity": "", "resolved": "2026-10-02T01:00:00Z", "decision": "", "blocks": "nothing",
        "resolution_path": "", "description": "",
        "body": "# Prioritisation methodology\n\n## Question\n\nWhere did it go?\n"
    });
    let (status, page) = send(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/settings/decisions/vq-001"),
        &body.to_string(),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        std::fs::read(vault_dir.path().join("memory/decisions/vq-001.md"))?,
        before
    );
    Ok(())
}

#[tokio::test]
async fn editing_a_question_keeps_its_stamped_timestamps() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    register_fixture(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, form) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/vq-001/edit")).await?;
    assert!(
        !form.contains("name=\"created\""),
        "created is never offered for editing: {form}"
    );
    assert!(
        form.contains("type=\"text\" name=\"resolved\" value=\"2026-10-02T01:00:00Z\""),
        "a stored timestamp is offered as text, not blanked by a date input: {form}"
    );
    let body = serde_json::json!({
        "expected_hash": hidden_value(&form, "expected_hash")?, "title": "Prioritisation methodology",
        "status": "resolved", "entity": "", "resolved": "2026-10-02T01:00:00Z", "decision": "", "blocks": "nothing",
        "resolution_path": "", "description": "",
        "body": "# Prioritisation methodology\n\n## Question\n\nWhere did it go?\n"
    });
    let (_, page) = send(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/settings/decisions/vq-001"),
        &body.to_string(),
    )
    .await?;
    assert!(!page.contains("role=\"alert\""), "{page}");
    let saved = std::fs::read_to_string(vault_dir.path().join("memory/decisions/vq-001.md"))?;
    assert!(saved.contains("status: resolved"), "{saved}");
    assert!(saved.contains("created: 2026-10-01T03:12:00Z"), "{saved}");
    assert!(saved.contains("resolved: 2026-10-02T01:00:00Z"), "{saved}");
    Ok(())
}

#[tokio::test]
async fn a_windows_line_ending_record_is_saved_with_unix_line_endings() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    register_fixture(vault_dir.path())?;
    write(
        vault_dir.path(),
        "memory/decisions/vd-003.md",
        "---\r\nid: VD-003\r\ntitle: Windows note\r\nstatus: adopted\r\n---\r\n\r\n# Windows note\r\n\r\nOld.\r\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, form) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/vd-003/edit")).await?;
    assert!(
        !form.contains("id: VD-003"),
        "the frontmatter is not part of the body editor: {form}"
    );
    let body = serde_json::json!({
        "expected_hash": hidden_value(&form, "expected_hash")?, "title": "Windows note", "status": "adopted",
        "date": "", "superseded_by": "", "applies_to": "", "description": "",
        "body": "# Windows note\n\nNew.\n"
    });
    let (_, page) = send(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/settings/decisions/vd-003"),
        &body.to_string(),
    )
    .await?;
    assert!(!page.contains("role=\"alert\""), "{page}");
    let saved = std::fs::read_to_string(vault_dir.path().join("memory/decisions/vd-003.md"))?;
    assert!(!saved.contains('\r'), "{saved:?}");
    assert!(
        saved.starts_with("---\nid: VD-003\ntitle: Windows note\nstatus: adopted\nupdated: "),
        "{saved:?}"
    );
    assert!(saved.ends_with("\n---\n\n# Windows note\n\nNew.\n"), "{saved:?}");
    Ok(())
}

#[tokio::test]
async fn an_invalid_or_stale_save_writes_nothing_and_keeps_what_was_typed() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    register_fixture(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;
    let before = std::fs::read_to_string(vault_dir.path().join("memory/decisions/vd-002.md"))?;

    let (_, form) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/vd-002/edit")).await?;
    let invalid = serde_json::json!({
        "expected_hash": hidden_value(&form, "expected_hash")?, "title": "Kept title", "status": "pending",
        "body": "Typed body."
    });
    let (_, page) = send(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/settings/decisions/vd-002"),
        &invalid.to_string(),
    )
    .await?;
    assert!(page.contains("Status must be one of: adopted, superseded."), "{page}");
    assert!(page.contains("Kept title") && page.contains("Typed body."), "{page}");

    let stale = serde_json::json!({
        "expected_hash": "0".repeat(64), "title": "Kept title", "status": "adopted", "body": "Typed body."
    });
    let (_, page) = send(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/settings/decisions/vd-002"),
        &stale.to_string(),
    )
    .await?;
    assert!(page.contains("changed after you opened it"), "{page}");
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("memory/decisions/vd-002.md"))?,
        before
    );
    Ok(())
}

#[tokio::test]
async fn each_new_record_takes_its_types_id_series_type_and_template() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    register_fixture(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, form) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/new?type=decision")).await?;
    assert_eq!(status, StatusCode::OK, "{form}");
    assert!(form.contains("## Context") && !form.contains("{{NNN}}"), "{form}");
    assert!(
        form.contains("status must be adopted or superseded."),
        "authoring notes are shown: {form}"
    );
    let decision = serde_json::json!({
        "type": "decision", "title": "Registers live in Vault Settings", "status": "adopted", "date": "2026-10-08",
        "superseded_by": "", "applies_to": "", "description": "", "body": "## Context\n\nOne place.\n"
    });
    let (status, page) = send(
        &router,
        "POST",
        &format!("/{VAULT_NAME}/settings/decisions/"),
        &decision.to_string(),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    let created = std::fs::read_to_string(vault_dir.path().join("memory/decisions/vd-003.md"))?;
    assert!(
        created.contains("id: VD-003") && created.contains("type: decision"),
        "{created}"
    );
    assert!(
        created.contains("# Registers live in Vault Settings") && created.contains("One place."),
        "{created}"
    );
    assert!(
        !created.contains("superseded_by"),
        "an unset field is simply absent: {created}"
    );

    let (_, form) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/new?type=question")).await?;
    assert!(
        form.contains("## Question") && form.contains("name=\"resolution_path\""),
        "{form}"
    );
    let question = serde_json::json!({
        "type": "question", "title": "Who owns templates?", "status": "open", "entity": "", "resolved": "",
        "decision": "VD-003", "blocks": "nothing", "resolution_path": "", "description": "",
        "body": "## Question\n\nWho?\n"
    });
    let (status, page) = send(
        &router,
        "POST",
        &format!("/{VAULT_NAME}/settings/decisions/"),
        &question.to_string(),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    let created = std::fs::read_to_string(vault_dir.path().join("memory/decisions/vq-002.md"))?;
    assert!(
        created.contains("id: VQ-002") && created.contains("type: question"),
        "{created}"
    );
    assert!(created.contains("decision: VD-003"), "{created}");
    Ok(())
}

#[tokio::test]
async fn a_type_whose_prefix_the_sidecar_does_not_declare_cannot_be_created() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    register_fixture(vault_dir.path())?;
    write(vault_dir.path(), "memory/decisions.md", &sidecar("  VD: 2\n"))?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let question = serde_json::json!({"type": "question", "title": "Q", "status": "open", "body": ""});
    let (_, page) = send(
        &router,
        "POST",
        &format!("/{VAULT_NAME}/settings/decisions/"),
        &question.to_string(),
    )
    .await?;
    assert!(page.contains("does not declare the id prefix VQ"), "{page}");
    assert!(!vault_dir.path().join("memory/decisions/vq-002.md").exists());
    Ok(())
}

fn settings_body(dir: &str, base: &str, hash: &str) -> String {
    serde_json::json!({
        "base": base, "dir": dir, "decision_prefix": "VD", "decision_template": "",
        "question_prefix": "VQ", "question_template": "", "expected_hash": hash
    })
    .to_string()
}

#[tokio::test]
async fn the_registers_settings_are_validated_and_conflict_checked() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    register_fixture(vault_dir.path())?;
    write(vault_dir.path(), "records/decisions.base", DECISIONS_BASE)?;
    write(
        vault_dir.path(),
        "records/decisions/rd-1.md",
        "---\nid: RD-1\ntype: decision\ntitle: Moved\nstatus: adopted\n---\n",
    )?;
    std::fs::create_dir_all(vault_dir.path().join("memory/log"))?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;
    let settings_url = format!("/{VAULT_NAME}/settings/decisions/");

    let (status, form) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{form}");
    assert!(form.contains("id=\"vm-paths-question_prefix\""), "{form}");
    for (dir, base, expected) in [
        (
            "records/decisions",
            "records/missing.base",
            "does not exist in this vault",
        ),
        ("records/decisions", "records/decisions.md", "must name a `.base` file"),
        ("memory/log", "records/decisions.base", "append-only"),
        (".contextos", "records/decisions.base", "hidden"),
    ] {
        let (_, refused) = send(&router, "PATCH", &settings_url, &settings_body(dir, base, "")).await?;
        assert!(refused.contains(expected), "{dir} {base}: {refused}");
    }
    assert!(!vault_dir.path().join(".contextos/vault-management.toml").exists());

    let (status, page) = send(
        &router,
        "PATCH",
        &settings_url,
        &settings_body("records/decisions", "records/decisions.base", ""),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(
        page.contains(">RD-1<"),
        "the register now lists the configured folder: {page}"
    );

    let (_, stale) = send(
        &router,
        "PATCH",
        &settings_url,
        &settings_body("memory/decisions", "memory/decisions.base", ""),
    )
    .await?;
    assert!(
        stale.contains("already exists"),
        "a save made from an older version is refused: {stale}"
    );
    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/vault-management.toml"))?;
    assert!(saved.contains("dir = \"records/decisions\""), "{saved}");
    Ok(())
}

#[tokio::test]
async fn an_unprovisioned_register_offers_no_new_record() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, page) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/")).await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("No decisions registered yet"), "{page}");
    assert!(
        !page.contains("New question") && !page.contains("New decision"),
        "{page}"
    );
    let (status, _) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/new?type=question")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &router,
        "POST",
        &format!("/{VAULT_NAME}/settings/decisions/"),
        r#"{"type": "question", "title": "T", "status": "open", "body": ""}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::CONFLICT);
    Ok(())
}

#[tokio::test]
async fn a_save_is_held_to_the_note_rules_and_refused_whole_when_it_breaks_one() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    register_fixture(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;
    let before = std::fs::read_to_string(vault_dir.path().join("memory/decisions/vd-002.md"))?;

    let (_, form) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/vd-002/edit")).await?;
    let hash = hidden_value(&form, "expected_hash")?;
    for (description, body, expected) in [
        (
            "Records carry their own title",
            "# Records carry their own title\n\nRevised body.\n",
            "description cannot repeat the title",
        ),
        ("", "See [[unclosed.\n", "malformed Obsidian link or callout"),
    ] {
        let save = serde_json::json!({
            "expected_hash": hash, "title": "Records carry their own title", "status": "superseded",
            "date": "2026-09-01", "superseded_by": "VD-001", "applies_to": "", "description": description,
            "body": body
        });
        let (_, page) = send(
            &router,
            "PATCH",
            &format!("/{VAULT_NAME}/settings/decisions/vd-002"),
            &save.to_string(),
        )
        .await?;
        assert!(page.contains(expected), "{expected}: {page}");
        assert!(!page.contains("fields were saved"), "nothing is half saved: {page}");
        assert_eq!(
            std::fs::read_to_string(vault_dir.path().join("memory/decisions/vd-002.md"))?,
            before
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_record_starting_with_a_byte_order_mark_keeps_its_fields_and_mark_when_saved() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    register_fixture(vault_dir.path())?;
    write(
        vault_dir.path(),
        "memory/decisions/vd-003.md",
        "\u{feff}---\nid: VD-003\ntype: decision\ntitle: Marked\nstatus: adopted\ncreated: 2026-08-07T09:00+10:00\n---\n\n# Marked\n\nOld.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, form) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/vd-003/edit")).await?;
    assert!(
        form.contains("value=\"Marked\""),
        "the fields after the mark are read: {form}"
    );
    let body = serde_json::json!({
        "expected_hash": hidden_value(&form, "expected_hash")?, "title": "Marked", "status": "superseded",
        "date": "", "superseded_by": "", "applies_to": "", "description": "", "body": "# Marked\n\nNew.\n"
    });
    let (_, page) = send(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/settings/decisions/vd-003"),
        &body.to_string(),
    )
    .await?;
    assert!(!page.contains("role=\"alert\""), "{page}");
    let saved = std::fs::read_to_string(vault_dir.path().join("memory/decisions/vd-003.md"))?;
    assert!(
        saved.starts_with("\u{feff}---\nid: VD-003\ntype: decision\ntitle: Marked\nstatus: superseded\ncreated: 2026-08-07T09:00+10:00\n"),
        "{saved:?}"
    );
    assert!(
        saved.ends_with("---\n\n# Marked\n\nNew.\n") && saved.matches("---\n").count() == 2,
        "{saved:?}"
    );
    Ok(())
}
