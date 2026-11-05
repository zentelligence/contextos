//! The Notes screen's fragments: the note list beside the file it opens, and the "New" form. Split from
//! `routes::notes` to keep each file within the line limit.

use askama::Template;

use super::{
    CreatingView, DateStructureSide, McpCallError, McpClient, NotesConfig, NotesTemplate, PageFileView,
    load_error_message, metadata_panel, notes, notes_view, selected_note_view,
};

/// Loads and renders the Notes screen: every real note's own sidebar row,
/// most-recently-dated first, and whichever one is selected.
///
/// `requested_note` selects which note starts open: when it names a note
/// still present in the freshly-loaded list, that one; otherwise (absent,
/// or naming one no longer there - a stale link, or a note just renamed
/// away from it) the most recent note - the most calendar-recent note
/// within the configured date hierarchy
/// ([`notes_view::most_recent_dated_note`]) when `config.date_structure_active`
/// and at least one note actually matches it, otherwise the freshly-loaded
/// list's own first entry ([`notes::list_notes`]'s frontmatter-based order);
/// `None` when the list itself is empty (nothing to select, no error of its
/// own - the template's own empty-state prompt). Shared by [`get`] and every
/// action that follows it back to this same screen (create, save, rename),
/// always re-reading fresh rather than assuming what a prior read already
/// knew is still true.
pub(super) async fn render_notes_fragment(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
    requested_note: Option<&str>,
    error: Option<String>,
) -> String {
    Box::pin(render_notes_selection(
        client,
        vault_name,
        config,
        requested_note,
        error,
    ))
    .await
    .0
}

/// [`render_notes_fragment`], with the path (within the notes folder) of the file it shows, if it shows one.
pub(super) async fn render_notes_selection(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
    requested_note: Option<&str>,
    error: Option<String>,
) -> (String, Option<String>) {
    let summaries = notes::list_notes(client, vault_name, config).await;
    // A base, canvas, or diagram asked for by name is shown on its own, read-only: the note list holds notes only.
    if let Some(requested) = requested_note
        && let Ok(kind) = crate::file_view::PageFile::try_from(requested)
    {
        let full_path = notes::note_path(config, requested);
        if crate::file_view::page_file_exists(client, vault_name, &full_path).await {
            let page = match crate::file_view::page_file_html(client, vault_name, kind, &full_path, None).await {
                Ok(html) => PageFileView {
                    title: requested.rsplit('/').next().unwrap_or(requested).to_owned(),
                    html,
                },
                Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => PageFileView {
                    title: requested.to_owned(),
                    html: "<p class=\"hint error\">The vault could not be reached.</p>".to_owned(),
                },
            };
            let split = notes_view::build_split_view(vault_name, &summaries, Some(requested), config);
            let fragment = NotesTemplate {
                urls: crate::urls::VaultUrls::from(vault_name),
                date_structure_active: config.date_structure_active,
                date_structure_side_is_left: config.date_structure_side == DateStructureSide::Left,
                metadata_panel_side_is_left: config.metadata_panel_side == crate::panel_side::PanelSide::Left,
                regular_tree_events: notes_view::flatten(&split.regular),
                dated_tree_events: split.dated.as_ref().map(notes_view::flatten).unwrap_or_default(),
                selected: None,
                page_file: Some(page),
                creating: None,
                error,
            }
            .render()
            .unwrap_or_else(|_| "<p>The Notes screen could not be rendered.</p>".to_owned());
            return (fragment, Some(requested.to_owned()));
        }
    }
    let selected_relative = requested_note
        .filter(|note| summaries.iter().any(|summary| summary.relative_path == *note))
        .map(str::to_owned)
        .or_else(|| notes_view::most_recent_dated_note(&summaries, config).map(|summary| summary.relative_path.clone()))
        .or_else(|| summaries.first().map(|summary| summary.relative_path.clone()));

    let (selected, load_error) = match &selected_relative {
        Some(relative_path) => match notes::load_note(client, vault_name, config, relative_path).await {
            Ok(state) => {
                let mut view = selected_note_view(&state);
                let note_path = notes::note_path(config, relative_path);
                view.metadata_fields =
                    metadata_panel::linked_field_views(client, vault_name, &note_path, &state.frontmatter).await;
                (Some(view), None)
            }
            Err(error) => (None, Some(load_error_message(&error))),
        },
        None => (None, None),
    };

    // A note that could not be loaded is not shown, so the page shows no file.
    let has_selection = selected.is_some();
    let split = notes_view::build_split_view(vault_name, &summaries, selected_relative.as_deref(), config);
    let fragment = NotesTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        page_file: None,
        date_structure_active: config.date_structure_active,
        date_structure_side_is_left: config.date_structure_side == DateStructureSide::Left,
        metadata_panel_side_is_left: config.metadata_panel_side == crate::panel_side::PanelSide::Left,
        regular_tree_events: notes_view::flatten(&split.regular),
        dated_tree_events: split.dated.as_ref().map(notes_view::flatten).unwrap_or_default(),
        selected,
        creating: None,
        error: error.or(load_error),
    }
    .render()
    .unwrap_or_else(|_| "<p>The Notes screen could not be rendered.</p>".to_owned());
    (fragment, selected_relative.filter(|_| has_selection))
}

/// `GET /{{vault_name}}/notes/new`: the "New" form itself ([`docs/http-routing-response-contract.md`]'s
/// `GET /{entities}/new` pattern), pre-filled from [`notes::new_note_defaults`] so the
/// operator sees a real, save-ready path and body rather than a blank form. The sidebar
/// stays the same real note list every other Notes response shows; only the main pane
/// differs (`creating` instead of `selected`, never both).
/// Renders the "New" form fragment with `creating` as its own pre-filled state: either
/// freshly computed defaults ([`load_new_form_state`], `GET .../new`'s own call) or the
/// operator's own just-submitted (and rejected) values, preserved rather than lost
/// ([`create_root`]'s own error path) - the same "redisplay what was actually submitted"
/// contract every other form-error path on this screen already follows.
pub(super) async fn render_new_form_fragment(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
    creating: CreatingView,
    error: Option<String>,
) -> String {
    let summaries = notes::list_notes(client, vault_name, config).await;
    let split = notes_view::build_split_view(vault_name, &summaries, None, config);
    NotesTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        page_file: None,
        date_structure_active: config.date_structure_active,
        date_structure_side_is_left: config.date_structure_side == DateStructureSide::Left,
        metadata_panel_side_is_left: config.metadata_panel_side == crate::panel_side::PanelSide::Left,
        regular_tree_events: notes_view::flatten(&split.regular),
        dated_tree_events: split.dated.as_ref().map(notes_view::flatten).unwrap_or_default(),
        selected: None,
        creating: Some(creating),
        error,
    }
    .render()
    .unwrap_or_else(|_| "<p>The Notes screen could not be rendered.</p>".to_owned())
}
