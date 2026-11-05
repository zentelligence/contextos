//! Links, images, embeds, Mermaid diagrams, and author HTML: every place the page can point somewhere or show
//! something it did not write itself.

use std::fmt::Write as _;
use std::ops::Range;

use pulldown_cmark::{CowStr, Event, LinkType, Tag, TagEnd};

use super::{Diagram, Page};
use crate::escape::{escape_attribute, escape_text};
use crate::html_filter::{AuthorHtml, SafeHtml};
use crate::metadata::LinkKind;
use crate::prepare::{BaseViewKey, EmbedKey, Spanned, code_block_text, is_mermaid};
use crate::{FileKind, Fragment, LinkDestination, LinkTarget};

/// What a link to a note no screen shows says instead of a destination.
pub const UNROUTED_HINT: &str = "No screen shows this note yet.";

/// What a link whose name more than one file answers to says instead of a destination.
pub const AMBIGUOUS_HINT: &str = "More than one note answers to this name.";

/// A file's name without its folder or a `.md` extension, for a label.
fn stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, extension))
            if !stem.is_empty() && (extension.eq_ignore_ascii_case("md") || !stem.contains('.')) =>
        {
            stem
        }
        _ => name,
    }
}

/// What a link renders as: an anchor to a URL, or an unavailable span saying why.
enum Destination {
    Href { href: String, external: bool },
    Dead { target: String, reason: Dead },
}

#[derive(Clone, Copy)]
enum Dead {
    NotFound,
    Ambiguous,
    Unrouted,
}

impl Page<'_> {
    /// Where `target` goes when followed.
    fn destination(&self, target: &LinkTarget) -> Destination {
        match target {
            LinkTarget::Vault { path, fragment } => {
                let kind = FileKind::from(path.as_str());
                if kind.is_page() {
                    let anchor = fragment.as_ref().map(Fragment::anchor);
                    match self.router.page(path, anchor.as_deref()) {
                        Some(href) => Destination::Href { href, external: false },
                        None => Destination::Dead {
                            target: path.clone(),
                            reason: Dead::Unrouted,
                        },
                    }
                } else {
                    Destination::Href {
                        href: self.router.file(path),
                        external: false,
                    }
                }
            }
            LinkTarget::External(url) => Destination::Href {
                href: url.clone(),
                external: true,
            },
            LinkTarget::Anchor(fragment) => Destination::Href {
                href: format!("#{}", fragment.anchor()),
                external: false,
            },
            LinkTarget::NotFound(original) | LinkTarget::Unresolved(original) => Destination::Dead {
                target: original.clone(),
                reason: Dead::NotFound,
            },
            LinkTarget::Ambiguous(original) => Destination::Dead {
                target: original.clone(),
                reason: Dead::Ambiguous,
            },
        }
    }

    /// The opening and closing HTML of a link with class `class`.
    fn link_html(&self, target: &LinkTarget, class: &str, title: &str) -> (String, &'static str) {
        let title = if title.is_empty() {
            String::new()
        } else {
            format!(" title=\"{}\"", escape_attribute(title))
        };
        match self.destination(target) {
            Destination::Href { href, external: true } => (
                format!(
                    "<a class=\"external-link\" href=\"{}\" rel=\"noopener noreferrer\"{title}>",
                    escape_attribute(&href)
                ),
                "</a>",
            ),
            Destination::Href { href, external: false } => (
                format!("<a class=\"{class}\" href=\"{}\"{title}>", escape_attribute(&href)),
                "</a>",
            ),
            Destination::Dead { target, reason } => {
                let (extra, hint) = match reason {
                    Dead::NotFound => ("", None),
                    Dead::Ambiguous => (" ambiguous", Some(AMBIGUOUS_HINT)),
                    Dead::Unrouted => (" unrouted", Some(UNROUTED_HINT)),
                };
                let hint = hint
                    .map(|hint| format!(" title=\"{}\"", escape_attribute(hint)))
                    .unwrap_or_default();
                (
                    format!(
                        "<span class=\"{class} dead{extra}\" data-target=\"{}\"{hint}>",
                        escape_attribute(&target)
                    ),
                    "</span>",
                )
            }
        }
    }

    /// The text a wikilink written without a display shows.
    fn display_for(target: &LinkTarget) -> String {
        match target {
            LinkTarget::Vault { path, fragment } => {
                let mut display = stem(path).to_owned();
                match fragment {
                    Some(Fragment::Heading(heading)) => {
                        display.push('#');
                        display.push_str(heading);
                    }
                    Some(Fragment::Block(id)) => {
                        display.push_str("#^");
                        display.push_str(id);
                    }
                    None => {}
                }
                display
            }
            LinkTarget::External(text)
            | LinkTarget::NotFound(text)
            | LinkTarget::Ambiguous(text)
            | LinkTarget::Unresolved(text) => text.clone(),
            LinkTarget::Anchor(fragment) => match fragment {
                Fragment::Heading(heading) => heading.clone(),
                Fragment::Block(id) => format!("^{id}"),
            },
        }
    }

    /// Every link, image, embed, and Mermaid block in `events` rendered, and author HTML filtered.
    pub(super) fn linked(&mut self, events: &[Spanned], depth: usize) -> (Vec<Spanned>, Vec<Found>) {
        let mut walk = Walk {
            events,
            index: 0,
            out: Vec::with_capacity(events.len()),
            found: Vec::new(),
            closings: Vec::new(),
        };
        while let Some((event, range)) = events.get(walk.index) {
            walk.index = walk.index.saturating_add(1);
            match event {
                Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    title,
                    ..
                }) => self.link(&mut walk, *link_type, dest_url, title, range),
                Event::End(TagEnd::Link) => {
                    let close = walk.closings.pop().unwrap_or("</a>");
                    walk.out.push((Event::Html(CowStr::from(close)), range.clone()));
                }
                Event::Start(Tag::Image {
                    link_type,
                    dest_url,
                    title,
                    ..
                }) => self.image_or_embed(&mut walk, *link_type, dest_url, title, range, depth),
                Event::Start(Tag::CodeBlock(kind)) if is_mermaid(kind) => {
                    let (source, end) = code_block_text(events, walk.index.saturating_sub(1));
                    match self.resources.diagrams.get(&source) {
                        Some(diagram) => {
                            walk.out
                                .push((Event::Html(CowStr::from(diagram_html(diagram))), range.clone()));
                            walk.index = end.saturating_add(1);
                        }
                        None => walk.out.push((event.clone(), range.clone())),
                    }
                }
                Event::Start(Tag::HtmlBlock) => walk.html_block(range),
                Event::InlineHtml(html) => walk.inline_html(html, range),
                other => walk.out.push((other.clone(), range.clone())),
            }
        }
        (walk.out, walk.found)
    }

    fn link(&self, walk: &mut Walk<'_>, link_type: LinkType, dest_url: &str, title: &str, range: &Range<usize>) {
        let wiki = matches!(link_type, LinkType::WikiLink { .. });
        let target = LinkTarget::from(LinkDestination {
            text: dest_url,
            percent_encoded: !wiki,
        });
        let kind = if wiki { LinkKind::Wikilink } else { LinkKind::Link };
        walk.found.push((kind, target.clone(), range.start));
        let class = if wiki { "wikilink" } else { "internal-link" };
        let (open, close) = self.link_html(&target, class, title);
        walk.out.push((Event::Html(CowStr::from(open)), range.clone()));
        if matches!(link_type, LinkType::WikiLink { has_pothole: false }) {
            // The parser shows the destination itself; show the name the author wrote instead.
            walk.skip_to(|event| matches!(event, Event::End(TagEnd::Link)));
            walk.out
                .push((Event::Text(CowStr::from(Self::display_for(&target))), range.clone()));
            walk.out.push((Event::Html(CowStr::from(close)), range.clone()));
        } else {
            walk.closings.push(close);
        }
    }

    fn image_or_embed(
        &mut self,
        walk: &mut Walk<'_>,
        link_type: LinkType,
        dest_url: &str,
        title: &str,
        range: &Range<usize>,
        depth: usize,
    ) {
        let mut words = String::new();
        while let Some((inner, _)) = walk.events.get(walk.index) {
            walk.index = walk.index.saturating_add(1);
            match inner {
                Event::End(TagEnd::Image) => break,
                Event::Text(part) | Event::Code(part) => words.push_str(part),
                _ => {}
            }
        }
        let wiki = matches!(link_type, LinkType::WikiLink { .. });
        let target = LinkTarget::from(LinkDestination {
            text: dest_url,
            percent_encoded: !wiki,
        });
        let kind = if wiki { LinkKind::Embed } else { LinkKind::Image };
        walk.found.push((kind, target.clone(), range.start));
        let alone = matches!(walk.out.last(), Some((Event::Start(Tag::Paragraph), _)))
            && matches!(walk.events.get(walk.index), Some((Event::End(TagEnd::Paragraph), _)));
        let html = if wiki {
            let hint = if matches!(link_type, LinkType::WikiLink { has_pothole: true }) {
                words.as_str()
            } else {
                ""
            };
            self.embed(&target, hint, depth, alone)
        } else {
            Embedded::Inline(self.image(&target, &words, title))
        };
        match html {
            Embedded::Inline(html) => walk.out.push((Event::Html(CowStr::from(html)), range.clone())),
            Embedded::Block(html) => {
                // A block embed replaces the paragraph that held only it.
                walk.out.pop();
                walk.index = walk.index.saturating_add(1);
                walk.out.push((Event::Html(CowStr::from(html)), range.clone()));
            }
        }
    }

    /// A standard image: a vault image or a web image is shown; a vault file of another kind is a link.
    fn image(&self, target: &LinkTarget, alt: &str, title: &str) -> String {
        let title = if title.is_empty() {
            String::new()
        } else {
            format!(" title=\"{}\"", escape_attribute(title))
        };
        let source = match target {
            LinkTarget::Vault { path, .. } if FileKind::from(path.as_str()) == FileKind::Image => {
                Some(self.router.file(path))
            }
            LinkTarget::External(url) if url.to_ascii_lowercase().starts_with("http") || url.starts_with("//") => {
                Some(url.clone())
            }
            LinkTarget::Vault { .. } => {
                let (open, close) = self.link_html(target, "internal-link", "");
                return format!("{open}{}{close}", escape_text(alt));
            }
            _ => None,
        };
        match source {
            Some(source) => format!(
                "<img src=\"{}\" alt=\"{}\"{title}>",
                escape_attribute(&source),
                escape_attribute(alt)
            ),
            None => format!("<span class=\"embed dead\">{}</span>", escape_text(alt)),
        }
    }

    /// A wikilink embed, `![[target|hint]]`.
    fn embed(&mut self, target: &LinkTarget, hint: &str, depth: usize, alone: bool) -> Embedded {
        let LinkTarget::Vault { path, fragment } = target else {
            let (open, close) = self.link_html(target, "wikilink", "");
            return Embedded::Inline(format!("{open}{}{close}", escape_text(&Self::display_for(target))));
        };
        let name = stem(path);
        let file = || escape_attribute(&self.router.file(path));
        let as_link = |page: &Self| {
            let (open, close) = page.link_html(target, "wikilink", "");
            format!("{open}{}{close}", escape_text(name))
        };
        match FileKind::from(path.as_str()) {
            FileKind::Image => {
                let size = match hint.split_once('x') {
                    Some((width, height)) => size_attributes(width, Some(height)),
                    None => size_attributes(hint, None),
                };
                Embedded::Inline(format!(
                    "<img class=\"embed-image\" src=\"{}\" alt=\"{}\"{size}>",
                    file(),
                    escape_attribute(name)
                ))
            }
            FileKind::Audio => Embedded::Inline(format!(
                "<audio class=\"embed-audio\" controls src=\"{}\">{}</audio>",
                file(),
                escape_text(name)
            )),
            FileKind::Video => Embedded::Inline(format!(
                "<video class=\"embed-video\" controls src=\"{}\">{}</video>",
                file(),
                escape_text(name)
            )),
            FileKind::Pdf => {
                let page = match fragment {
                    Some(Fragment::Heading(heading)) => heading
                        .strip_prefix("page=")
                        .filter(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
                        .map(|number| format!("#page={number}"))
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                Embedded::Inline(format!(
                    "<iframe class=\"embed-pdf\" src=\"{}{page}\" title=\"{}\"></iframe>",
                    file(),
                    escape_attribute(name)
                ))
            }
            FileKind::Note if depth == 0 && alone => {
                let key = EmbedKey {
                    path: path.clone(),
                    fragment: fragment.clone(),
                };
                match self.resources.embeds.get(&key) {
                    Some(Some(prepared)) => {
                        let label = as_link(self);
                        let body = self.document(prepared, depth.saturating_add(1));
                        Embedded::Block(format!(
                            "<div class=\"embed-block\"><div class=\"embed-label\">{label}</div>{body}</div>\n"
                        ))
                    }
                    _ => Embedded::Inline(dead(path, name)),
                }
            }
            FileKind::Base if alone => {
                let view = match fragment {
                    Some(Fragment::Heading(view)) => Some(view.clone()),
                    _ => None,
                };
                let key = BaseViewKey {
                    path: path.clone(),
                    view,
                };
                match self.resources.bases.get(&key) {
                    Some(html) => {
                        // A base's fragment names a view, not a place on its page, so the label links to the base.
                        let base = LinkTarget::Vault {
                            path: path.clone(),
                            fragment: None,
                        };
                        let (open, close) = self.link_html(&base, "wikilink", "");
                        Embedded::Block(format!(
                            "<div class=\"embed-block\"><div class=\"embed-label\">{open}{}{close}</div>{html}</div>\n",
                            escape_text(name)
                        ))
                    }
                    None => Embedded::Inline(dead(path, name)),
                }
            }
            _ => Embedded::Inline(as_link(self)),
        }
    }
}

/// A link or embed found, with how it was written and the offset it was written at.
pub(super) type Found = (LinkKind, LinkTarget, usize);

/// The state of one pass over a segment's events.
pub(super) struct Walk<'a> {
    events: &'a [Spanned],
    /// The next event to read.
    index: usize,
    out: Vec<Spanned>,
    found: Vec<Found>,
    /// The closing HTML of each link still open.
    closings: Vec<&'static str>,
}

impl Walk<'_> {
    /// Moves past the first event from here that `is_end` accepts.
    fn skip_to(&mut self, is_end: impl Fn(&Event<'_>) -> bool) {
        while let Some((event, _)) = self.events.get(self.index) {
            self.index = self.index.saturating_add(1);
            if is_end(event) {
                break;
            }
        }
    }

    /// An HTML block, judged whole: the parser gives it a line at a time, and a tag can run over several lines.
    fn html_block(&mut self, range: &Range<usize>) {
        let mut html = String::new();
        while let Some((inner, _)) = self.events.get(self.index) {
            self.index = self.index.saturating_add(1);
            match inner {
                Event::End(TagEnd::HtmlBlock) => break,
                Event::Html(part) => html.push_str(part),
                _ => {}
            }
        }
        let safe = String::from(SafeHtml::from(AuthorHtml(&html)));
        self.out.push((Event::Html(CowStr::from(safe)), range.clone()));
    }

    /// An inline tag. One that opens an element removed with its content removes that content too, up to its end tag
    /// in this block.
    fn inline_html(&mut self, html: &str, range: &Range<usize>) {
        if let Some(removed) = crate::html_filter::removed_element(html) {
            while let Some((inner, _)) = self.events.get(self.index) {
                if crate::html_filter::closes(inner, &removed) {
                    self.index = self.index.saturating_add(1);
                    break;
                }
                if !matches!(
                    inner,
                    Event::Text(_) | Event::Code(_) | Event::InlineHtml(_) | Event::SoftBreak
                ) {
                    break;
                }
                self.index = self.index.saturating_add(1);
            }
            return;
        }
        let safe = String::from(SafeHtml::from(AuthorHtml(html)));
        self.out.push((Event::InlineHtml(CowStr::from(safe)), range.clone()));
    }
}

/// An embed that sits in its line of text, or one that takes the place of the paragraph it stood alone in.
enum Embedded {
    Inline(String),
    Block(String),
}

fn dead(path: &str, name: &str) -> String {
    format!(
        "<span class=\"wikilink dead\" data-target=\"{}\">{}</span>",
        escape_attribute(path),
        escape_text(name)
    )
}

/// `width` and `height` attributes from an image embed's size hint, when they are whole numbers.
fn size_attributes(width: &str, height: Option<&str>) -> String {
    let number = |text: &str| {
        let text = text.trim();
        (!text.is_empty() && text.len() <= 5 && text.chars().all(|c| c.is_ascii_digit())).then(|| text.to_owned())
    };
    let Some(width) = number(width) else {
        return String::new();
    };
    match height.and_then(number) {
        Some(height) => format!(" width=\"{width}\" height=\"{height}\""),
        None => format!(" width=\"{width}\""),
    }
}

fn diagram_html(diagram: &Diagram) -> String {
    match diagram {
        Diagram::Svg(svg) => format!("<div class=\"mermaid-diagram\">{svg}</div>\n"),
        Diagram::Failed(problems) => {
            let mut items = String::new();
            for problem in problems {
                let _ = write!(
                    items,
                    "<div class=\"diagnostic\" data-code=\"{code}\"><span class=\"diagnostic-code\">{code}</span><p class=\"diagnostic-message\">{}</p></div>",
                    escape_text(&problem.message),
                    code = escape_attribute(&problem.code),
                );
            }
            format!("<div class=\"diagnostic-panel\" role=\"alert\">{items}</div>\n")
        }
    }
}
