//! The vault a browser was last in, kept in a cookie, through the real router and a real `contextos-mcp` session with two
//! vaults. A page with no vault in its address uses it for its sidebar, and says plainly that it is not that vault.

mod support;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use contextos_web::McpClientSet;
use contextos_web::config::WorkflowsConfig;
use contextos_web::package_store::AppDataRoots;
use support::BoxError;
use tower::ServiceExt as _;

struct Site {
    _directories: [tempfile::TempDir; 4],
    _clients: Arc<McpClientSet>,
    router: Router,
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

async fn site() -> Result<Site, BoxError> {
    let (alpha, beta) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let (config, data) = (tempfile::tempdir()?, tempfile::tempdir()?);
    std::fs::write(alpha.path().join("index.md"), "# Alpha\n")?;
    std::fs::write(beta.path().join("index.md"), "# Beta\n")?;
    let workflow = beta.path().join("registry/workflows/ops/sync");
    std::fs::create_dir_all(workflow.join("jobs"))?;
    std::fs::write(
        beta.path().join("registry/workflows/ops/workflows.toml"),
        "workflows = [\"sync\"]\n",
    )?;
    std::fs::write(
        workflow.join("workflow.toml"),
        "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
    )?;
    std::fs::write(workflow.join("sync.rhai"), "log_info(\"a\");")?;
    std::fs::write(
        workflow.join("jobs/nightly.toml"),
        "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    std::fs::create_dir_all(beta.path().join(".contextos"))?;
    std::fs::write(beta.path().join(".contextos/team.toml"), support::jobs::TEAM)?;
    // A failed latest run is what puts a job in the sidebar.
    let runs = beta.path().join("memory/jobs/ops/sync/nightly/2026/09");
    std::fs::create_dir_all(&runs)?;
    std::fs::write(
        runs.join("R-1.md"),
        "---\ntype: job-run\nworkflow: sync\njob: nightly\nstatus: failed\ntrigger: manual\n\
         started: 2026-09-01T00:00:00Z\n---\n",
    )?;
    let config_path =
        support::write_named_vaults_config(config.path(), &[("alpha", alpha.path()), ("beta", beta.path())])?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(std::slice::from_ref(&entry)).await?);
    let web_config = config.path().join("web.toml");
    std::fs::write(
        &web_config,
        WEB_TOML.replace("CONFIG", &config_path.to_string_lossy().replace('\\', "/")),
    )?;
    let router = contextos_web::build_router_in(
        Arc::clone(&clients),
        None,
        &web_config,
        &WorkflowsConfig::default(),
        Some(&AppDataRoots::from(data.path())),
    );
    Ok(Site {
        _directories: [alpha, beta, config, data],
        _clients: clients,
        router,
    })
}

const WEB_TOML: &str = "[[mcp_server]]\nname = \"contextos\"\ntransport = \"stdio\"\ncommand = \"contextos\"\n\
                        args = [\"--config\", \"CONFIG\", \"--stdio\"]\n";

impl Site {
    async fn get(&self, path: &str, cookie: Option<&str>) -> Result<Answer, BoxError> {
        let mut request = Request::builder().uri(path);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        let response = self.router.clone().oneshot(request.body(Body::empty())?).await?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(Answer {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }
}

impl Answer {
    fn active_vault_cookie(&self) -> Option<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with("cc_active_vault="))
            .map(|value| value.split(';').next().unwrap_or_default().to_owned())
    }
}

#[tokio::test]
async fn visiting_a_vault_makes_it_the_active_vault() -> Result<(), BoxError> {
    let site = site().await?;
    let page = site.get("/beta/workflows", None).await?;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.active_vault_cookie().as_deref(), Some("cc_active_vault=beta"));
    Ok(())
}

#[tokio::test]
async fn a_page_with_no_vault_does_not_change_the_active_vault() -> Result<(), BoxError> {
    let site = site().await?;
    for path in ["/shared/workflows", "/shared/packages", "/settings/"] {
        let page = site.get(path, Some("cc_active_vault=beta")).await?;
        assert_eq!(page.status, StatusCode::OK, "{path}");
        assert_eq!(page.active_vault_cookie(), None, "{path}");
    }
    Ok(())
}

#[tokio::test]
async fn a_missing_page_does_not_change_the_active_vault() -> Result<(), BoxError> {
    let site = site().await?;
    let page = site.get("/nowhere/workflows", Some("cc_active_vault=beta")).await?;
    assert_eq!(page.status, StatusCode::NOT_FOUND);
    assert_eq!(page.active_vault_cookie(), None);
    Ok(())
}

#[tokio::test]
async fn the_shared_and_settings_sidebars_follow_the_active_vault() -> Result<(), BoxError> {
    let site = site().await?;
    for path in ["/shared/workflows", "/shared/packages", "/settings/"] {
        let page = site.get(path, Some("cc_active_vault=beta")).await?;
        assert!(page.body.contains("href=\"/beta/tasks"), "{path}: {}", page.body);
        assert!(!page.body.contains("href=\"/alpha/tasks"), "{path}: {}", page.body);
    }
    Ok(())
}

// A job whose latest run failed appears in the sidebar, but only when its workflow's servers are known, so every page that
// shows a vault's sidebar must pass them.
#[tokio::test]
async fn the_settings_sidebar_is_the_same_as_the_shared_one() -> Result<(), BoxError> {
    let site = site().await?;
    let sidebar = |body: &str| {
        body.split("shell-sidebar")
            .nth(1)
            .and_then(|rest| rest.split("</aside>").next())
            .map(|sidebar| sidebar.contains("href=\"/beta/workflows/ops/sync/jobs/nightly"))
    };
    let shared = site.get("/shared/workflows", Some("cc_active_vault=beta")).await?;
    let settings = site.get("/settings/", Some("cc_active_vault=beta")).await?;
    assert_eq!(sidebar(&shared.body), Some(true), "{}", shared.body);
    assert_eq!(sidebar(&settings.body), Some(true), "{}", settings.body);
    Ok(())
}

#[tokio::test]
async fn with_no_active_vault_the_first_configured_vault_stands_in() -> Result<(), BoxError> {
    let site = site().await?;
    let page = site.get("/shared/workflows", None).await?;
    assert!(page.body.contains("href=\"/alpha/tasks"), "{}", page.body);
    Ok(())
}

// A cookie is the browser's to send, so a vault since removed, or a value never set here, is never believed.
#[tokio::test]
async fn an_active_vault_that_is_not_configured_is_ignored() -> Result<(), BoxError> {
    let site = site().await?;
    for cookie in ["cc_active_vault=gone", "cc_active_vault=%22%3E%3Cscript%3E"] {
        let page = site.get("/shared/workflows", Some(cookie)).await?;
        assert_eq!(page.status, StatusCode::OK, "{cookie}");
        assert!(page.body.contains("href=\"/alpha/tasks"), "{cookie}: {}", page.body);
        assert!(
            !page.body.contains("<script>\"") && !page.body.contains("gone"),
            "{cookie}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn the_sidebar_says_which_view_is_showing_and_which_vault_its_links_open() -> Result<(), BoxError> {
    let site = site().await?;
    let shared = site.get("/shared/workflows", Some("cc_active_vault=beta")).await?;
    assert!(shared.body.contains("class=\"sidebar-scope\""), "{}", shared.body);
    assert!(shared.body.contains(">Shared view<"), "{}", shared.body);
    assert!(shared.body.contains("Links open beta"), "{}", shared.body);

    let settings = site.get("/settings/", Some("cc_active_vault=beta")).await?;
    assert!(settings.body.contains("class=\"sidebar-scope\""), "{}", settings.body);
    assert!(settings.body.contains(">Server settings<"), "{}", settings.body);

    let vault = site.get("/beta/workflows", None).await?;
    assert!(!vault.body.contains("class=\"sidebar-scope\""), "{}", vault.body);
    Ok(())
}

#[tokio::test]
async fn the_shared_view_is_marked_in_the_sidebar_and_the_vaults_workflows_are_not() -> Result<(), BoxError> {
    let site = site().await?;
    let page = site.get("/shared/workflows", Some("cc_active_vault=beta")).await?;
    let sidebar = page.body.split("shell-sidebar").nth(1).unwrap_or_default();
    assert!(sidebar.contains("href=\"/shared/workflows\""), "{sidebar}");
    assert!(!sidebar.contains("nav-group-heading-link active"), "{sidebar}");
    Ok(())
}

// The browser selects the first option when none is marked, which would name a vault the page is not in.
#[tokio::test]
async fn the_vault_switcher_does_not_claim_a_vault_on_a_page_with_none() -> Result<(), BoxError> {
    let site = site().await?;
    for (path, label) in [("/shared/workflows", "Shared view"), ("/settings/", "Server settings")] {
        let page = site.get(path, Some("cc_active_vault=beta")).await?;
        let switcher = page.body.split("id=\"vault-select\"").nth(1).unwrap_or_default();
        let switcher = switcher.split("</select>").next().unwrap_or_default();
        assert!(
            switcher.contains(&format!("<option value=\"\" selected disabled>{label}</option>")),
            "{path}: {switcher}"
        );
    }
    let vault = site.get("/beta/workflows", None).await?;
    assert!(!vault.body.contains("selected disabled"), "{}", vault.body);
    Ok(())
}
