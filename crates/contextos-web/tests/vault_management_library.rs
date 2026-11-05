//! Vault settings: Standards, Templates, and the Registry, the management categories that are folders of source
//! files. Real `contextos-mcp` sessions against real temporary vault fixtures.

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

/// Decodes the HTML character references a browser would: named `&lt;`-style references and numeric `&#60;` or
/// `&#x3C;` ones.
fn unescape(html: &str) -> String {
    let mut decoded = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find('&') {
        decoded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let reference = after.find(';').map(|end| (&after[..end], &after[end + 1..]));
        let character = reference.and_then(|(name, _)| match name {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "amp" => Some('&'),
            _ => name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")).map_or_else(
                || {
                    name.strip_prefix('#')
                        .and_then(|digits| digits.parse().ok())
                        .and_then(char::from_u32)
                },
                |hex| u32::from_str_radix(hex, 16).ok().and_then(char::from_u32),
            ),
        });
        if let (Some(character), Some((_, remaining))) = (character, reference) {
            decoded.push(character);
            rest = remaining;
        } else {
            decoded.push('&');
            rest = after;
        }
    }
    decoded.push_str(rest);
    decoded
}

/// The editor's content exactly as a browser would submit it: the HTML parser drops the one newline that follows
/// `<textarea>`, and every entity is decoded.
fn textarea_value(body: &str) -> Result<String, BoxError> {
    let start = body.find("class=\"vm-source-editor\"").ok_or("no editor")?;
    let open_end = start + body[start..].find('>').ok_or("unterminated textarea")? + 1;
    let close = open_end + body[open_end..].find("</textarea>").ok_or("no closing textarea")?;
    let raw = &body[open_end..close];
    Ok(unescape(raw.strip_prefix('\n').unwrap_or(raw)))
}

fn hidden_value(body: &str, name: &str) -> Result<String, BoxError> {
    let marker = format!("name=\"{name}\" value=\"");
    let start = body.find(&marker).ok_or_else(|| format!("no {name}"))? + marker.len();
    let end = start + body[start..].find('"').ok_or("unterminated value")?;
    Ok(unescape(&body[start..end]))
}

fn save_body(content: &str, expected_hash: &str) -> Result<String, BoxError> {
    Ok(serde_json::to_string(&serde_json::json!({
        "content": content, "expected_hash": expected_hash
    }))?)
}

#[tokio::test]
async fn a_standard_reads_rendered_with_its_properties_and_its_nested_folders_kept() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "standards/console-ui/07 HTMx Uplifts.md",
        "---\nstatus: draft\ntags:\n  - htmx\n  - ui\n---\n# HTMx uplifts\n\nUse **fragments**.\n",
    )?;
    write(vault_dir.path(), "standards/rust.md", "# Rust\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, root) = get(&router, &format!("/{VAULT_NAME}/settings/standards/")).await?;
    assert_eq!(status, StatusCode::OK, "{root}");
    assert!(
        root.contains("console-ui"),
        "the nested folder is shown as a folder: {root}"
    );
    assert!(
        root.contains("/settings/standards/console-ui/07%20HTMx%20Uplifts.md"),
        "{root}"
    );

    let (status, page) = get(
        &router,
        &format!("/{VAULT_NAME}/settings/standards/console-ui/07%20HTMx%20Uplifts.md"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("<strong>fragments</strong>"), "{page}");
    assert!(page.contains("<dt>status</dt>"), "{page}");
    assert!(page.contains(">htmx<"), "{page}");
    assert!(
        page.contains("/settings/standards/console-ui/07%20HTMx%20Uplifts.md/edit"),
        "{page}"
    );
    Ok(())
}

#[tokio::test]
async fn saving_a_standard_replaces_the_whole_file_including_its_frontmatter() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "standards/rust.md",
        "---\nstatus: draft\n---\n# Rust\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, form) = get(&router, &format!("/{VAULT_NAME}/settings/standards/rust.md/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{form}");
    assert_eq!(textarea_value(&form)?, "---\nstatus: draft\n---\n# Rust\n");
    let hash = hidden_value(&form, "expected_hash")?;

    let updated = "---\nstatus: adopted\n---\n# Rust\n\nNo unwrap.\n";
    let (status, page) = send(
        &router,
        "PUT",
        &format!("/{VAULT_NAME}/settings/standards/rust.md"),
        &save_body(updated, &hash)?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("standards/rust.md"))?,
        updated
    );
    assert!(page.contains("No unwrap."), "the saved file is shown read: {page}");
    Ok(())
}

#[tokio::test]
async fn a_save_against_a_stale_version_is_refused_and_writes_nothing() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "standards/rust.md", "# Rust\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, form) = get(&router, &format!("/{VAULT_NAME}/settings/standards/rust.md/edit")).await?;
    let hash = hidden_value(&form, "expected_hash")?;
    write(vault_dir.path(), "standards/rust.md", "# Rust, changed elsewhere\n")?;

    let (status, page) = send(
        &router,
        "PUT",
        &format!("/{VAULT_NAME}/settings/standards/rust.md"),
        &save_body("# Mine\n", &hash)?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("changed after you opened it"), "{page}");
    assert!(
        page.contains("# Mine"),
        "the submitted text is kept in the editor: {page}"
    );
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("standards/rust.md"))?,
        "# Rust, changed elsewhere\n"
    );
    Ok(())
}

#[tokio::test]
async fn a_template_round_trips_byte_for_byte_through_the_edit_form() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let template = "---\nbase: \"[[path_to_base|{{base_name}}]]\"\ndate: <% tp.date.now(\"YYYY-MM-DD\") %>\n---\n\n# {{title}} & <notes>\n\n<%* tR += \"'quoted'\" %>\n";
    write(vault_dir.path(), "templates/decision.md", template)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, form) = get(&router, &format!("/{VAULT_NAME}/settings/templates/decision.md/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{form}");
    let shown = textarea_value(&form)?;
    assert_eq!(shown, template, "the editor shows the file exactly");
    assert!(
        !form.contains("line endings"),
        "a file already stored as the vault stores text needs no notice"
    );
    let (status, page) = send(
        &router,
        "PUT",
        &format!("/{VAULT_NAME}/settings/templates/decision.md"),
        &save_body(&shown, &hidden_value(&form, "expected_hash")?)?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        std::fs::read(vault_dir.path().join("templates/decision.md"))?,
        template.as_bytes()
    );
    Ok(())
}

#[tokio::test]
async fn a_windows_line_ending_file_is_saved_with_unix_line_endings_and_says_so_first() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "templates/windows.md", "# {{title}}\r\n\r\nBody.\r\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, form) = get(&router, &format!("/{VAULT_NAME}/settings/templates/windows.md/edit")).await?;
    assert!(form.contains("Saving writes it with \\n line endings"), "{form}");
    assert!(form.contains("hx-put"), "the file can be saved: {form}");
    // A browser submits a textarea's lines with `\n`.
    let submitted = textarea_value(&form)?
        .replace("\r\n", "\n")
        .replace("Body.", "Changed.");
    let (status, page) = send(
        &router,
        "PUT",
        &format!("/{VAULT_NAME}/settings/templates/windows.md"),
        &save_body(&submitted, &hidden_value(&form, "expected_hash")?)?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("templates/windows.md"))?,
        "# {{title}}\n\nChanged.\n"
    );
    Ok(())
}

#[tokio::test]
async fn a_template_reads_with_its_placeholders_marked_rather_than_interpreted() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "templates/note.md",
        "# {{title}}\n\nCreated <% tp.date.now(\"YYYY-MM-DD\") %>.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, page) = get(&router, &format!("/{VAULT_NAME}/settings/templates/note.md")).await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(
        page.contains("<code class=\"template-token\">{{title}}</code>"),
        "{page}"
    );
    assert!(
        // Quotes in element text need no escaping; only `<`, `>`, and `&` do.
        page.contains("<code class=\"template-token\">&lt;% tp.date.now(\"YYYY-MM-DD\") %&gt;</code>"),
        "{page}"
    );
    Ok(())
}

#[tokio::test]
async fn a_new_standard_is_created_and_never_overwrites_an_existing_one() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "standards/rust.md", "# Rust\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, page) = send(
        &router,
        "POST",
        &format!("/{VAULT_NAME}/settings/standards/"),
        r##"{"name": "python", "content": "# Python\n"}"##,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("standards/python.md"))?,
        "# Python\n"
    );

    let (status, page) = send(
        &router,
        "POST",
        &format!("/{VAULT_NAME}/settings/standards/"),
        r##"{"name": "rust", "content": "# Replaced\n"}"##,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("already exists"), "{page}");
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("standards/rust.md"))?,
        "# Rust\n"
    );

    let (_, page) = send(
        &router,
        "POST",
        &format!("/{VAULT_NAME}/settings/standards/"),
        r#"{"name": "../escape", "content": "x"}"#,
    )
    .await?;
    assert!(page.contains("single plain name"), "{page}");
    assert!(!vault_dir.path().join("escape.md").exists());
    Ok(())
}

#[tokio::test]
async fn a_path_outside_the_listed_folder_is_never_read() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "standards/rust.md", "# Rust\n")?;
    write(vault_dir.path(), "secret.md", "# Secret\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    for path in [
        "..%2Fsecret.md",
        "%2E%2E/secret.md",
        "missing.md",
        "rust.md/../../secret.md",
    ] {
        let (status, body) = get(&router, &format!("/{VAULT_NAME}/settings/standards/{path}")).await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
        assert!(!body.contains("Secret"), "{path}");
    }
    Ok(())
}

#[tokio::test]
async fn the_registry_shows_one_tab_per_type_with_empty_and_unprovisioned_types_saying_so() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "registry/agents/alex.md",
        "---\nname: alex\ndescription: Reviews drafts.\n---\n# Agent: Alex\n",
    )?;
    std::fs::create_dir_all(vault_dir.path().join("registry/hats"))?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, agents) = get(&router, &format!("/{VAULT_NAME}/settings/registry/")).await?;
    assert_eq!(status, StatusCode::OK, "{agents}");
    for label in ["Agents", "Hats", "Prompts", "Roles", "Scripts", "Skills"] {
        assert!(
            agents.contains(&format!(">{label} <span class=\"vm-tab-count\">")),
            "{label}: {agents}"
        );
    }
    assert!(
        agents.contains("Reviews drafts."),
        "a definition's description is listed: {agents}"
    );

    let (_, hats) = get(&router, &format!("/{VAULT_NAME}/settings/registry/hats")).await?;
    assert!(hats.contains("Nothing defined yet"), "{hats}");
    let (status, roles) = get(&router, &format!("/{VAULT_NAME}/settings/registry/roles")).await?;
    assert_eq!(status, StatusCode::OK, "{roles}");
    assert!(roles.contains("Nothing defined yet"), "{roles}");
    assert!(roles.contains("does not exist in the vault yet"), "{roles}");
    Ok(())
}

#[tokio::test]
async fn a_script_reads_as_highlighted_source() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "registry/scripts/tidy.py",
        "def tidy():\n    return 1  # done\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, page) = get(&router, &format!("/{VAULT_NAME}/settings/registry/scripts/tidy.py")).await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(page.contains("<span class=\"tok-keyword\">def</span>"), "{page}");
    assert!(page.contains("<span class=\"tok-comment\"># done</span>"), "{page}");
    assert!(page.contains("Python"), "{page}");
    Ok(())
}

#[tokio::test]
async fn a_new_definition_starts_from_its_types_template() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    std::fs::create_dir_all(vault_dir.path().join("registry/agents"))?;
    write(
        vault_dir.path(),
        "templates/registry-agent.md",
        "---\nname: <slug>\n---\n# Agent: <Title>\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, form) = get(&router, &format!("/{VAULT_NAME}/settings/registry/agents/new")).await?;
    assert_eq!(status, StatusCode::OK, "{form}");
    assert_eq!(textarea_value(&form)?, "---\nname: <slug>\n---\n# Agent: <Title>\n");
    Ok(())
}

#[tokio::test]
async fn a_new_skill_is_a_folder_with_its_entry_file() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    std::fs::create_dir_all(vault_dir.path().join("registry/skills"))?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, page) = send(
        &router,
        "POST",
        &format!("/{VAULT_NAME}/settings/registry/skills"),
        r##"{"name": "plan-week", "content": "# Plan week\n"}"##,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("registry/skills/plan-week/SKILL.md"))?,
        "# Plan week\n"
    );
    Ok(())
}

#[tokio::test]
async fn a_registry_types_folder_and_template_are_configurable_and_validated() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "library/prompts/brief.md", "# Brief\n")?;
    std::fs::create_dir_all(vault_dir.path().join("registry/agents"))?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, form) = get(&router, &format!("/{VAULT_NAME}/settings/registry/prompts/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{form}");
    assert!(form.contains("id=\"vm-paths-dir\""), "{form}");
    assert!(form.contains(">Update<"), "{form}");

    let (_, refused) = send(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/settings/registry/prompts"),
        r#"{"dir": "library/nowhere", "template": ""}"#,
    )
    .await?;
    assert!(refused.contains("does not exist in this vault"), "{refused}");
    assert!(!vault_dir.path().join(".contextos/vault-management.toml").exists());

    let (status, page) = send(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/settings/registry/prompts"),
        r#"{"dir": "library/prompts", "template": ""}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/vault-management.toml"))?;
    assert!(saved.contains("[registry.prompts]"), "{saved}");
    assert!(saved.contains("dir = \"library/prompts\""), "{saved}");
    assert!(
        page.contains("brief"),
        "the type now lists the configured folder: {page}"
    );
    Ok(())
}

#[tokio::test]
async fn a_file_with_classic_mac_line_endings_is_editable_and_saved_with_unix_line_endings() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "standards/classic.md",
        "# Classic\rline one\rline two\r",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, form) = get(&router, &format!("/{VAULT_NAME}/settings/standards/classic.md/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{form}");
    assert!(form.contains("hx-put") && !form.contains("readonly"), "{form}");
    assert!(form.contains("Saving writes it with \\n line endings"), "{form}");
    let (status, page) = send(
        &router,
        "PUT",
        &format!("/{VAULT_NAME}/settings/standards/classic.md"),
        &save_body(
            "# Classic\nline one\nline two, edited\n",
            &hidden_value(&form, "expected_hash")?,
        )?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("standards/classic.md"))?,
        "# Classic\nline one\nline two, edited\n"
    );
    Ok(())
}

#[tokio::test]
async fn a_file_mixing_line_endings_is_saved_with_unix_line_endings() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "standards/mixed.md",
        "# Mixed\r\nline one\nline two\r\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, form) = get(&router, &format!("/{VAULT_NAME}/settings/standards/mixed.md/edit")).await?;
    assert!(form.contains("hx-put"), "{form}");
    let (status, page) = send(
        &router,
        "PUT",
        &format!("/{VAULT_NAME}/settings/standards/mixed.md"),
        &save_body("# Mixed\nline one\nline two\n", &hidden_value(&form, "expected_hash")?)?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join("standards/mixed.md"))?,
        "# Mixed\nline one\nline two\n"
    );
    Ok(())
}

#[tokio::test]
async fn a_registry_type_cannot_be_pointed_at_the_append_only_or_hidden_folders() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    std::fs::create_dir_all(vault_dir.path().join("registry/agents"))?;
    std::fs::create_dir_all(vault_dir.path().join("memory/log/2026"))?;
    std::fs::create_dir_all(vault_dir.path().join(".obsidian"))?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    for (dir, expected) in [
        ("memory/log", "append-only"),
        ("memory/log/2026", "append-only"),
        (".obsidian", "hidden"),
    ] {
        let (_, refused) = send(
            &router,
            "PATCH",
            &format!("/{VAULT_NAME}/settings/registry/agents"),
            &format!(r#"{{"dir": "{dir}", "template": ""}}"#),
        )
        .await?;
        assert!(refused.contains(expected), "{dir}: {refused}");
    }
    assert!(!vault_dir.path().join(".contextos/vault-management.toml").exists());
    Ok(())
}

#[tokio::test]
async fn a_registry_type_save_from_an_older_version_of_the_settings_is_refused() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    std::fs::create_dir_all(vault_dir.path().join("registry/agents"))?;
    std::fs::create_dir_all(vault_dir.path().join("library/agents"))?;
    write(vault_dir.path(), ".contextos/vault-management.toml", "# Hand edited.\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, form) = get(&router, &format!("/{VAULT_NAME}/settings/registry/agents/edit")).await?;
    let hash = hidden_value(&form, "expected_hash")?;
    write(
        vault_dir.path(),
        ".contextos/vault-management.toml",
        "# Changed in another tab.\n",
    )?;
    let (_, refused) = send(
        &router,
        "PATCH",
        &format!("/{VAULT_NAME}/settings/registry/agents"),
        &format!(r#"{{"dir": "library/agents", "template": "", "expected_hash": "{hash}"}}"#),
    )
    .await?;
    assert!(refused.contains("changed after you opened it"), "{refused}");
    assert_eq!(
        std::fs::read_to_string(vault_dir.path().join(".contextos/vault-management.toml"))?,
        "# Changed in another tab.\n"
    );
    Ok(())
}
