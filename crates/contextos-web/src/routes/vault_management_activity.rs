//! The Activity log in Vault Settings: the operation log and the session summaries, read only. Both are append-only
//! records of what happened, written as it happened, so this view offers no edit, new, or save affordance at all and
//! says so on the page.
//!
//! Beneath `/{vault}/settings/activity/`: `log` and `log/{path}` show the operation log, a day's entries as a table
//! (time, origin, operation, description, files) with the previous and next logged day either side; `sessions` and
//! `sessions/{path}` show session summaries rendered. A bare `activity/` opens the most recent day, or the most recent
//! session when the vault has no operation log. Navigation runs newest first, by year and month, over whatever the
//! folders actually hold: a file that does not follow the dated naming is still listed and shown.

use askama::Template;

use crate::activity_log::{self, LogLine, folder_label, session_label};
use crate::note_render::{self, NoteView};
use crate::routes::vault::{not_found, unreachable};
use crate::tree_view::{self, TreeEvent};
use crate::vault_catalogue::{ManagementCategory, OPERATION_LOG_DIR, SESSIONS_DIR, is_log_day};
use crate::vault_content::{AttachedRead, read_attached};
use crate::vault_relative_path::VaultRelativePath;
use crate::vault_tree::{FolderListing, VaultFile, VaultFolder};

use super::vault_management::{ManagementContext, Outcome, Panel};

/// One of the two activity records.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Log,
    Sessions,
}

impl Source {
    const fn slug(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Sessions => "sessions",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Log => "Operation log",
            Self::Sessions => "Session summaries",
        }
    }

    const fn root(self) -> &'static str {
        match self {
            Self::Log => OPERATION_LOG_DIR,
            Self::Sessions => SESSIONS_DIR,
        }
    }

    const fn empty_message(self) -> &'static str {
        match self {
            Self::Log => "No operations logged yet",
            Self::Sessions => "No session summaries yet",
        }
    }
}

pub(crate) struct SourceTab {
    label: &'static str,
    href: String,
    active: bool,
}

pub(crate) struct Leaf {
    label: String,
    detail: String,
    href: String,
    active: bool,
}

pub(crate) struct EntryRow {
    time: String,
    origin: String,
    operation: String,
    description: String,
    files: Vec<FileLink>,
}

pub(crate) struct FileLink {
    path: String,
    href: String,
}

pub(crate) enum DayLine {
    Entry(EntryRow),
    Other(String),
}

pub(crate) struct Neighbour {
    label: String,
    href: String,
}

pub(crate) struct DayView {
    title: String,
    path: String,
    entry_count: usize,
    file_count: usize,
    operations: Vec<(String, usize)>,
    lines: Vec<DayLine>,
    previous: Option<Neighbour>,
    next: Option<Neighbour>,
}

pub(crate) struct DocumentView {
    title: String,
    meta: String,
    path: String,
    html: String,
    previous: Option<Neighbour>,
    next: Option<Neighbour>,
}

pub(crate) enum MainView {
    Day(DayView),
    Document(DocumentView),
    Empty(&'static str),
}

#[derive(Template)]
#[template(path = "vm_activity.html")]
struct ActivityTemplate {
    icon: &'static str,
    tabs: Vec<SourceTab>,
    source_label: &'static str,
    tree_events: Vec<TreeEvent<Leaf>>,
    main: MainView,
}

fn listing<'a>(context: &ManagementContext<'a>, source: Source) -> &'a FolderListing {
    match source {
        Source::Log => &context.catalogue.operation_log,
        Source::Sessions => &context.catalogue.sessions,
    }
}

/// A copy of `folder` arranged for reading activity: newest first throughout, months named, and index files left out.
fn arranged(folder: &VaultFolder, parent: Option<&str>) -> VaultFolder {
    VaultFolder {
        name: folder_label(&folder.name, parent),
        relative_path: folder.relative_path.clone(),
        folders: folder
            .folders
            .iter()
            .rev()
            .map(|child| arranged(child, Some(&folder.name)))
            .collect(),
        files: folder
            .files
            .iter()
            .rev()
            .filter(|file| !file.is_index())
            .cloned()
            .collect(),
    }
}

/// Every readable file in `folder`, oldest first, so a day or session can name its neighbours.
fn chronological(folder: &VaultFolder) -> Vec<&VaultFile> {
    let mut files: Vec<&VaultFile> = folder
        .files_depth_first()
        .filter(|file| file.is_markdown() && !file.is_index())
        .collect();
    files.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    files
}

fn leaf_label(source: Source, file: &VaultFile) -> (String, String) {
    match source {
        Source::Log if is_log_day(file) => (file.stem().to_owned(), String::new()),
        Source::Log => (file.name.clone(), String::new()),
        Source::Sessions => {
            let label = session_label(&file.name);
            let when = [label.date, label.time]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            (label.title, when)
        }
    }
}

fn href(context: &ManagementContext<'_>, source: Source, relative: &str) -> String {
    let location = if relative.is_empty() {
        source.slug().to_owned()
    } else {
        format!("{}/{relative}", source.slug())
    };
    context.href(ManagementCategory::Activity, &location)
}

fn neighbours(
    context: &ManagementContext<'_>,
    source: Source,
    files: &[&VaultFile],
    selected: &str,
) -> (Option<Neighbour>, Option<Neighbour>) {
    let Some(index) = files.iter().position(|file| file.relative_path == selected) else {
        return (None, None);
    };
    let neighbour = |file: &&VaultFile| Neighbour {
        label: leaf_label(source, file).0,
        href: href(context, source, &file.relative_path),
    };
    (
        index
            .checked_sub(1)
            .and_then(|previous| files.get(previous))
            .map(neighbour),
        files.get(index + 1).map(neighbour),
    )
}

async fn day_view(
    context: &ManagementContext<'_>,
    file: &VaultFile,
    path: &VaultRelativePath,
    neighbours: (Option<Neighbour>, Option<Neighbour>),
) -> Result<MainView, Outcome> {
    let text = match read_attached(context.client, &format!("{}://{path}", context.vault_name)).await {
        Ok(AttachedRead::Text(attached)) => attached.text,
        Ok(_) => return Err(Outcome::from(not_found())),
        Err(_) => return Err(Outcome::from(unreachable())),
    };
    let day = activity_log::parse_day(&text);
    let urls = crate::urls::VaultUrls::from(context.vault_name);
    let vault_href = |file: &str| urls.file(file);
    let lines = day
        .lines
        .iter()
        .filter_map(|line| match line {
            LogLine::Entry(entry) => Some(DayLine::Entry(EntryRow {
                time: entry.time.clone(),
                origin: entry.origin.clone(),
                operation: entry.operation.clone(),
                description: entry.description.clone(),
                files: entry
                    .files
                    .iter()
                    .map(|file| FileLink {
                        path: file.clone(),
                        href: vault_href(file),
                    })
                    .collect(),
            })),
            LogLine::Other(text) => Some(DayLine::Other(text.clone())),
            LogLine::Heading(_) | LogLine::Blank => None,
        })
        .collect();
    let (previous, next) = neighbours;
    Ok(MainView::Day(DayView {
        title: file.stem().to_owned(),
        path: path.to_string(),
        entry_count: day.entries().count(),
        file_count: day.distinct_files(),
        operations: day.operation_counts(),
        lines,
        previous,
        next,
    }))
}

async fn document_view(
    context: &ManagementContext<'_>,
    source: Source,
    file: &VaultFile,
    path: &VaultRelativePath,
    neighbours: (Option<Neighbour>, Option<Neighbour>),
) -> Result<MainView, Outcome> {
    let text = match read_attached(context.client, &format!("{}://{path}", context.vault_name)).await {
        Ok(AttachedRead::Text(attached)) => attached.text,
        Ok(_) => return Err(Outcome::from(not_found())),
        Err(_) => return Err(Outcome::from(unreachable())),
    };
    let html = note_render::render_note(
        context.client,
        context.vault_name,
        NoteView {
            path: path.as_str(),
            raw: Some(&text),
            template: false,
        },
    )
    .await
    .html;
    let (title, meta) = leaf_label(source, file);
    let (previous, next) = neighbours;
    Ok(MainView::Document(DocumentView {
        title,
        meta,
        path: path.to_string(),
        html,
        previous,
        next,
    }))
}

fn tabs(context: &ManagementContext<'_>, active: Source) -> Vec<SourceTab> {
    [Source::Log, Source::Sessions]
        .into_iter()
        .filter(|source| !matches!(listing(context, *source), FolderListing::Missing))
        .map(|source| SourceTab {
            label: source.label(),
            href: href(context, source, ""),
            active: source == active,
        })
        .collect()
}

async fn show(context: &ManagementContext<'_>, source: Source, relative: &str) -> Outcome {
    let Some(folder) = listing(context, source).folder() else {
        return Outcome::from(not_found());
    };
    let files = chronological(folder);
    let selected = if relative.is_empty() {
        let latest = match source {
            Source::Log => files.iter().rev().find(|file| is_log_day(file)).or(files.last()),
            Source::Sessions => files.last(),
        };
        latest.map(|file| file.relative_path.clone()).unwrap_or_default()
    } else if folder.find_file(relative).is_some() {
        relative.to_owned()
    } else {
        return Outcome::from(not_found());
    };
    let main = match folder.find_file(&selected) {
        None => MainView::Empty(source.empty_message()),
        Some(file) => {
            let Ok(path) = VaultRelativePath::try_from(format!("{}/{selected}", source.root())) else {
                return Outcome::from(not_found());
            };
            let around = neighbours(context, source, &files, &selected);
            let shown = match source {
                Source::Log if is_log_day(file) => day_view(context, file, &path, around).await,
                Source::Log | Source::Sessions => document_view(context, source, file, &path, around).await,
            };
            match shown {
                Ok(main) => main,
                Err(outcome) => return outcome,
            }
        }
    };
    let arranged_folder = arranged(folder, None);
    let anchor = selected.rsplit_once('/').map_or("", |(parent, _)| parent);
    let tree_events = tree_view::flatten(&arranged_folder, anchor, &mut |file: &VaultFile| {
        let (label, detail) = leaf_label(source, file);
        Leaf {
            label,
            detail,
            href: href(context, source, &file.relative_path),
            active: file.relative_path == selected,
        }
    });
    let template = ActivityTemplate {
        icon: ManagementCategory::Activity.icon(),
        tabs: tabs(context, source),
        source_label: source.label(),
        tree_events,
        main,
    };
    Outcome::Show(Panel {
        title: format!("Activity log · {}", source.label()),
        html: template
            .render()
            .unwrap_or_else(|_| "<p class=\"hint error\">This page could not be rendered.</p>".to_owned()),
    })
}

/// Answers a `GET` beneath the Activity log category.
pub(super) async fn handle(context: &ManagementContext<'_>, rest: &str) -> Outcome {
    let rest = rest.trim_matches('/');
    let (first, beneath) = rest.split_once('/').unwrap_or((rest, ""));
    let source = match first {
        "log" => Source::Log,
        "" if listing(context, Source::Log).folder().is_some() => Source::Log,
        "sessions" | "" => Source::Sessions,
        _ => return Outcome::from(not_found()),
    };
    show(context, source, beneath).await
}

#[cfg(test)]
#[path = "vault_management_activity_test.rs"]
mod tests;
