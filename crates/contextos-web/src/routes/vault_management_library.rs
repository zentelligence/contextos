//! Standards, Templates, and the Registry: the vault management categories that are folders of source files. Every
//! one shares the same screen: the folder tree beside a main area that reads the selected file rendered by default,
//! with Edit (the whole file in one text box) and New (a name, plus content seeded from the type's template where one
//! is configured). The Registry adds one tab per definition type above that screen, each with its own configurable
//! folder and template.
//!
//! Beneath a library's own URL, the path is the file's or folder's path within the library, with two reserved final
//! segments from the routing contract: `.../edit` loads a file's editor (or, at a registry type's root, the type's
//! locations) and `.../new` loads the new-file form for a folder. `PUT` on a file replaces it whole; `POST` on a
//! folder creates a file in it; `PATCH` on a registry type's root updates its locations. Every path is looked up in
//! the folder listing before it is read or written, so nothing outside the configured folder is ever addressed.

use askama::Template;
use axum::body::Bytes;
use axum::http::{Method, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::inbox_outbox::sanitised_path_segment;
use crate::library_document::{
    self, CONVERTED_LINE_ENDINGS, Document, DocumentBody, LineBreaks, Property, ReadDocumentError, RenderMode,
};
use crate::mcp_client::{McpCallError, McpClient};
use crate::routes::vault::{not_found, unreachable};
use crate::tree_view::{self, TreeEvent};
use crate::vault_catalogue::{Catalogue, ManagementCategory, STANDARDS_DIR, TEMPLATES_DIR, count_definitions};
use crate::vault_content::{AttachedRead, read_attached};
use crate::vault_management_config::{DefinitionLayout, RegistryKind};
use crate::vault_relative_path::VaultRelativePath;
use crate::vault_tree::{FolderListing, VaultFile, VaultFolder};
use crate::vault_write::{WriteOutcome, write_text};

use super::vault_management::{ManagementContext, Outcome, Panel};
use super::vault_management_paths::{PathsView, RegistrySubmission, save_registry_paths};

/// One folder of source files managed from Vault Settings.
struct Library<'a> {
    category: ManagementCategory,
    kind: Option<RegistryKind>,
    root: VaultRelativePath,
    listing: &'a FolderListing,
    template: Option<VaultRelativePath>,
    layout: DefinitionLayout,
    mode: RenderMode,
    title: &'static str,
    singular: &'static str,
    empty_message: &'static str,
    description: &'static str,
}

impl<'a> Library<'a> {
    fn standards(catalogue: &'a Catalogue) -> Self {
        Self {
            category: ManagementCategory::Standards,
            kind: None,
            root: VaultRelativePath::scaffold(STANDARDS_DIR),
            listing: &catalogue.standards,
            template: None,
            layout: DefinitionLayout::Markdown,
            mode: RenderMode::Document,
            title: "Standards",
            singular: "standard",
            empty_message: "No standards yet",
            description: "Language, engineering, and writing standards that agents load for the work they apply to.",
        }
    }

    fn templates(catalogue: &'a Catalogue) -> Self {
        Self {
            category: ManagementCategory::Templates,
            kind: None,
            root: VaultRelativePath::scaffold(TEMPLATES_DIR),
            listing: &catalogue.templates,
            template: None,
            layout: DefinitionLayout::Markdown,
            mode: RenderMode::Template,
            title: "Templates",
            singular: "template",
            empty_message: "No templates yet",
            description: "The files new notes and records start from. Templater commands and {{placeholders}} are \
                          kept exactly as written.",
        }
    }

    fn registry(catalogue: &'a Catalogue, kind: RegistryKind) -> Self {
        let paths = catalogue.loaded.config.registry(kind);
        Self {
            category: ManagementCategory::Registry,
            kind: Some(kind),
            root: paths.dir.clone(),
            listing: catalogue.registry_listing(kind),
            template: paths.template.clone(),
            layout: kind.layout(),
            mode: RenderMode::Document,
            title: kind.label(),
            singular: kind.singular(),
            empty_message: "Nothing defined yet",
            description: kind.description(),
        }
    }

    /// `relative` (a path within the library) as a location beneath the category.
    fn location(&self, relative: &str) -> String {
        match self.kind {
            Some(kind) if relative.is_empty() => kind.slug().to_owned(),
            Some(kind) => format!("{}/{relative}", kind.slug()),
            None => relative.to_owned(),
        }
    }

    fn href(&self, context: &ManagementContext<'_>, relative: &str) -> String {
        context.href(self.category, &self.location(relative))
    }

    fn item_count(&self) -> usize {
        self.listing.folder().map_or(0, |folder| match self.kind {
            Some(kind) => count_definitions(kind, folder),
            None => folder.count_files(&|file| file.is_markdown() && !file.is_index()),
        })
    }

    fn path_of(&self, relative: &str) -> Option<VaultRelativePath> {
        VaultRelativePath::try_from(relative)
            .ok()
            .map(|relative| self.root.join(&relative))
    }
}

/// What a path beneath a library names.
enum Target {
    Folder(String),
    File(String),
    New(String),
    Edit(String),
    TypeSettings,
    Missing,
}

fn resolve(folder: Option<&VaultFolder>, rest: &str, is_registry: bool) -> Target {
    let rest = rest.trim_matches('/');
    let (parent, last) = rest.rsplit_once('/').unwrap_or(("", rest));
    let Some(folder) = folder else {
        return match rest {
            "" => Target::Folder(String::new()),
            "edit" if is_registry => Target::TypeSettings,
            _ => Target::Missing,
        };
    };
    match last {
        "" => Target::Folder(String::new()),
        "new" if folder.find_folder(parent).is_some() => Target::New(parent.to_owned()),
        "edit" if parent.is_empty() && is_registry => Target::TypeSettings,
        "edit" if folder.find_file(parent).is_some() => Target::Edit(parent.to_owned()),
        _ if folder.find_file(rest).is_some() => Target::File(rest.to_owned()),
        _ if folder.find_folder(rest).is_some() => Target::Folder(rest.to_owned()),
        _ => Target::Missing,
    }
}

/// The name a new file in `layout` gets, relative to the folder it is created in.
///
/// # Errors
///
/// Returns a sentence saying why `raw` cannot be used.
fn new_file_name(layout: DefinitionLayout, raw: &str) -> Result<String, String> {
    let name = sanitised_path_segment(raw).map_err(|reason| format!("The name {reason}."))?;
    if name.starts_with('.') {
        return Err("The name must not start with a dot.".to_owned());
    }
    if matches!(name, "new" | "edit") {
        return Err(format!("`{name}` is reserved; choose another name."));
    }
    match layout {
        DefinitionLayout::Markdown => {
            let stem = name
                .strip_suffix(".md")
                .or_else(|| name.strip_suffix(".MD"))
                .unwrap_or(name);
            if stem.is_empty() {
                return Err("The name must not be empty.".to_owned());
            }
            Ok(format!("{stem}.md"))
        }
        DefinitionLayout::AnyFile => match name.rsplit_once('.') {
            Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => Ok(name.to_owned()),
            _ => Err("Include the file's extension, for example `tidy.py`.".to_owned()),
        },
        DefinitionLayout::Folder { entry } => {
            if name.contains('.') {
                return Err("A folder name must not contain a dot.".to_owned());
            }
            Ok(format!("{name}/{entry}"))
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// View model
// ---------------------------------------------------------------------------------------------------------------------

pub(crate) struct RegistryTab {
    label: &'static str,
    href: String,
    count: usize,
    active: bool,
    provisioned: bool,
}

pub(crate) struct TreeLeaf {
    label: String,
    href: String,
    active: bool,
    is_markdown: bool,
}

pub(crate) struct ItemRow {
    label: String,
    href: String,
    is_folder: bool,
    description: String,
}

pub(crate) struct BrowseView {
    heading: String,
    items: Vec<ItemRow>,
    new_href: String,
}

pub(crate) enum ReadBody {
    Markdown {
        properties: Vec<Property>,
        raw_frontmatter: Option<String>,
        html: String,
    },
    Code {
        html: String,
        language: &'static str,
    },
    NotText,
}

pub(crate) struct ReadView {
    name: String,
    path: String,
    edit_href: String,
    body: ReadBody,
}

pub(crate) struct EditView {
    name: String,
    path: String,
    save_href: String,
    cancel_href: String,
    content: String,
    expected_hash: String,
    /// What saving will change beyond the edit itself, said before the save.
    notice: Option<&'static str>,
    error: Option<String>,
    /// Why this file is shown here but cannot be saved from here, when it cannot.
    locked: Option<String>,
}

pub(crate) struct NewView {
    folder_label: String,
    create_href: String,
    cancel_href: String,
    name: String,
    content: String,
    name_hint: String,
    name_placeholder: &'static str,
    notice: Option<String>,
    error: Option<String>,
}

pub(crate) enum MainView {
    Browse(BrowseView),
    Read(ReadView),
    Edit(EditView),
    New(NewView),
    Unprovisioned,
    Unreadable,
}

#[derive(Template)]
#[template(path = "vm_library.html")]
struct LibraryTemplate {
    icon: &'static str,
    heading: &'static str,
    description: &'static str,
    /// The selected registry type's own description, shown under the tabs.
    type_description: &'static str,
    singular: &'static str,
    problem: Option<String>,
    tabs: Vec<RegistryTab>,
    paths: Option<PathsView>,
    is_empty: bool,
    empty_message: &'static str,
    tree_events: Vec<TreeEvent<TreeLeaf>>,
    root_href: String,
    root_active: bool,
    main: MainView,
}

fn registry_tabs(context: &ManagementContext<'_>, active: RegistryKind) -> Vec<RegistryTab> {
    RegistryKind::ALL
        .into_iter()
        .map(|kind| {
            let listing = context.catalogue.registry_listing(kind);
            RegistryTab {
                label: kind.label(),
                href: context.href(ManagementCategory::Registry, kind.slug()),
                count: listing.folder().map_or(0, |folder| count_definitions(kind, folder)),
                active: kind == active,
                provisioned: !matches!(listing, FolderListing::Missing),
            }
        })
        .collect()
}

fn tree_events(context: &ManagementContext<'_>, library: &Library<'_>, selected: &str) -> Vec<TreeEvent<TreeLeaf>> {
    let Some(folder) = library.listing.folder() else {
        return Vec::new();
    };
    let anchor = selected.rsplit_once('/').map_or(selected, |(parent, _)| parent);
    let anchor = if folder.find_folder(selected).is_some() {
        selected
    } else {
        anchor
    };
    tree_view::flatten(folder, anchor, &mut |file: &VaultFile| TreeLeaf {
        label: file.name.clone(),
        href: library.href(context, &file.relative_path),
        active: file.relative_path == selected,
        is_markdown: file.is_markdown(),
    })
}

#[derive(Debug, Deserialize)]
struct BaseQueryResult {
    content: String,
}

/// Each Markdown file's `description` frontmatter in `dir`, by file name: one `base_query` for the whole folder.
async fn folder_descriptions(
    client: &McpClient,
    vault_name: &str,
    dir: &VaultRelativePath,
) -> std::collections::HashMap<String, String> {
    let filter = format!(
        "file.folder == {}",
        serde_json::to_string(dir.as_str()).unwrap_or_default()
    );
    let mut args = Map::new();
    args.insert("vault".to_owned(), Value::String(format!("{vault_name}://.")));
    args.insert("format".to_owned(), Value::String("json".to_owned()));
    args.insert(
        "definition".to_owned(),
        json!({
            "filters": filter,
            "order": ["file.name", "description"],
        }),
    );
    let Ok(result) = client.call_tool("base_query".to_owned(), args).await else {
        return std::collections::HashMap::new();
    };
    if result.is_error == Some(true) {
        return std::collections::HashMap::new();
    }
    let rows: Vec<Map<String, Value>> = result
        .into_typed::<BaseQueryResult>()
        .ok()
        .and_then(|parsed| serde_json::from_str(&parsed.content).ok())
        .unwrap_or_default();
    rows.into_iter()
        .filter_map(|row| {
            let name = row.get("file.name")?.as_str()?.to_owned();
            let description = row.get("description")?.as_str()?.trim().to_owned();
            (!description.is_empty()).then_some((name, description))
        })
        .collect()
}

async fn browse_view(context: &ManagementContext<'_>, library: &Library<'_>, relative: &str) -> MainView {
    let Some(root) = library.listing.folder() else {
        return MainView::Unprovisioned;
    };
    let Some(folder) = root.find_folder(relative) else {
        return MainView::Unprovisioned;
    };
    let dir = if relative.is_empty() {
        library.root.clone()
    } else {
        library.path_of(relative).unwrap_or_else(|| library.root.clone())
    };
    let descriptions = folder_descriptions(context.client, context.vault_name, &dir).await;
    let mut items: Vec<ItemRow> = folder
        .folders
        .iter()
        .map(|child| {
            let entry = match library.layout {
                DefinitionLayout::Folder { entry } => child.files.iter().find(|file| file.name == entry),
                DefinitionLayout::Markdown | DefinitionLayout::AnyFile => None,
            };
            ItemRow {
                label: child.name.clone(),
                href: library.href(context, entry.map_or(&child.relative_path, |file| &file.relative_path)),
                is_folder: entry.is_none(),
                description: String::new(),
            }
        })
        .collect();
    items.extend(folder.files.iter().filter(|file| !file.is_index()).map(|file| ItemRow {
        label: if file.is_markdown() {
            file.stem().to_owned()
        } else {
            file.name.clone()
        },
        href: library.href(context, &file.relative_path),
        is_folder: false,
        description: descriptions.get(&file.name).cloned().unwrap_or_default(),
    }));
    let new_location = if relative.is_empty() {
        "new".to_owned()
    } else {
        format!("{relative}/new")
    };
    MainView::Browse(BrowseView {
        heading: if relative.is_empty() {
            library.title.to_owned()
        } else {
            relative.to_owned()
        },
        items,
        new_href: library.href(context, &new_location),
    })
}

impl From<DocumentBody> for ReadBody {
    fn from(body: DocumentBody) -> Self {
        match body {
            DocumentBody::Markdown {
                properties,
                raw_frontmatter,
                html,
            } => Self::Markdown {
                properties,
                raw_frontmatter,
                html,
            },
            DocumentBody::Code { html, language } => Self::Code {
                html,
                language: language.label(),
            },
            DocumentBody::NotText => Self::NotText,
        }
    }
}

async fn read_library_file(
    context: &ManagementContext<'_>,
    library: &Library<'_>,
    relative: &str,
) -> Result<Document, Outcome> {
    let Some(path) = library.path_of(relative) else {
        return Err(Outcome::from(not_found()));
    };
    match library_document::read_document(context.client, context.vault_name, &path, library.mode).await {
        Ok(document) => Ok(document),
        Err(ReadDocumentError::Refused { .. }) => Err(Outcome::from(not_found())),
        Err(ReadDocumentError::Unreachable(_)) => Err(Outcome::from(unreachable())),
    }
}

fn file_label(relative: &str) -> String {
    relative.rsplit('/').next().unwrap_or(relative).to_owned()
}

async fn read_view(
    context: &ManagementContext<'_>,
    library: &Library<'_>,
    relative: &str,
) -> Result<MainView, Outcome> {
    let document = read_library_file(context, library, relative).await?;
    Ok(MainView::Read(ReadView {
        name: file_label(relative),
        path: library
            .path_of(relative)
            .map(|path| path.to_string())
            .unwrap_or_default(),
        edit_href: library.href(context, &format!("{relative}/edit")),
        body: document.body.into(),
    }))
}

/// The largest file offered for editing in the browser; a larger one is read here and edited elsewhere.
const EDITABLE_BYTES: usize = 1024 * 1024;

async fn edit_view(
    context: &ManagementContext<'_>,
    library: &Library<'_>,
    relative: &str,
) -> Result<MainView, Outcome> {
    let document = read_library_file(context, library, relative).await?;
    if matches!(document.body, DocumentBody::NotText) {
        // Nothing to edit: show the file as read, which says it is not text.
        return read_view(context, library, relative).await;
    }
    let line_breaks = LineBreaks::from(document.raw.as_str());
    let locked = if document.raw.len() > EDITABLE_BYTES {
        Some(format!(
            "This file is {} KB, larger than the {} KB this editor handles, so it is shown here but not saved from \
             here.",
            document.raw.len() / 1024,
            EDITABLE_BYTES / 1024
        ))
    } else {
        None
    };
    Ok(MainView::Edit(EditView {
        name: file_label(relative),
        path: library
            .path_of(relative)
            .map(|path| path.to_string())
            .unwrap_or_default(),
        save_href: library.href(context, relative),
        cancel_href: library.href(context, relative),
        notice: (line_breaks == LineBreaks::Converted).then_some(CONVERTED_LINE_ENDINGS),
        content: document.raw,
        expected_hash: document.content_hash.unwrap_or_default(),
        error: None,
        locked,
    }))
}

fn name_guidance(library: &Library<'_>) -> (String, &'static str) {
    match library.layout {
        DefinitionLayout::Markdown => (
            format!("The {} file's name; `.md` is added for you.", library.singular),
            "my-new-note",
        ),
        DefinitionLayout::AnyFile => ("The file's name, including its extension.".to_owned(), "tidy-links.py"),
        DefinitionLayout::Folder { entry } => (
            format!(
                "The {} folder's name; `{entry}` is created inside it.",
                library.singular
            ),
            "my-skill",
        ),
    }
}

/// The content a new file starts from: the library's configured template, or empty with a notice when the template
/// cannot be read.
async fn seed_content(context: &ManagementContext<'_>, library: &Library<'_>) -> (String, Option<String>) {
    let Some(template) = &library.template else {
        return (String::new(), None);
    };
    match read_attached(context.client, &format!("{}://{template}", context.vault_name)).await {
        Ok(AttachedRead::Text(attached)) => (attached.text, None),
        _ => (
            String::new(),
            Some(format!(
                "The configured template `{template}` could not be read, so this starts blank."
            )),
        ),
    }
}

async fn new_view(context: &ManagementContext<'_>, library: &Library<'_>, folder: &str) -> MainView {
    let (seed, notice) = seed_content(context, library).await;
    let (name_hint, name_placeholder) = name_guidance(library);
    MainView::New(NewView {
        folder_label: if folder.is_empty() {
            library.root.to_string()
        } else {
            library.path_of(folder).map(|path| path.to_string()).unwrap_or_default()
        },
        create_href: library.href(context, folder),
        cancel_href: library.href(context, folder),
        name: String::new(),
        content: seed,
        name_hint,
        name_placeholder,
        notice,
        error: None,
    })
}

fn render_panel(
    context: &ManagementContext<'_>,
    library: &Library<'_>,
    main: MainView,
    paths: Option<PathsView>,
    selected: &str,
) -> Panel {
    let (icon, heading, description) = match library.kind {
        Some(_) => (
            ManagementCategory::Registry.icon(),
            "Registry",
            "Definitions the vault's skills and agents load by name.",
        ),
        None => (library.category.icon(), library.title, library.description),
    };
    let problem = match library.listing {
        FolderListing::Unreadable { code } => Some(format!(
            "`{}` could not be listed{}.",
            library.root,
            code.as_ref().map(|code| format!(" ({code})")).unwrap_or_default()
        )),
        FolderListing::Present(_) | FolderListing::Missing => context.catalogue.loaded.problem(),
    };
    let template = LibraryTemplate {
        icon,
        heading,
        description,
        type_description: library.description,
        singular: library.singular,
        problem,
        tabs: library
            .kind
            .map(|kind| registry_tabs(context, kind))
            .unwrap_or_default(),
        paths,
        is_empty: library.item_count() == 0,
        empty_message: library.empty_message,
        tree_events: tree_events(context, library, selected),
        root_href: library.href(context, ""),
        root_active: selected.is_empty(),
        main,
    };
    Panel {
        title: library.kind.map_or_else(
            || library.title.to_owned(),
            |kind| format!("Registry · {}", kind.label()),
        ),
        html: template
            .render()
            .unwrap_or_else(|_| "<p class=\"hint error\">This page could not be rendered.</p>".to_owned()),
    }
}

fn paths_view(context: &ManagementContext<'_>, library: &Library<'_>, editing: bool) -> Option<PathsView> {
    let kind = library.kind?;
    let mut view = PathsView::registry(&context.catalogue.loaded, kind, library.href(context, ""));
    view.editing = editing;
    Some(view)
}

async fn show(context: &ManagementContext<'_>, library: &Library<'_>, target: Target) -> Outcome {
    let (main, selected, editing_paths) = match target {
        Target::Folder(relative) => (browse_view(context, library, &relative).await, relative, false),
        Target::TypeSettings => (browse_view(context, library, "").await, String::new(), true),
        Target::File(relative) => match read_view(context, library, &relative).await {
            Ok(main) => (main, relative, false),
            Err(outcome) => return outcome,
        },
        Target::Edit(relative) => match edit_view(context, library, &relative).await {
            Ok(main) => (main, relative, false),
            Err(outcome) => return outcome,
        },
        Target::New(folder) => (new_view(context, library, &folder).await, folder, false),
        Target::Missing => return Outcome::from(not_found()),
    };
    let main = match (&main, library.listing) {
        (MainView::Unprovisioned, FolderListing::Unreadable { .. }) => MainView::Unreadable,
        _ => main,
    };
    let paths = paths_view(context, library, editing_paths);
    Outcome::Show(render_panel(context, library, main, paths, &selected))
}

// ---------------------------------------------------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveSubmission {
    content: String,
    expected_hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateSubmission {
    name: String,
    #[serde(default)]
    content: String,
}

use super::vault_management::malformed;

async fn save_file(context: &ManagementContext<'_>, library: &Library<'_>, relative: &str, body: &Bytes) -> Outcome {
    let Ok(submitted) = serde_json::from_slice::<SaveSubmission>(body) else {
        return malformed();
    };
    let Some(path) = library.path_of(relative) else {
        return Outcome::from(not_found());
    };
    let error = if submitted.expected_hash.is_empty() {
        "This file's version could not be confirmed, so it was not saved. Reopen it and try again.".to_owned()
    } else {
        match write_text(
            context.client,
            context.vault_name,
            &path,
            &submitted.content,
            Some(&submitted.expected_hash),
        )
        .await
        {
            Ok(WriteOutcome::Written) => {
                return Outcome::Wrote {
                    location: library.location(relative),
                };
            }
            Ok(refused) => refused.refusal_reason().unwrap_or_default(),
            Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => return Outcome::from(unreachable()),
        }
    };
    let main = MainView::Edit(EditView {
        name: file_label(relative),
        path: path.to_string(),
        save_href: library.href(context, relative),
        cancel_href: library.href(context, relative),
        content: submitted.content,
        expected_hash: submitted.expected_hash,
        notice: None,
        error: Some(error),
        locked: None,
    });
    Outcome::Show(render_panel(
        context,
        library,
        main,
        paths_view(context, library, false),
        relative,
    ))
}

async fn create_file(context: &ManagementContext<'_>, library: &Library<'_>, folder: &str, body: &Bytes) -> Outcome {
    let Ok(submitted) = serde_json::from_slice::<CreateSubmission>(body) else {
        return malformed();
    };
    let created = new_file_name(library.layout, &submitted.name).and_then(|name| {
        let relative = if folder.is_empty() {
            name
        } else {
            format!("{folder}/{name}")
        };
        library
            .path_of(&relative)
            .map(|path| (relative, path))
            .ok_or_else(|| "That name cannot be used here.".to_owned())
    });
    let error = match created {
        Err(error) => error,
        Ok((relative, path)) => {
            match write_text(context.client, context.vault_name, &path, &submitted.content, None).await {
                Ok(WriteOutcome::Written) => {
                    return Outcome::Wrote {
                        location: library.location(&relative),
                    };
                }
                Ok(refused) => refused.refusal_reason().unwrap_or_default(),
                Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => {
                    return Outcome::from(unreachable());
                }
            }
        }
    };
    let (name_hint, name_placeholder) = name_guidance(library);
    let main = MainView::New(NewView {
        folder_label: if folder.is_empty() {
            library.root.to_string()
        } else {
            library.path_of(folder).map(|path| path.to_string()).unwrap_or_default()
        },
        create_href: library.href(context, folder),
        cancel_href: library.href(context, folder),
        name: submitted.name,
        content: submitted.content,
        name_hint,
        name_placeholder,
        notice: None,
        error: Some(error),
    });
    Outcome::Show(render_panel(
        context,
        library,
        main,
        paths_view(context, library, false),
        folder,
    ))
}

async fn update_type_paths(context: &ManagementContext<'_>, library: &Library<'_>, body: &Bytes) -> Outcome {
    let Some(kind) = library.kind else {
        return Outcome::from(StatusCode::METHOD_NOT_ALLOWED.into_response());
    };
    let Ok(submitted) = serde_json::from_slice::<RegistrySubmission>(body) else {
        return malformed();
    };
    match save_registry_paths(context, kind, &submitted).await {
        Ok(()) => Outcome::Wrote {
            location: kind.slug().to_owned(),
        },
        Err(error) => {
            let paths = paths_view(context, library, true)
                .map(|view| view.rejected(&submitted.values(), &submitted.expected_hash, error));
            let main = browse_view(context, library, "").await;
            Outcome::Show(render_panel(context, library, main, paths, ""))
        }
    }
}

async fn handle_library(
    context: &ManagementContext<'_>,
    library: &Library<'_>,
    rest: &str,
    method: &Method,
    body: &Bytes,
) -> Outcome {
    let target = resolve(library.listing.folder(), rest, library.kind.is_some());
    match (method, target) {
        (&Method::GET, target) => show(context, library, target).await,
        (&Method::PUT, Target::File(relative)) => save_file(context, library, &relative, body).await,
        (&Method::POST, Target::Folder(folder)) if library.listing.folder().is_some() => {
            create_file(context, library, &folder, body).await
        }
        (&Method::PATCH, Target::Folder(folder)) if folder.is_empty() && library.kind.is_some() => {
            update_type_paths(context, library, body).await
        }
        (_, Target::Missing) => Outcome::from(not_found()),
        _ => Outcome::from(StatusCode::METHOD_NOT_ALLOWED.into_response()),
    }
}

/// Answers a request beneath the Standards, Templates, or Registry category.
pub(super) async fn handle(
    context: &ManagementContext<'_>,
    category: ManagementCategory,
    rest: &str,
    method: &Method,
    body: &Bytes,
) -> Outcome {
    match category {
        ManagementCategory::Standards => {
            handle_library(context, &Library::standards(context.catalogue), rest, method, body).await
        }
        ManagementCategory::Templates => {
            handle_library(context, &Library::templates(context.catalogue), rest, method, body).await
        }
        ManagementCategory::Registry => {
            let rest = rest.trim_matches('/');
            let (first, beneath) = rest.split_once('/').unwrap_or((rest, ""));
            let kind = if first.is_empty() {
                RegistryKind::ALL
                    .into_iter()
                    .find(|kind| !matches!(context.catalogue.registry_listing(*kind), FolderListing::Missing))
                    .unwrap_or(RegistryKind::Agents)
            } else {
                match RegistryKind::try_from(first) {
                    Ok(kind) => kind,
                    Err(_) => return Outcome::from(not_found()),
                }
            };
            if first.is_empty() && method != Method::GET {
                return Outcome::from(StatusCode::METHOD_NOT_ALLOWED.into_response());
            }
            handle_library(
                context,
                &Library::registry(context.catalogue, kind),
                beneath,
                method,
                body,
            )
            .await
        }
        ManagementCategory::Decisions | ManagementCategory::Activity => Outcome::from(not_found()),
    }
}

#[cfg(test)]
#[path = "vault_management_library_test.rs"]
mod tests;
