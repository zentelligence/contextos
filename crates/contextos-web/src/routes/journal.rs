//! The Journal screen: `GET /{{vault_name}}/journal/{{date}}`, and each of the
//! day's sections as a sub-resource, `GET`, `GET .../edit`, and `PATCH` on
//! `/{{vault_name}}/journal/{{date}}/sections/{{index}}`.
//!
//! The screen has one tab per `##` section of the day's entry, in the order
//! written; a day with no entry yet shows the configured template, rendered
//! for that day, which is what its first save creates. Nothing here knows a
//! section by name: the only sections treated differently are the ones
//! `.contextos/journal.toml` lists as read-only. Every section of every day is
//! otherwise editable, as the Markdown it holds ([`crate::journal_entry`]).
//!
//! `date` is a real path segment, never a `?date=` query parameter, and one
//! that is not a real calendar date (garbage, or `2026-02-30`) is a genuine
//! `404`: a garbled date never named a resource. The bare
//! `/{{vault_name}}/journal` root redirects to today's date path.
//!
//! A save replaces one section and leaves every other byte of the entry as
//! it is on disk at that moment. It is checked against the version of that
//! section the edit form was opened on, never the whole file, so an append
//! elsewhere in the entry (a task being listed, an agent's log) never
//! refuses it; a refused save keeps what was typed and shows the section's
//! current content beside it.

use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use time::Date;

use crate::config;
use crate::date_structure::DateStructureSide;
use crate::journal::{self, Day, LoadEntryError, SaveOutcome, WriteEntryError};
use crate::journal_config::{self, JournalConfig};
use crate::journal_entry::{EntryInput, JournalEntry, SectionEdit, SectionEditRefused, SectionFingerprint};
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::routes::file_response::{self, FileRequest};
use crate::routes::journal_view;
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::urls::VaultUrls;

#[derive(Clone)]
pub struct JournalRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
    /// The operator's local time, for every date this screen writes into a vault.
    pub vault_time: Arc<crate::vault_time::VaultTime>,
}

impl From<super::CommonRoutesState> for JournalRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self {
            clients: value.clients,
            web_config_path: value.web_config_path,
            vault_time: value.vault_time,
        }
    }
}

impl JournalRoutesState {
    fn client(&self) -> Result<Arc<McpClient>, Box<Response>> {
        crate::routes::primary_session(&self.clients)
    }
}

async fn ensure_vault(client: &McpClient, vault_name: &str) -> Result<(), Box<Response>> {
    match crate::apps::vault_exists(client, vault_name).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Box::new(not_found())),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Err(Box::new(unreachable())),
    }
}

/// The session and the day a request names, or the response that ends it: an unreachable session, an unknown vault,
/// or a `date` that is not a real calendar date.
async fn open_day(
    state: &JournalRoutesState,
    vault_name: &str,
    date: &str,
) -> Result<(Arc<McpClient>, Date), Box<Response>> {
    let client = state.client()?;
    ensure_vault(&client, vault_name).await?;
    let viewed = journal::parse_date(date).ok_or_else(|| Box::new(not_found()))?;
    Ok((client, viewed))
}

/// A full-navigation `Redirect` for a plain browser `GET`, or the HTMX-native `HX-Redirect` header on a `200` for an
/// `hx-request` (`docs/http-routing-response-contract.md`'s "use `HX-Redirect` for HTMX, a real 3xx otherwise").
fn redirect_response(target: &str, headers: &HeaderMap) -> Response {
    if headers.contains_key("hx-request") {
        (StatusCode::OK, [("HX-Redirect", target.to_owned())], String::new()).into_response()
    } else {
        Redirect::to(target).into_response()
    }
}

/// The day a request is about, and what it is read with.
#[derive(Clone, Copy)]
struct DayRequest<'a> {
    client: &'a McpClient,
    vault_name: &'a str,
    config: &'a JournalConfig,
    viewed: Date,
}

// ---------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------

/// A section's tab label: its heading, or a stand-in when it has none.
fn section_label(entry: &JournalEntry, index: usize) -> String {
    match entry.sections().get(index) {
        Some(section) if section.whole_body => "Entry".to_owned(),
        Some(section) if section.heading.is_empty() => "Untitled".to_owned(),
        Some(section) => section.heading.clone(),
        None => String::new(),
    }
}

/// Renders each of `texts` (pieces of the entry, all body) to HTML in one batch, so what they link to and embed is
/// fetched once.
async fn rendered(client: &McpClient, vault_name: &str, texts: &[&str]) -> Vec<String> {
    crate::note_render::render_texts(client, vault_name, texts, &[])
        .await
        .notes
        .into_iter()
        .map(|note| note.html)
        .collect()
}

/// The entry as shown: the server's copy with its links expanded, so they resolve, when it has exactly the same
/// sections as the entry itself; otherwise the entry as read.
async fn shown_entry(client: &McpClient, vault_name: &str, day: &Day) -> JournalEntry {
    if !day.exists {
        return day.entry.clone();
    }
    let expanded = crate::vault_content::read_expanded_text(client, &format!("{vault_name}://{}", day.path())).await;
    let Ok(Some(expanded)) = expanded else {
        return day.entry.clone();
    };
    let shown = JournalEntry::from(EntryInput {
        text: &expanded.text,
        template: day.template(),
    });
    let same_sections = shown.sections().len() == day.entry.sections().len()
        && shown
            .sections()
            .iter()
            .zip(day.entry.sections())
            .all(|(left, right)| left.heading == right.heading);
    if same_sections { shown } else { day.entry.clone() }
}

#[derive(Template)]
#[template(path = "journal_pane.html")]
struct PaneTemplate {
    index: usize,
    label: String,
    hidden: bool,
    read_only: bool,
    guidance_html: String,
    body_html: String,
    is_empty: bool,
    edit_url: String,
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "journal_pane_edit.html")]
struct PaneEditTemplate {
    index: usize,
    label: String,
    guidance_html: String,
    heading: String,
    base: String,
    content: String,
    section_url: String,
    day_url: String,
    error: Option<String>,
    /// The section's current content, shown beside what was typed when a save found it changed.
    current_html: Option<String>,
}

/// Which panes to render, and how.
struct PaneChoice {
    /// The pane shown first; every other read view starts hidden.
    active: usize,
    /// The pane rendered as its edit form, if any.
    edit: Option<usize>,
    /// Render only this pane (a fragment answering one section's request), rather than every pane.
    only: Option<usize>,
    /// A message for the pane `only` names.
    error: Option<String>,
}

/// Renders the chosen panes: each section's read view, or its edit form, with what each shows rendered in one batch.
async fn render_panes(request: DayRequest<'_>, day: &Day, choice: PaneChoice) -> Vec<String> {
    let DayRequest {
        client,
        vault_name,
        config,
        viewed,
    } = request;
    let shown = shown_entry(client, vault_name, day).await;
    let indices: Vec<usize> = match choice.only {
        Some(index) => vec![index],
        None => (0..day.entry.sections().len()).collect(),
    };
    let mut texts: Vec<&str> = Vec::with_capacity(indices.len() * 2);
    for index in &indices {
        texts.push(shown.content(*index).unwrap_or_default());
        let guidance = day
            .entry
            .sections()
            .get(*index)
            .and_then(|section| section.guidance.as_deref());
        texts.push(guidance.unwrap_or_default());
    }
    let html = rendered(client, vault_name, &texts).await;
    let urls = VaultUrls::from(vault_name);
    let date = journal::format_date(viewed);
    indices
        .iter()
        .enumerate()
        .map(|(position, &index)| {
            let body_html = html.get(position * 2).cloned().unwrap_or_default();
            let guidance_html = html.get(position * 2 + 1).cloned().unwrap_or_default();
            let heading = day
                .entry
                .sections()
                .get(index)
                .map(|section| section.heading.as_str())
                .unwrap_or_default();
            let read_only = config.is_read_only(heading);
            let error = if choice.only == Some(index) {
                choice.error.clone()
            } else {
                None
            };
            if choice.edit == Some(index) && !read_only {
                PaneEditTemplate {
                    index,
                    label: section_label(&day.entry, index),
                    guidance_html,
                    heading: heading.to_owned(),
                    base: day
                        .entry
                        .fingerprint(index)
                        .map(|base| base.as_str().to_owned())
                        .unwrap_or_default(),
                    content: day.entry.content(index).unwrap_or_default().to_owned(),
                    section_url: urls.journal_section(&date, index),
                    day_url: urls.journal_day(&date),
                    error,
                    current_html: None,
                }
                .render()
            } else {
                PaneTemplate {
                    index,
                    label: section_label(&day.entry, index),
                    hidden: index != choice.active,
                    read_only,
                    guidance_html,
                    body_html,
                    is_empty: day.entry.content(index).unwrap_or_default().is_empty(),
                    edit_url: urls.journal_section_edit(&date, index),
                    error,
                }
                .render()
            }
            .unwrap_or_else(|_| "<p class=\"hint error\">This section could not be shown.</p>".to_owned())
        })
        .collect()
}

/// One section's pane alone, as its read view or its edit form, with `error` shown in it.
async fn render_pane(request: DayRequest<'_>, day: &Day, index: usize, edit: bool, error: Option<String>) -> String {
    let choice = PaneChoice {
        active: index,
        edit: edit.then_some(index),
        only: Some(index),
        error,
    };
    render_panes(request, day, choice)
        .await
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// A section index from the path: a plain decimal number, written as it would be written back, else `None`.
fn section_index(raw: &str) -> Option<usize> {
    raw.parse::<usize>().ok().filter(|index| index.to_string() == raw)
}

/// One tab of the section navigation.
struct Tab {
    index: usize,
    label: String,
}

#[derive(Template)]
#[template(path = "journal.html")]
struct JournalTemplate {
    /// Every URL the template links to within the vault.
    urls: VaultUrls,
    date_value: String,
    is_today: bool,
    weekday_label: String,
    iso_week: u8,
    prev_href: String,
    next_href: String,
    calendar: journal_view::Calendar,
    tree: Vec<journal_view::TreeYear>,
    /// Whether the date-tree sidebar renders before the section nav (`true`) or after the main content (`false`).
    folder_pane_side_is_left: bool,
    tabs: Vec<Tab>,
    active: usize,
    panes: Vec<String>,
    error: Option<String>,
}

/// The whole day: header, calendar, sidebar tree, and every section, `active` the section shown first and `edit` the
/// one open for editing, if any.
async fn render_day_fragment(request: DayRequest<'_>, today: Date, active: usize, edit: Option<usize>) -> String {
    let DayRequest {
        client,
        vault_name,
        config,
        viewed,
    } = request;
    let (day, entry_dates) = tokio::join!(
        journal::load_day(client, vault_name, config, viewed),
        journal::entry_dates(client, vault_name, config),
    );
    let (tabs, panes, error) = match day {
        Ok(day) => {
            let tabs = (0..day.entry.sections().len())
                .map(|index| Tab {
                    index,
                    label: section_label(&day.entry, index),
                })
                .collect();
            let choice = PaneChoice {
                active,
                edit,
                only: None,
                error: None,
            };
            let panes = render_panes(request, &day, choice).await;
            (tabs, panes, None)
        }
        Err(error) => (
            Vec::new(),
            Vec::new(),
            Some(format!("This day's entry could not be prepared: {error}.")),
        ),
    };
    let urls = VaultUrls::from(vault_name);
    JournalTemplate {
        date_value: journal::format_date(viewed),
        is_today: viewed == today,
        weekday_label: journal::weekday_name(viewed.weekday()).to_owned(),
        iso_week: viewed.iso_week(),
        prev_href: urls.journal_day(&journal::format_date(journal::add_days(viewed, -1))),
        next_href: urls.journal_day(&journal::format_date(journal::add_days(viewed, 1))),
        calendar: journal_view::build_calendar(viewed, today, &entry_dates),
        tree: journal_view::build_tree(&entry_dates, viewed, today),
        folder_pane_side_is_left: config.date_structure_side == DateStructureSide::Left,
        tabs,
        active,
        panes,
        error,
        urls,
    }
    .render()
    .unwrap_or_else(|_| "<p>The Journal could not be rendered.</p>".to_owned())
}

/// `fragment` inside the full page shell, for a plain browser navigation.
async fn full_page(
    state: &JournalRoutesState,
    request: DayRequest<'_>,
    fragment: &str,
    headers: &HeaderMap,
) -> Response {
    let DayRequest {
        client,
        vault_name,
        config,
        viewed,
    } = request;
    let entry = journal::entry_path(config, viewed);
    let web_config_path = Arc::clone(&state.web_config_path);
    let entity_base_path = VaultUrls::from(vault_name).journal_day(&journal::format_date(viewed));
    let known_mcp_servers = state.clients.names();
    let (mut nav, appearance, mcp_status, entry_exists) = tokio::join!(
        Box::pin(shell::build_nav(
            client,
            ActiveScreen::Vault,
            Some(vault_name),
            Some("journal"),
            None,
            Some(&entity_base_path),
            &known_mcp_servers,
        )),
        async move {
            tokio::task::spawn_blocking(move || config::current_appearance(&web_config_path))
                .await
                .unwrap_or_default()
        },
        shell::mcp_status(&state.clients),
        crate::file_view::page_file_exists(client, vault_name, &entry),
    );
    nav.appearance = appearance;
    nav.sidebar_wide = crate::routes::sidebar_wide_from_headers(headers);
    nav.mcp_status = mcp_status;
    // A day with no entry yet has no file to download or favourite, so it keeps the Journal's own title bar.
    if entry_exists {
        shell::show_file(
            &mut nav,
            client,
            vault_name,
            (&crate::view_favourites::JOURNAL).into(),
            (&entry, &entity_base_path),
        )
        .await;
    }
    html_response(page::render_page(&nav, "Journal", fragment)).into_response()
}

// ---------------------------------------------------------------------
// GET
// ---------------------------------------------------------------------

/// `GET /{{vault_name}}/journal` and `/{{vault_name}}/journal/`: redirects to today's date path.
pub async fn get_root(
    State(state): State<JournalRoutesState>,
    Path(vault_name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let target = VaultUrls::from(&*vault_name).journal_day(&journal::format_date(state.vault_time.today().await));
    redirect_response(&target, &headers)
}

/// `GET /{{vault_name}}/journal/{{date}}`: the whole day, its first section shown.
pub async fn get(
    State(state): State<JournalRoutesState>,
    Path((vault_name, date)): Path<(String, String)>,
    file: FileRequest,
    headers: HeaderMap,
) -> Response {
    let (client, viewed) = match open_day(&state, &vault_name, &date).await {
        Ok(opened) => opened,
        Err(response) => return *response,
    };
    let config = journal_config::load_journal_config(&client, &vault_name).await;
    if file.raw.0 {
        return file_response::stored(&client, &vault_name, &journal::entry_path(&config, viewed), file.raw).await;
    }
    let request = DayRequest {
        client: &client,
        vault_name: &vault_name,
        config: &config,
        viewed,
    };
    respond_with_day(&state, request, 0, None, &headers).await
}

/// The whole day as a fragment for HTMX, or inside the page shell for a browser navigation.
async fn respond_with_day(
    state: &JournalRoutesState,
    request: DayRequest<'_>,
    active: usize,
    edit: Option<usize>,
    headers: &HeaderMap,
) -> Response {
    let today = state.vault_time.today().await;
    let fragment = Box::pin(render_day_fragment(request, today, active, edit)).await;
    if headers.contains_key("hx-request") {
        return html_response(fragment);
    }
    full_page(state, request, &fragment, headers).await
}

/// The section a request names, with its day loaded, or the response that ends the request: `404` for an index that
/// is not a plain number or names no section, the unreachable page when the vault cannot be asked, and the whole day
/// with its reason when the entry cannot be prepared.
async fn open_section(
    state: &JournalRoutesState,
    request: DayRequest<'_>,
    raw_index: &str,
    headers: &HeaderMap,
) -> Result<(Day, usize), Box<Response>> {
    let index = section_index(raw_index).ok_or_else(|| Box::new(not_found()))?;
    let day = match journal::load_day(request.client, request.vault_name, request.config, request.viewed).await {
        Ok(day) => day,
        Err(LoadEntryError::Transport(_)) => return Err(Box::new(unreachable())),
        Err(_) => return Err(Box::new(respond_with_day(state, request, 0, None, headers).await)),
    };
    if index >= day.entry.sections().len() {
        return Err(Box::new(not_found()));
    }
    Ok((day, index))
}

/// `GET /{{vault_name}}/journal/{{date}}/sections/{{index}}`: the section's read view for HTMX (a cancelled edit), or
/// the whole day with that section shown for a browser navigation.
pub async fn get_section(
    State(state): State<JournalRoutesState>,
    Path((vault_name, date, raw_index)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let (client, viewed) = match open_day(&state, &vault_name, &date).await {
        Ok(opened) => opened,
        Err(response) => return *response,
    };
    let config = journal_config::load_journal_config(&client, &vault_name).await;
    let request = DayRequest {
        client: &client,
        vault_name: &vault_name,
        config: &config,
        viewed,
    };
    let (day, index) = match open_section(&state, request, &raw_index, &headers).await {
        Ok(opened) => opened,
        Err(response) => return *response,
    };
    if !headers.contains_key("hx-request") {
        return respond_with_day(&state, request, index, None, &headers).await;
    }
    html_response(render_pane(request, &day, index, false, None).await)
}

/// `GET /{{vault_name}}/journal/{{date}}/sections/{{index}}/edit`: the section's edit form for HTMX, or the whole day
/// with that form open for a browser navigation. A read-only section has no edit form.
pub async fn get_section_edit(
    State(state): State<JournalRoutesState>,
    Path((vault_name, date, raw_index)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let (client, viewed) = match open_day(&state, &vault_name, &date).await {
        Ok(opened) => opened,
        Err(response) => return *response,
    };
    let config = journal_config::load_journal_config(&client, &vault_name).await;
    let request = DayRequest {
        client: &client,
        vault_name: &vault_name,
        config: &config,
        viewed,
    };
    let (day, index) = match open_section(&state, request, &raw_index, &headers).await {
        Ok(opened) => opened,
        Err(response) => return *response,
    };
    let read_only = day
        .entry
        .sections()
        .get(index)
        .is_some_and(|section| config.is_read_only(&section.heading));
    if read_only {
        return not_found();
    }
    if !headers.contains_key("hx-request") {
        return respond_with_day(&state, request, index, Some(index), &headers).await;
    }
    html_response(render_pane(request, &day, index, true, None).await)
}

// ---------------------------------------------------------------------
// PATCH /{{vault_name}}/journal/{{date}}/sections/{{index}}
// ---------------------------------------------------------------------

/// What the edit form submits (as JSON, through `json-enc`). Anything else is malformed.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SectionSaveBody {
    /// The heading the form was opened on.
    heading: String,
    /// The version of the section's content the form was opened on.
    base: SectionFingerprint,
    content: String,
}

fn malformed_body() -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        [("content-type", "text/html; charset=utf-8")],
        "<p class=\"hint error\">Malformed request.</p>",
    )
        .into_response()
}

/// Why a save was refused, and whether to show the section's current content beside what was typed.
struct Refusal {
    error: String,
    show_current: bool,
}

/// The edit form again, with the refusal, what was typed, and, when the section changed, its current content.
async fn refused_form(
    request: DayRequest<'_>,
    index: usize,
    body: &SectionSaveBody,
    day: Option<&Day>,
    refusal: Refusal,
) -> Response {
    let DayRequest {
        client,
        vault_name,
        viewed,
        ..
    } = request;
    let Refusal { error, show_current } = refusal;
    let urls = VaultUrls::from(vault_name);
    let date = journal::format_date(viewed);
    let section = day.and_then(|day| day.entry.sections().get(index).map(|section| (day, section)));
    let guidance = section
        .and_then(|(_, section)| section.guidance.clone())
        .unwrap_or_default();
    let current = if show_current {
        section
            .and_then(|(day, _)| day.entry.content(index))
            .unwrap_or_default()
    } else {
        ""
    };
    let html = rendered(client, vault_name, &[&guidance, current]).await;
    let base = match section {
        Some((day, _)) if show_current => day.entry.fingerprint(index),
        _ => None,
    }
    .unwrap_or_else(|| body.base.clone());
    let label = match section {
        Some((day, _)) if show_current => section_label(&day.entry, index),
        _ => body.heading.clone(),
    };
    let fragment = PaneEditTemplate {
        index,
        label,
        guidance_html: html.first().cloned().unwrap_or_default(),
        heading: body.heading.clone(),
        base: base.as_str().to_owned(),
        content: body.content.clone(),
        section_url: urls.journal_section(&date, index),
        day_url: urls.journal_day(&date),
        error: Some(error),
        current_html: show_current.then(|| html.get(1).cloned().unwrap_or_default()),
    }
    .render()
    .unwrap_or_else(|_| "<p class=\"hint error\">This section could not be shown.</p>".to_owned());
    html_response(fragment)
}

/// `PATCH /{{vault_name}}/journal/{{date}}/sections/{{index}}`: saves the section, creating the day's entry from the
/// template when it has none yet. Answers with the section's read view, or the whole day when the saved content
/// added sections of its own, or the edit form again when the save was refused.
pub async fn patch_section(
    State(state): State<JournalRoutesState>,
    Path((vault_name, date, raw_index)): Path<(String, String, String)>,
    body: axum::body::Bytes,
) -> Response {
    let (client, viewed) = match open_day(&state, &vault_name, &date).await {
        Ok(opened) => opened,
        Err(response) => return *response,
    };
    let Some(index) = section_index(&raw_index) else {
        return not_found();
    };
    let Ok(body) = serde_json::from_slice::<SectionSaveBody>(&body) else {
        return malformed_body();
    };
    let config = journal_config::load_journal_config(&client, &vault_name).await;
    let request = DayRequest {
        client: &client,
        vault_name: &vault_name,
        config: &config,
        viewed,
    };
    if config.is_read_only(&body.heading) {
        let message = "This section is read-only here, so nothing was saved.".to_owned();
        return match journal::load_day(&client, &vault_name, &config, viewed).await {
            Ok(day) if index < day.entry.sections().len() => {
                html_response(render_pane(request, &day, index, false, Some(message)).await)
            }
            Err(LoadEntryError::Transport(_)) => unreachable(),
            _ => html_response(format!("<p class=\"hint error\">{message}</p>")),
        };
    }

    let edit = SectionEdit {
        index,
        heading: &body.heading,
        base: &body.base,
        content: &body.content,
    };
    let outcome = journal::save_section(&client, &vault_name, &config, viewed, &edit).await;
    let (day, error, show_current) = match outcome {
        Ok(SaveOutcome::Saved { day, sections_changed }) => {
            if sections_changed {
                let today = state.vault_time.today().await;
                let fragment = Box::pin(render_day_fragment(request, today, index, None)).await;
                return (
                    StatusCode::OK,
                    [("HX-Retarget", ".journal-screen"), ("HX-Reswap", "outerHTML")],
                    html_response(fragment),
                )
                    .into_response();
            }
            return html_response(render_pane(request, &day, index, false, None).await);
        }
        Ok(SaveOutcome::Refused {
            refused: SectionEditRefused::Changed,
            day,
        }) => (
            Some(day),
            "This section changed since you opened it, so your text was not saved. Its current content is shown \
             below yours: save again to replace it with yours, or cancel to keep it."
                .to_owned(),
            true,
        ),
        Ok(SaveOutcome::Refused {
            refused: SectionEditRefused::Moved,
            day,
        }) => (
            Some(day),
            "The day's sections changed since you opened this one (a section was added, removed, or renamed), so \
             your text was not saved. Copy it, then reload the day."
                .to_owned(),
            false,
        ),
        Ok(SaveOutcome::Refused {
            refused: SectionEditRefused::Unbalanced,
            day,
        }) => (
            Some(day),
            "Your text was not saved: it would change how the rest of the day's entry reads, such as a code block, \
             %%comment%%, or <!-- comment --> it opens but does not close. Close it and save again."
                .to_owned(),
            false,
        ),
        Err(WriteEntryError::Transport(_) | WriteEntryError::Load(LoadEntryError::Transport(_))) => {
            return unreachable();
        }
        Err(WriteEntryError::Contended) => (
            None,
            "The day's entry kept changing while it was being saved, so your text was not saved. Save again."
                .to_owned(),
            false,
        ),
        Err(error @ (WriteEntryError::Load(_) | WriteEntryError::Refused(_))) => {
            (None, format!("Your text was not saved: {error}"), false)
        }
    };
    let refusal = Refusal { error, show_current };
    Box::pin(refused_form(request, index, &body, day.as_ref(), refusal)).await
}
