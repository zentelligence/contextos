//! Shared helpers for the renderer's tests: a router with predictable URLs and one-call rendering.

use crate::{FenceVocabulary, LinkRouter, Note, PrepareInput, PreparedNote, RenderInput, RenderedNote, Resources};

/// Pages under `unowned/` have no screen; every other page has `/v/page/{path}`, and every file `/v/file/{path}`.
pub(crate) struct Routes;

impl LinkRouter for Routes {
    fn page(&self, path: &str, anchor: Option<&str>) -> Option<String> {
        if path.starts_with("unowned/") {
            return None;
        }
        Some(match anchor {
            Some(anchor) => format!("/v/page/{path}#{anchor}"),
            None => format!("/v/page/{path}"),
        })
    }

    fn file(&self, path: &str) -> String {
        format!("/v/file/{path}")
    }
}

pub(crate) fn prepare(source: &str) -> PreparedNote {
    let note = Note::try_from(source).unwrap_or_default();
    PreparedNote::from(PrepareInput {
        body: note.body(),
        vocabulary: &FenceVocabulary::default(),
        template: false,
    })
}

pub(crate) fn render_template(source: &str) -> String {
    let note = Note::try_from(source).unwrap_or_default();
    let prepared = PreparedNote::from(PrepareInput {
        body: note.body(),
        vocabulary: &FenceVocabulary::default(),
        template: true,
    });
    RenderedNote::from(RenderInput {
        prepared: &prepared,
        resources: &Resources::default(),
        router: &Routes,
    })
    .html
}

pub(crate) fn render_with(source: &str, resources: &Resources) -> RenderedNote {
    let prepared = prepare(source);
    RenderedNote::from(RenderInput {
        prepared: &prepared,
        resources,
        router: &Routes,
    })
}

pub(crate) fn render(source: &str) -> String {
    render_with(source, &Resources::default()).html
}
