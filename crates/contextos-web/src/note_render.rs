//! Showing a vault note: the one place a note is read for display, what it needs is fetched, and it is rendered.
//!
//! The note is read once with every link already expanded by the server (`fs_attach_file` with `expand_links`), so
//! rendering asks the vault nothing per link. Rendering itself is `contextos-markdown`'s pure pipeline; this module
//! supplies what that pipeline cannot do without I/O: the vault's fence vocabulary, the embedded notes, diagrams, and
//! base views, and the routes every vault URL is made from.

use std::collections::BTreeSet;

use contextos_markdown::{
    BaseViewKey, Body, BodyMetadata, Diagram, DiagramProblem, EmbedInput, EmbedKey, FenceVocabulary, Note,
    PrepareInput, PreparedNote, RenderInput, RenderedNote, Resources,
};
use futures_util::future::join_all;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClient};
use crate::rendering::escape_html;
use crate::screen_routes::{ScreenRoutes, load_link_routes};
use crate::vault_content::{AttachedRead, read_attached, read_expanded_text};

/// Where the vault's fence vocabulary lives (`.contextos/fences.toml`, which the vault scaffold ships).
pub const FENCES_PATH: &str = ".contextos/fences.toml";

/// The fence vocabulary a vault declares, and why the built-in one is used instead, when it is.
pub struct LoadedVocabulary {
    pub vocabulary: FenceVocabulary,
    /// Set when the vault's manifest exists but cannot be used: it is malformed, or newer than this build reads.
    pub problem: Option<String>,
}

/// Reads `.contextos/fences.toml`. A vault without one uses the vocabulary this build carries, which is the same file
/// the scaffold ships; a manifest that cannot be used also falls back to it, is logged, and is reported by Vault
/// health.
pub async fn load_fence_vocabulary(client: &McpClient, vault_name: &str) -> LoadedVocabulary {
    let read = read_attached(client, &format!("{vault_name}://{FENCES_PATH}")).await;
    let problem = match read {
        Ok(AttachedRead::Text(text)) => match FenceVocabulary::try_from(text.text.as_str()) {
            Ok(vocabulary) => {
                return LoadedVocabulary {
                    vocabulary,
                    problem: None,
                };
            }
            Err(error) => Some(error.to_string()),
        },
        Ok(AttachedRead::NotText) => Some("the fence manifest is not a text file".to_owned()),
        Ok(AttachedRead::Refused { code }) if code.as_deref() == Some("path/not-found") => None,
        Ok(AttachedRead::Refused { code }) => Some(format!(
            "the fence manifest could not be read ({})",
            code.as_deref().unwrap_or("no code")
        )),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => None,
    };
    if let Some(problem) = &problem {
        tracing::warn!(vault = vault_name, problem = %problem, "the vault's fence vocabulary cannot be used; using the built-in one");
    }
    LoadedVocabulary {
        vocabulary: FenceVocabulary::default(),
        problem,
    }
}

/// A rendered note and what was read with it.
pub struct RenderedPage {
    pub html: String,
    /// The note's frontmatter as the server expanded it: every link value a vault-rooted path or a marker. For
    /// reading only, never for writing back.
    pub frontmatter: Map<String, Value>,
    pub metadata: BodyMetadata,
    /// The routes the note's links were made with, for the panels beside it.
    pub routes: ScreenRoutes,
}

fn flag(text: &str) -> String {
    format!("<div class=\"render-flag\" role=\"note\">{}</div>\n", escape_html(text))
}

/// Which note to show, and how.
pub struct NoteView<'a> {
    /// The note's vault-relative path.
    pub path: &'a str,
    /// The note as the caller already read it, shown if it cannot be read with its links expanded.
    pub raw: Option<&'a str>,
    /// The note is a template: its Templater tokens are shown as written.
    pub template: bool,
}

/// Renders a note. When it cannot be read with its links expanded, the caller's `raw` copy is shown with a flag saying
/// its links could not be resolved, or, with no `raw`, a flag saying it could not be read; the page never silently
/// shows less than the note.
pub async fn render_note(client: &McpClient, vault_name: &str, view: NoteView<'_>) -> RenderedPage {
    let NoteView {
        path: note_path,
        raw,
        template,
    } = view;
    // A note whose text, as the caller read it, refers to nothing in the vault renders the same expanded or not, so it
    // is not read again.
    let refers = raw.is_none_or(|raw| template || refers_to_vault(raw));
    let expanded = if refers {
        read_expanded_text(client, &format!("{vault_name}://{note_path}"))
            .await
            .ok()
            .flatten()
    } else {
        raw.map(|raw| crate::vault_content::ExpandedText { text: raw.to_owned() })
    };
    let (text, problem) = match (&expanded, raw) {
        (Some(expanded), _) => (expanded.text.as_str(), None),
        (None, Some(raw)) => (
            raw,
            Some("This note's links could not be resolved, so they are shown as unavailable."),
        ),
        (None, None) => {
            return RenderedPage {
                html: flag("This note could not be read."),
                frontmatter: Map::new(),
                metadata: BodyMetadata::default(),
                routes: ScreenRoutes::unloaded(vault_name),
            };
        }
    };
    if template {
        // A template's properties are not data until Templater fills them in, and tokens such as `{{date}}` are not
        // valid YAML, so a template's body is what follows its frontmatter delimiters, read without parsing them.
        let body = Body::from(crate::vault_content::body_of(text));
        let mut rendered = render_bodies(client, vault_name, std::slice::from_ref(&body), true, &[]).await;
        let routes = rendered.routes;
        let (html, metadata) = rendered
            .notes
            .pop()
            .map_or_else(Default::default, |note| (note.html, note.metadata));
        return RenderedPage {
            html: problem.map(flag).unwrap_or_default() + html.as_str(),
            frontmatter: Map::new(),
            metadata,
            routes,
        };
    }
    let note = match Note::try_from(text) {
        Ok(note) => note,
        Err(error) => {
            let routes = ScreenRoutes::unloaded(vault_name);
            return RenderedPage {
                html: format!(
                    "{}<pre class=\"note-source\">{}</pre>\n",
                    flag(&format!(
                        "This note's properties could not be read ({error}), so its text is shown as written."
                    )),
                    escape_html(text)
                ),
                frontmatter: Map::new(),
                metadata: BodyMetadata::default(),
                routes,
            };
        }
    };
    // The frontmatter's links are routed with the body's, so the metadata panel beside the note can use these routes.
    let frontmatter_paths = crate::metadata_panel::linked_paths(note.frontmatter().properties());
    let mut rendered = render_bodies(
        client,
        vault_name,
        std::slice::from_ref(note.body()),
        false,
        &frontmatter_paths,
    )
    .await;
    let routes = rendered.routes;
    let (html, metadata) = rendered
        .notes
        .pop()
        .map_or_else(Default::default, |note| (note.html, note.metadata));
    RenderedPage {
        html: problem.map(flag).unwrap_or_default() + html.as_str(),
        frontmatter: note.frontmatter().properties().clone(),
        metadata,
        routes,
    }
}

/// Whether `text`, a note as written, refers to anything in the vault: a link or embed in its body that is not a web
/// address or a place on the page, or a wikilink in its frontmatter. One that does not renders the same without the
/// server's expansion.
fn refers_to_vault(text: &str) -> bool {
    let Ok(note) = Note::try_from(text) else {
        return true;
    };
    let frontmatter_links = note
        .frontmatter()
        .properties()
        .values()
        .any(|value| value.to_string().contains("[["));
    frontmatter_links
        || PreparedNote::from(PrepareInput {
            body: note.body(),
            vocabulary: &FenceVocabulary::default(),
            template: false,
        })
        .needs()
        .refers_to_vault
}

/// Several notes (or pieces of one, such as a journal's sections or a canvas's text nodes) rendered together: what
/// any of them needs is fetched once for all of them, and they share one set of routes.
pub struct RenderedNotes {
    pub notes: Vec<RenderedNote>,
    pub routes: ScreenRoutes,
}

/// Renders `texts`, each a piece of a note already expanded by the server, sharing what they need. A piece is all
/// body: it is never read for frontmatter.
/// `also_linked` names further vault paths the caller will route with the same routes.
pub async fn render_texts(
    client: &McpClient,
    vault_name: &str,
    texts: &[&str],
    also_linked: &[String],
) -> RenderedNotes {
    let bodies: Vec<Body> = texts.iter().map(|text| Body::from(*text)).collect();
    render_bodies(client, vault_name, &bodies, false, also_linked).await
}

async fn render_bodies(
    client: &McpClient,
    vault_name: &str,
    bodies: &[Body],
    template: bool,
    also_linked: &[String],
) -> RenderedNotes {
    // Only a body with a triple-colon line can hold a fence, so only then is the vault's vocabulary read.
    let vocabulary = if bodies.iter().any(|body| body.text().contains(":::")) {
        load_fence_vocabulary(client, vault_name).await.vocabulary
    } else {
        FenceVocabulary::default()
    };
    let prepared: Vec<PreparedNote> = bodies
        .iter()
        .map(|body| {
            PreparedNote::from(PrepareInput {
                body,
                vocabulary: &vocabulary,
                template,
            })
        })
        .collect();

    let mut resources = Resources::default();
    let embed_keys: BTreeSet<EmbedKey> = prepared
        .iter()
        .flat_map(|note| note.needs().embeds.iter().cloned())
        .collect();
    // An embed is itself a vault link, so whether any route is needed is known from the pages' own links; the route
    // table is read while the embedded notes are, and not at all for a page that links to nothing in the vault.
    let needs_routes = !also_linked.is_empty() || prepared.iter().any(|note| !note.needs().linked.is_empty());
    let (embedded, table) = tokio::join!(
        join_all(
            embed_keys
                .iter()
                .map(|key| embedded_note(client, vault_name, key, &vocabulary))
        ),
        async {
            if needs_routes {
                Some(load_link_routes(client, vault_name).await)
            } else {
                None
            }
        }
    );
    for (key, note) in embed_keys.into_iter().zip(embedded) {
        resources.embeds.insert(key, note);
    }

    // Diagrams, base views, and linked paths come from the notes and from what they embed, since an embedded note is
    // rendered on this page too.
    let all: Vec<&PreparedNote> = prepared.iter().chain(resources.embeds.values().flatten()).collect();
    let diagrams: BTreeSet<String> = all
        .iter()
        .flat_map(|note| note.needs().diagrams.iter().cloned())
        .collect();
    let bases: BTreeSet<BaseViewKey> = all.iter().flat_map(|note| note.needs().bases.iter().cloned()).collect();
    let linked: BTreeSet<String> = all
        .iter()
        .flat_map(|note| note.needs().linked.iter().cloned())
        .chain(also_linked.iter().cloned())
        .collect();

    let (diagram_results, base_views, routes) = tokio::join!(
        join_all(diagrams.iter().map(|source| diagram(client, source))),
        join_all(bases.iter().map(|key| base_view(client, vault_name, key))),
        async {
            match table {
                Some(table) => {
                    ScreenRoutes::with_routes(client, vault_name, table, linked.iter().map(String::as_str)).await
                }
                None => ScreenRoutes::unloaded(vault_name),
            }
        },
    );
    resources.diagrams.extend(diagrams.into_iter().zip(diagram_results));
    resources.bases.extend(
        bases
            .into_iter()
            .zip(base_views)
            .filter_map(|(key, html)| html.map(|html| (key, html))),
    );

    let notes = prepared
        .iter()
        .map(|prepared| {
            RenderedNote::from(RenderInput {
                prepared,
                resources: &resources,
                router: &routes,
            })
        })
        .collect();
    RenderedNotes { notes, routes }
}

/// An embedded note, read with its links expanded and narrowed to the place the embed names; `None` when it cannot be
/// read or parsed, which the page shows as an unavailable embed.
async fn embedded_note(
    client: &McpClient,
    vault_name: &str,
    key: &EmbedKey,
    vocabulary: &FenceVocabulary,
) -> Option<PreparedNote> {
    let expanded = read_expanded_text(client, &format!("{vault_name}://{}", key.path))
        .await
        .ok()
        .flatten()?;
    let note = Note::try_from(expanded.text.as_str()).ok()?;
    Some(PreparedNote::from(EmbedInput {
        note: &note,
        fragment: key.fragment.as_ref(),
        vocabulary,
    }))
}

async fn base_view(client: &McpClient, vault_name: &str, key: &BaseViewKey) -> Option<String> {
    crate::rendering::base::render_view(client, vault_name, &key.path, key.view.as_deref())
        .await
        .ok()
}

#[derive(Debug, Deserialize)]
struct MermaidDiagnosticsResult {
    diagnostics: Vec<crate::rendering::diagnostics::Diagnostic>,
}

/// A Mermaid diagram, rendered by the server.
pub async fn diagram(client: &McpClient, source: &str) -> Diagram {
    let mut args = Map::new();
    args.insert("source".to_owned(), Value::String(source.to_owned()));
    let Ok(result) = client.call_tool("mermaid_render".to_owned(), args).await else {
        return Diagram::Failed(vec![DiagramProblem {
            code: "mcp/unreachable".to_owned(),
            message: "Mermaid rendering is unavailable.".to_owned(),
        }]);
    };
    if let Some(svg) = svg_of(&result) {
        return Diagram::Svg(svg);
    }
    let problems = result.into_typed::<MermaidDiagnosticsResult>().map_or_else(
        |_| {
            vec![DiagramProblem {
                code: "mermaid/render-failed".to_owned(),
                message: "The diagram could not be rendered.".to_owned(),
            }]
        },
        |read| {
            read.diagnostics
                .into_iter()
                .map(|diagnostic| DiagramProblem {
                    code: diagnostic.code,
                    message: diagnostic.message,
                })
                .collect()
        },
    );
    Diagram::Failed(problems)
}

/// The HTML a standalone diagram file shows: the diagram, or the panel naming its problems.
pub async fn diagram_html(client: &McpClient, source: &str) -> String {
    match diagram(client, source).await {
        Diagram::Svg(svg) => format!("<div class=\"mermaid-diagram\">{svg}</div>"),
        Diagram::Failed(problems) => crate::rendering::diagnostics::render_diagnostic_panel(
            &problems
                .into_iter()
                .map(|problem| crate::rendering::diagnostics::Diagnostic {
                    code: problem.code,
                    path: String::new(),
                    message: problem.message,
                })
                .collect::<Vec<_>>(),
        ),
    }
}

fn svg_of(result: &rmcp::model::CallToolResult) -> Option<String> {
    result.content.iter().find_map(|block| match block {
        rmcp::model::ContentBlock::Resource(embedded) => match &embedded.resource {
            rmcp::model::ResourceContents::TextResourceContents {
                mime_type: Some(mime),
                text,
                ..
            } if mime == "image/svg+xml" => Some(text.clone()),
            _ => None,
        },
        _ => None,
    })
}
