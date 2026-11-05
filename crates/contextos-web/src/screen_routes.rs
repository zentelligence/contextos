//! Where each vault note opens: the route table every screen already configures, the tasks base's ids for task notes,
//! and the router the renderer writes every vault URL through. Which file a link names is the server's answer
//! (`expand_links`); this module only maps a vault path to the screen that shows it.

use std::collections::HashMap;

use contextos_markdown::LinkRouter;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::entities::load_entities;
use crate::inbox_outbox::encode_query_value;
use crate::insights_config::load_insights_config;
use crate::journal_config::load_journal_config;
use crate::mcp_client::McpClient;
use crate::notes_config::load_notes_config;
use crate::vault_links::{LinkRoutes, RouteConfig, SettingsRoutes, TASKS_DIR, is_task_note_path};
use crate::wiki_config::load_wiki_config;

#[derive(Debug, Deserialize)]
struct FrontmatterResult {
    frontmatter: Map<String, Value>,
}

/// The route table for `vault_name`, from the configuration each screen reads: six reads, made together.
pub async fn load_link_routes(client: &McpClient, vault_name: &str) -> LinkRoutes {
    // Boxed, so the joined state lives on the heap rather than inflating every caller's own future.
    let (notes, wiki, insights, journal, entities, management) = Box::pin(async {
        tokio::join!(
            load_notes_config(client, vault_name),
            load_wiki_config(client, vault_name),
            load_insights_config(client, vault_name),
            load_journal_config(client, vault_name),
            load_entities(client, vault_name),
            crate::vault_management_config::load_vault_management_config(client, vault_name),
        )
    })
    .await;
    let management = &management.config;
    LinkRoutes::new(
        vault_name,
        RouteConfig {
            notes_dir: notes.notes_dir,
            wiki_dir: wiki.wiki_dir,
            insights_dir: insights.insights_dir,
            journal_dir: journal.journal_dir,
            journal_date_format: journal.date_format,
            entity_slugs: entities.into_iter().map(|entity| entity.slug).collect(),
            settings: SettingsRoutes {
                register_dir: management.register().paths.dir.as_str().to_owned(),
                register_base: management.register().paths.base.as_str().to_owned(),
                registry: crate::vault_management_config::RegistryKind::ALL
                    .into_iter()
                    .map(|kind| {
                        (
                            kind.slug().to_owned(),
                            management.registry(kind).dir.as_str().to_owned(),
                        )
                    })
                    .collect(),
            },
        },
    )
}

/// The web route for `vault_path`, a note known to exist, or `None` when no screen shows it: the route table, and
/// for a task note its id, read for that one note.
pub async fn route_for_note(client: &McpClient, vault_name: &str, vault_path: &str) -> Option<String> {
    let (routes, task_id) = tokio::join!(
        load_link_routes(client, vault_name),
        task_id_for_note_path(client, vault_name, vault_path)
    );
    routes.route(vault_path, task_id.as_deref())
}

/// `note`'s frontmatter with every link value already resolved by the server to a vault-rooted path, or to a marker
/// for a link naming no file or several ([`crate::vault_links::classify_expanded_target`]), through one
/// `frontmatter_read` call with `expand_links`. `None` when the read is refused. The values are a reading of the
/// note, never its content: nothing read here may be written back.
pub(crate) async fn expanded_frontmatter(
    client: &McpClient,
    vault_name: &str,
    note: &str,
) -> Option<Map<String, Value>> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{note}")));
    args.insert("expand_links".to_owned(), Value::Bool(true));
    client
        .call_tool("frontmatter_read".to_owned(), args)
        .await
        .ok()
        .filter(|result| result.is_error != Some(true))?
        .into_typed::<FrontmatterResult>()
        .ok()
        .map(|read| read.frontmatter)
}

/// The web routes of a list of vault paths a screen renders (a base view's rows, search results, a link panel), so
/// every list sends each note to the screen that owns it by the one route table ([`LinkRoutes`]). The route table is
/// loaded once, and the tasks base's ids are read in one query only when a listed path is a task note.
pub struct ScreenRoutes {
    vault_name: String,
    routes: LinkRoutes,
    task_ids: HashMap<String, String>,
}

impl ScreenRoutes {
    /// The routes for `paths` (vault-relative) in `vault_name`.
    pub async fn load<'p>(client: &McpClient, vault_name: &str, paths: impl IntoIterator<Item = &'p str>) -> Self {
        let routes = load_link_routes(client, vault_name).await;
        Self::with_routes(client, vault_name, routes, paths).await
    }

    /// [`Self::load`] with a route table already loaded, reading the tasks base's ids only when a path is a task note.
    pub async fn with_routes<'p>(
        client: &McpClient,
        vault_name: &str,
        routes: LinkRoutes,
        paths: impl IntoIterator<Item = &'p str>,
    ) -> Self {
        let task_ids = if paths.into_iter().any(is_task_note_path) {
            task_ids_by_path(client, vault_name).await
        } else {
            HashMap::new()
        };
        Self {
            vault_name: vault_name.to_owned(),
            routes,
            task_ids,
        }
    }

    /// Routes for a page that links to nothing in the vault, made without reading the configuration the route table
    /// comes from: a page with no vault link never asks them for a route.
    #[must_use]
    pub fn unloaded(vault_name: &str) -> Self {
        Self {
            vault_name: vault_name.to_owned(),
            routes: LinkRoutes::new(vault_name, RouteConfig::default()),
            task_ids: HashMap::new(),
        }
    }

    /// The route table these routes come from.
    #[must_use]
    pub fn table(&self) -> &LinkRoutes {
        &self.routes
    }

    /// The web route that shows `vault_path`, or `None` when no screen does.
    #[must_use]
    pub fn route(&self, vault_path: &str) -> Option<String> {
        self.routes
            .route(vault_path, self.task_ids.get(vault_path).map(String::as_str))
    }
}

/// The renderer asks these routes for every URL it writes, so a rendered note, the lists beside it, and the metadata
/// panel agree on where each note opens.
impl LinkRouter for ScreenRoutes {
    fn page(&self, path: &str, anchor: Option<&str>) -> Option<String> {
        let route = self.route(path)?;
        Some(match anchor {
            Some(anchor) if !anchor.is_empty() => format!("{route}#{}", encode_query_value(anchor)),
            _ => route,
        })
    }

    fn file(&self, path: &str) -> String {
        crate::urls::VaultUrls::from(self.vault_name.as_str()).file(path)
    }
}

#[derive(Debug, Deserialize)]
struct TaskIdRow {
    #[serde(rename = "file.path")]
    path: String,
    /// `Option`, not `String`: `base_query` reports a task note with no `id` frontmatter of its own as an explicit
    /// JSON `null`, which `#[serde(default)]` alone does not cover (that only fills in a key missing outright, and
    /// a `String` field rejects `null` the same way it rejects any other wrong-shaped value) - a single such row
    /// would otherwise fail the whole batch's deserialisation, not just that row.
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskIdQueryResult {
    content: String,
}

/// Every task note's lower-cased `id`, keyed by its vault path, in one `base_query` round trip: the batch
/// counterpart to [`task_id_for_note_path`]'s own single-note lookup, for a render pass that needs several task
/// ids at once (one per distinct `memory/tasks/**` link a rendered note carries) rather than one `frontmatter_read`
/// per link. Only the two columns a route needs are requested, lighter than `routes::tasks::TaskRow`'s full row
/// shape. An empty map on any failure: a caller that cannot look up a task's id falls back to leaving that link
/// unrouted, never to failing the whole render.
pub async fn task_ids_by_path(client: &McpClient, vault_name: &str) -> HashMap<String, String> {
    let mut args = Map::new();
    args.insert("vault".to_owned(), Value::String(format!("{vault_name}://.")));
    args.insert("format".to_owned(), Value::String("json".to_owned()));
    args.insert(
        "definition".to_owned(),
        serde_json::json!({
            "filters": format!("file.inFolder(\"{TASKS_DIR}\")"),
            "properties": { "file.path": {}, "id": {} },
            "views": [{ "type": "table", "name": "rows", "order": ["file.path", "id"] }],
        }),
    );
    let Ok(result) = client.call_tool("base_query".to_owned(), args).await else {
        return HashMap::new();
    };
    if result.is_error == Some(true) {
        return HashMap::new();
    }
    let Ok(parsed) = result.into_typed::<TaskIdQueryResult>() else {
        return HashMap::new();
    };
    let rows: Vec<TaskIdRow> = serde_json::from_str(&parsed.content).unwrap_or_default();
    rows.into_iter()
        .filter_map(|row| {
            row.id
                .filter(|id| !id.is_empty())
                .map(|id| (row.path, id.to_ascii_lowercase()))
        })
        .collect()
}

/// The lower-cased `id` frontmatter field of `vault_path` when it is a task note ([`is_task_note_path`]), via one
/// `frontmatter_read` round trip rather than a whole-vault fetch. `None` for any note outside the tasks base, or a
/// task note with no `id` of its own.
pub async fn task_id_for_note_path(client: &McpClient, vault_name: &str, vault_path: &str) -> Option<String> {
    if !is_task_note_path(vault_path) {
        return None;
    }
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{vault_path}")));
    let result = client.call_tool("frontmatter_read".to_owned(), args).await.ok()?;
    if result.is_error == Some(true) {
        return None;
    }
    let read = result.into_typed::<FrontmatterResult>().ok()?;
    let id = read.frontmatter.get("id")?.as_str()?;
    (!id.is_empty()).then(|| id.to_ascii_lowercase())
}
