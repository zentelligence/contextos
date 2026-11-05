//! Rendering a prepared note to HTML.

mod callouts;
mod footnotes;
mod inline;
mod links;
mod math;

use std::collections::BTreeMap;
use std::fmt::Write as _;

use pulldown_cmark::{CowStr, Event};

pub use links::{AMBIGUOUS_HINT, UNROUTED_HINT};

use crate::escape::{escape_attribute, escape_text};
use crate::fence_attributes::AttributeValue;
use crate::fence_vocabulary::{FenceRole, FenceTone};
use crate::metadata::{BlockId, BodyMetadata, BodyTag, LinkRecord, OutlineHeading};
use crate::prepare::{BaseViewKey, EmbedKey, FenceHead, PreparedBlock, PreparedNote, Spanned};
use crate::router::LinkRouter;
use crate::slug::Anchors;

/// One problem a diagram's source has.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagramProblem {
    pub code: String,
    pub message: String,
}

/// A rendered diagram.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Diagram {
    /// The SVG the diagram renders to, from the server's renderer.
    Svg(String),
    /// Why it did not render.
    Failed(Vec<DiagramProblem>),
}

/// What a page's notes need, fetched by the caller.
#[derive(Clone, Debug, Default)]
pub struct Resources {
    /// Each embedded note, prepared, or `None` when it could not be read.
    pub embeds: BTreeMap<EmbedKey, Option<PreparedNote>>,
    /// Each diagram, by its source.
    pub diagrams: BTreeMap<String, Diagram>,
    /// Each embedded base view, as the HTML the caller rendered it to.
    pub bases: BTreeMap<BaseViewKey, String>,
}

/// A prepared note, what it needs, and the router that makes its URLs.
pub struct RenderInput<'a> {
    pub prepared: &'a PreparedNote,
    pub resources: &'a Resources,
    pub router: &'a dyn LinkRouter,
}

/// A rendered note.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderedNote {
    pub html: String,
    /// What the note's own body holds; embedded notes contribute nothing here.
    pub metadata: BodyMetadata,
}

impl From<RenderInput<'_>> for RenderedNote {
    fn from(input: RenderInput<'_>) -> Self {
        let mut page = Page {
            resources: input.resources,
            router: input.router,
            anchors: Anchors::default(),
            math: math::Math::default(),
            next_scope: 0,
            metadata: BodyMetadata::default(),
        };
        let html = page.document(input.prepared, 0);
        Self {
            html,
            metadata: page.metadata,
        }
    }
}

/// One page being rendered: its note and the notes embedded in it share its anchors, and only its own note adds to
/// its metadata.
pub(crate) struct Page<'a> {
    resources: &'a Resources,
    router: &'a dyn LinkRouter,
    anchors: Anchors,
    math: math::Math,
    /// The footnote scope the next note rendered takes.
    next_scope: usize,
    metadata: BodyMetadata,
}

fn flag_html(text: &str) -> String {
    format!("<div class=\"render-flag\" role=\"note\">{}</div>", escape_text(text))
}

impl Page<'_> {
    /// The HTML of one note, at `depth` embeds below the page.
    fn document(&mut self, prepared: &PreparedNote, depth: usize) -> String {
        let scope = self.next_scope;
        self.next_scope = self.next_scope.saturating_add(1);
        let mut events = Vec::new();
        self.blocks(&prepared.blocks, prepared, depth, &mut events);
        let events = footnotes::numbered(events, scope);
        let mut html = String::new();
        pulldown_cmark::html::push_html(&mut html, events.into_iter());
        html
    }

    fn blocks(
        &mut self,
        blocks: &[PreparedBlock],
        prepared: &PreparedNote,
        depth: usize,
        out: &mut Vec<Event<'static>>,
    ) {
        for block in blocks {
            match block {
                PreparedBlock::Markdown(events) => out.extend(self.segment(events.clone(), prepared, depth)),
                PreparedBlock::Fence { head, children } => {
                    let (open, close) = fence_html(head);
                    out.push(Event::Html(CowStr::from(open)));
                    self.blocks(children, prepared, depth, out);
                    out.push(Event::Html(CowStr::from(close)));
                }
                PreparedBlock::Flag(text) => out.push(Event::Html(CowStr::from(format!("{}\n", flag_html(text))))),
            }
        }
    }

    /// One Markdown segment through every pass, in the order each needs: block ids and heading anchors read the text
    /// as written, callouts read their marker before anything splits it, and links come last so every other pass
    /// has seen their text.
    fn segment(&mut self, events: Vec<Spanned>, prepared: &PreparedNote, depth: usize) -> Vec<Event<'static>> {
        let record = depth == 0;
        let events = inline::merged_text(events);
        let (events, block_ids) = inline::block_ids(events);
        let (events, headings) = inline::heading_anchors(&events, &mut self.anchors);
        let events = callouts::callouts(&events);
        let events = self.math.apply(events);
        let events = if prepared.template {
            template_tokens(events)
        } else {
            events
        };
        let events = inline::highlights(events);
        let (events, tags) = inline::tags(events);
        let (events, links) = self.linked(&events, depth);
        if record {
            let metadata = &mut self.metadata;
            metadata
                .outline
                .extend(headings.into_iter().map(|heading| OutlineHeading {
                    level: heading.level,
                    text: heading.text,
                    anchor: heading.anchor,
                    line: prepared.line_of(heading.offset),
                }));
            metadata.tags.extend(tags.into_iter().map(|(name, offset)| BodyTag {
                name,
                line: prepared.line_of(offset),
            }));
            metadata
                .block_ids
                .extend(block_ids.into_iter().map(|(id, offset)| BlockId {
                    id,
                    line: prepared.line_of(offset),
                }));
            metadata
                .links
                .extend(links.into_iter().map(|(kind, target, offset)| LinkRecord {
                    kind,
                    target,
                    line: prepared.line_of(offset),
                }));
        }
        events.into_iter().map(|(event, _)| event).collect()
    }
}

/// In a template, each code span holding a Templater token is shown as a token.
fn template_tokens(events: Vec<Spanned>) -> Vec<Spanned> {
    events
        .into_iter()
        .map(|(event, range)| match event {
            Event::Code(code) if crate::templates::is_token(&code) => (
                Event::Html(CowStr::from(format!(
                    "<code class=\"template-token\">{}</code>",
                    escape_text(&code)
                ))),
                range,
            ),
            other => (other, range),
        })
        .collect()
}

/// The opening and closing HTML of a fence.
fn fence_html(head: &FenceHead) -> (String, String) {
    let attribute_text = |key: &str| match head.attributes.get(key) {
        Some(AttributeValue::Text(text) | AttributeValue::Token(text)) => Some(text.clone()),
        _ => None,
    };
    let title = attribute_text("title");
    let mut flags: Vec<String> = head.problems.clone();
    flags.extend(head.attributes.problems.iter().cloned());

    let Some(spec) = &head.spec else {
        let name = escape_attribute(&head.name);
        let label = title.unwrap_or_else(|| head.name.clone());
        let mut open = format!(
            "<div class=\"fence fence-{name} fence-unrecognised\" data-fence=\"{name}\"><div class=\"fence-label\">{}</div>",
            escape_text(&label)
        );
        open.push_str(&flag_html(&format!(
            "This vault's fence vocabulary has no {} fence.",
            head.name
        )));
        for flag in &flags {
            open.push_str(&flag_html(flag));
        }
        open.push_str("<div class=\"fence-body\">");
        return (open, "</div></div>\n".to_owned());
    };

    let name = escape_attribute(&spec.name);
    if spec.role == FenceRole::Page {
        return (
            format!("<div class=\"fence fence-{name} fence-role-page\" aria-hidden=\"true\"></div>\n"),
            String::new(),
        );
    }
    let mut classes = vec![
        "fence".to_owned(),
        format!("fence-{name}"),
        "fence-recognised".to_owned(),
        format!("fence-role-{}", role_name(spec.role)),
    ];
    if let Some(tone) = spec.tone {
        classes.push(format!("fence-tone-{}", tone_name(tone)));
    }
    if let Some(variant) = attribute_text("variant") {
        classes.push(format!("fence-variant-{}", escape_attribute(&variant)));
    }
    if let Some(gap) = attribute_text("gap") {
        classes.push(format!("fence-gap-{}", escape_attribute(&gap)));
    }
    if attribute_text("display").as_deref() == Some("inline") {
        classes.push("fence-inline".to_owned());
    }
    let mut attributes = format!("class=\"{}\" data-fence=\"{name}\"", classes.join(" "));
    if let Some(level) = attribute_text("level") {
        let _ = write!(attributes, " data-level=\"{}\"", escape_attribute(&level));
    }
    if let Some(id) = attribute_text("id") {
        let _ = write!(attributes, " id=\"fence-{}\"", escape_attribute(&id));
    }
    if let Some(AttributeValue::Integer(columns)) = head.attributes.get("cols") {
        let _ = write!(attributes, " style=\"--fence-cols: {columns}\"");
    }
    let collapses = matches!(head.attributes.get("collapse"), Some(AttributeValue::Boolean(true)));
    let flags_html: String = flags.iter().map(|flag| flag_html(flag)).collect();
    if collapses {
        let open_now = matches!(head.attributes.get("open"), Some(AttributeValue::Boolean(true)));
        let label = title.unwrap_or_else(|| head.name.clone());
        return (
            format!(
                "<details {attributes}{}><summary class=\"fence-label\">{}</summary>{flags_html}<div class=\"fence-body\">",
                if open_now { " open" } else { "" },
                escape_text(&label)
            ),
            "</div></details>\n".to_owned(),
        );
    }
    let label = match spec.role {
        FenceRole::Layout => title,
        FenceRole::Callout | FenceRole::Status | FenceRole::Page => Some(title.unwrap_or_else(|| head.name.clone())),
    };
    let label_html = label
        .map(|label| format!("<div class=\"fence-label\">{}</div>", escape_text(&label)))
        .unwrap_or_default();
    (
        format!("<div {attributes}>{label_html}{flags_html}<div class=\"fence-body\">"),
        "</div></div>\n".to_owned(),
    )
}

const fn role_name(role: FenceRole) -> &'static str {
    match role {
        FenceRole::Callout => "callout",
        FenceRole::Layout => "layout",
        FenceRole::Status => "status",
        FenceRole::Page => "page",
    }
}

const fn tone_name(tone: FenceTone) -> &'static str {
    match tone {
        FenceTone::Neutral => "neutral",
        FenceTone::Positive => "positive",
        FenceTone::Caution => "caution",
        FenceTone::Critical => "critical",
        FenceTone::Emphasis => "emphasis",
        FenceTone::Analysis => "analysis",
    }
}

#[cfg(test)]
#[path = "support_test.rs"]
mod support_test;

#[cfg(test)]
#[path = "blocks_test.rs"]
mod blocks_test;

#[cfg(test)]
#[path = "links_test.rs"]
mod links_test;
