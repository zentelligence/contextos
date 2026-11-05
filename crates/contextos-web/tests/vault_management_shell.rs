//! Vault settings: the "Vault management" category group, its sidebar, and how a category's presence follows what
//! the vault has actually provisioned. Real `contextos-mcp` sessions against real temporary vault fixtures.

mod support;
mod vault_settings_common;

use vault_settings_common::*;

fn nav_section(body: &str) -> Result<&str, BoxError> {
    let start = body.find("id=\"vault-settings-nav\"").ok_or("no settings nav")?;
    let rest = &body[start..];
    let end = rest.find("</nav>").ok_or("unterminated settings nav")?;
    Ok(&rest[..end])
}

fn mkdir(dir: &Path, relative: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join(relative))
}

#[tokio::test]
async fn the_sidebar_separates_configuration_from_vault_management() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let nav = nav_section(&body)?;
    let configuration = nav.find(">Configuration<").ok_or("no Configuration heading")?;
    let management = nav.find(">Vault management<").ok_or("no Vault management heading")?;
    assert!(configuration < management, "{nav}");
    assert!(nav[configuration..management].contains("data-pane=\"tasks\""), "{nav}");
    Ok(())
}

#[tokio::test]
async fn unprovisioned_standards_templates_registry_and_activity_are_hidden_entirely() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    let nav = nav_section(&body)?;
    for hidden in ["standards", "templates", "registry", "activity"] {
        assert!(
            !nav.contains(&format!("/settings/{hidden}/")),
            "{hidden} should be hidden: {nav}"
        );
    }
    for (category, status) in [
        ("standards", StatusCode::NOT_FOUND),
        ("templates", StatusCode::NOT_FOUND),
        ("registry", StatusCode::NOT_FOUND),
        ("activity", StatusCode::NOT_FOUND),
    ] {
        let (actual, _) = get(&router, &format!("/{VAULT_NAME}/settings/{category}/")).await?;
        assert_eq!(actual, status, "{category}");
    }
    Ok(())
}

#[tokio::test]
async fn the_decisions_register_is_always_listed_because_its_location_is_configurable() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    let nav = nav_section(&body)?;
    assert!(nav.contains(&format!("/{VAULT_NAME}/settings/decisions/")), "{nav}");
    assert!(
        !nav.contains("/settings/questions/"),
        "questions live in the decisions register: {nav}"
    );

    let (status, decisions) = get(&router, &format!("/{VAULT_NAME}/settings/decisions/")).await?;
    assert_eq!(status, StatusCode::OK, "{decisions}");
    assert!(decisions.contains("No decisions registered yet"), "{decisions}");
    let (status, _) = get(&router, &format!("/{VAULT_NAME}/settings/questions/")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn an_empty_but_provisioned_category_shows_an_empty_view() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    mkdir(vault_dir.path(), "standards")?;
    mkdir(vault_dir.path(), "templates")?;
    mkdir(vault_dir.path(), "memory/log")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    let nav = nav_section(&body)?;
    for shown in ["standards", "templates", "activity"] {
        assert!(
            nav.contains(&format!("/{VAULT_NAME}/settings/{shown}/")),
            "{shown}: {nav}"
        );
    }
    let (status, standards) = get(&router, &format!("/{VAULT_NAME}/settings/standards/")).await?;
    assert_eq!(status, StatusCode::OK, "{standards}");
    assert!(standards.contains("No standards yet"), "{standards}");
    let (status, templates) = get(&router, &format!("/{VAULT_NAME}/settings/templates/")).await?;
    assert_eq!(status, StatusCode::OK, "{templates}");
    assert!(templates.contains("No templates yet"), "{templates}");
    let (status, activity) = get(&router, &format!("/{VAULT_NAME}/settings/activity/")).await?;
    assert_eq!(status, StatusCode::OK, "{activity}");
    assert!(activity.contains("No operations logged yet"), "{activity}");
    Ok(())
}

#[tokio::test]
async fn the_sidebar_counts_each_categorys_items_but_never_its_index() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "standards/index.md", "# Standards\n")?;
    write(vault_dir.path(), "standards/rust.md", "# Rust\n")?;
    write(vault_dir.path(), "standards/ui/htmx.md", "# HTMX\n")?;
    write(vault_dir.path(), "registry/agents/alex.md", "# Alex\n")?;
    write(vault_dir.path(), "registry/hats/critic.md", "# Critic\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    let nav = nav_section(&body)?;
    let standards = nav
        .split("</a>")
        .find(|link| link.contains("/settings/standards/"))
        .ok_or("no standards link")?;
    assert!(
        standards.contains("<span class=\"settings-nav-count\">2</span>"),
        "{standards}"
    );
    let registry = nav
        .split("</a>")
        .find(|link| link.contains("/settings/registry/"))
        .ok_or("no registry link")?;
    assert!(
        registry.contains("<span class=\"settings-nav-count\">2</span>"),
        "{registry}"
    );
    Ok(())
}

#[tokio::test]
async fn a_management_page_marks_its_own_category_active_and_links_back_to_configuration() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "standards/rust.md", "# Rust\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/settings/standards/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let nav = nav_section(&body)?;
    let active = nav
        .split("</a>")
        .find(|link| link.contains("/settings/standards/"))
        .ok_or("no standards link")?;
    assert!(active.contains("active"), "{active}");
    assert!(active.contains("aria-current=\"page\""), "{active}");
    assert!(
        nav.contains(&format!("href=\"/{VAULT_NAME}/settings/#tasks\"")),
        "{nav}"
    );
    Ok(())
}

#[tokio::test]
async fn an_htmx_request_for_a_category_returns_only_the_settings_fragment() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let response = tower::ServiceExt::oneshot(
        router.clone(),
        Request::builder()
            .uri(format!("/{VAULT_NAME}/settings/decisions/"))
            .header("hx-request", "true")
            .body(Body::empty())?,
    )
    .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8_lossy(&to_bytes(response.into_body(), usize::MAX).await?).into_owned();
    assert!(!body.contains("<html"), "{body}");
    assert!(body.contains("class=\"vault-settings settings\""), "{body}");
    Ok(())
}

#[tokio::test]
async fn an_unknown_category_is_not_found() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = get(&router, &format!("/{VAULT_NAME}/settings/workflows/")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}
