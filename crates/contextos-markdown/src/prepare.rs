//! Preparing a note: everything that needs no resource, done once, and a list of the resources rendering will need.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;

use pulldown_cmark::{BrokenLink, CodeBlockKind, CowStr, Event, LinkType, Options, Parser, Tag, TagEnd};

use crate::comments::{CommentFree, CommentInput};
use crate::fence_attributes::FenceAttributes;
use crate::fence_blocks::{Block, Fence, FenceInput, FenceTree};
use crate::fence_vocabulary::{FenceSpec, FenceVocabulary};
use crate::section::{Section, SectionInput};
use crate::templates::TokensAsCode;
use crate::{Body, FileKind, Fragment, LinkDestination, LinkTarget, Note};

/// An event and the bytes of the comment-free body it came from.
pub(crate) type Spanned = (Event<'static>, Range<usize>);

/// Fences nested deeper than this are shown as text: the convention asks for shallow nesting, and a bound keeps a
/// hostile note from nesting without limit.
const MAX_FENCE_DEPTH: usize = 8;

/// The Markdown this renderer reads: the `CommonMark` specification with tables, footnotes (counted even when defined in another fence),
/// strikethrough, task lists, smart punctuation, math, and Obsidian wikilinks. GitHub's alert blockquotes are left
/// off, because Obsidian callouts take any type and a title.
pub(crate) fn options() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_OLD_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_SMART_PUNCTUATION);
    options.insert(Options::ENABLE_MATH);
    options.insert(Options::ENABLE_WIKILINKS);
    options
}

/// A note embedded in a page, with the place in it the embed names.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EmbedKey {
    /// The note's vault-relative path.
    pub path: String,
    pub fragment: Option<Fragment>,
}

/// A base embedded in a page, with the view the embed names.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BaseViewKey {
    /// The base's vault-relative path.
    pub path: String,
    pub view: Option<String>,
}

/// What rendering a prepared note will look up.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Needs {
    /// Notes embedded in it.
    pub embeds: BTreeSet<EmbedKey>,
    /// The source of each Mermaid diagram in it.
    pub diagrams: BTreeSet<String>,
    /// Bases embedded in it.
    pub bases: BTreeSet<BaseViewKey>,
    /// Every vault path it links to or embeds, so a router can look up what it needs for them at once.
    pub linked: BTreeSet<String>,
    /// Whether any link or embed names something other than a web address or a place on the page: a body that does
    /// not needs no expansion by the server to render the same.
    pub refers_to_vault: bool,
}

/// A body to prepare and the fence vocabulary of its vault.
pub struct PrepareInput<'a> {
    pub body: &'a Body,
    pub vocabulary: &'a FenceVocabulary,
    /// The body is a template: its Templater tokens (`<% ... %>`, `{{ ... }}`) are shown as written.
    pub template: bool,
}

/// A note to embed, the place in it the embed names, and the fence vocabulary of its vault.
pub struct EmbedInput<'a> {
    pub note: &'a Note,
    pub fragment: Option<&'a Fragment>,
    pub vocabulary: &'a FenceVocabulary,
}

/// The head of a fence: everything but what it holds.
#[derive(Clone, Debug)]
pub(crate) struct FenceHead {
    pub(crate) name: String,
    pub(crate) spec: Option<FenceSpec>,
    pub(crate) attributes: FenceAttributes,
    pub(crate) problems: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum PreparedBlock {
    Markdown(Vec<Spanned>),
    Fence {
        head: FenceHead,
        children: Vec<PreparedBlock>,
    },
    /// A problem to show where it occurs.
    Flag(String),
}

/// A note ready to render once its resources are fetched.
#[derive(Clone, Debug)]
pub struct PreparedNote {
    pub(crate) blocks: Vec<PreparedBlock>,
    /// The line of the note each byte offset of the comment-free body starts, as a list of line starts.
    pub(crate) line_starts: Vec<usize>,
    pub(crate) first_line: usize,
    pub(crate) needs: Needs,
    /// Whether code spans holding a Templater token are shown as tokens.
    pub(crate) template: bool,
}

impl PreparedNote {
    /// What rendering this note will look up.
    #[must_use]
    pub const fn needs(&self) -> &Needs {
        &self.needs
    }

    /// The line of the note the byte at `offset` of the comment-free body is on, counting from 1.
    pub(crate) fn line_of(&self, offset: usize) -> usize {
        let index = self.line_starts.partition_point(|start| *start <= offset);
        self.first_line.saturating_add(index.saturating_sub(1))
    }
}

impl From<PrepareInput<'_>> for PreparedNote {
    fn from(input: PrepareInput<'_>) -> Self {
        if input.template {
            let text = String::from(TokensAsCode::from(input.body.text()));
            let mut prepared = prepared(&text, input.body.first_line(), input.vocabulary, Vec::new());
            prepared.template = true;
            return prepared;
        }
        prepared(input.body.text(), input.body.first_line(), input.vocabulary, Vec::new())
    }
}

impl From<EmbedInput<'_>> for PreparedNote {
    fn from(input: EmbedInput<'_>) -> Self {
        let body = input.note.body();
        let Some(fragment) = input.fragment else {
            return prepared(body.text(), body.first_line(), input.vocabulary, Vec::new());
        };
        match Section::try_from(SectionInput {
            text: body.text(),
            fragment,
        }) {
            Ok(section) => {
                let before = body.text().get(..section.0.start).unwrap_or_default();
                let first_line = body.first_line().saturating_add(before.matches('\n').count());
                let text = body.text().get(section.0).unwrap_or_default();
                prepared(text, first_line, input.vocabulary, Vec::new())
            }
            Err(missing) => prepared("", body.first_line(), input.vocabulary, vec![missing.to_string()]),
        }
    }
}

fn prepared(body: &str, first_line: usize, vocabulary: &FenceVocabulary, flags: Vec<String>) -> PreparedNote {
    let raw_code = code_ranges(body, true);
    let text = String::from(CommentFree::from(CommentInput {
        text: body,
        code: &raw_code,
    }));
    let code = code_ranges(&text, false);
    let references = references(&text);
    let tree = FenceTree::from(FenceInput {
        text: &text,
        code: &code,
        vocabulary,
    });
    let mut blocks: Vec<PreparedBlock> = flags.into_iter().map(PreparedBlock::Flag).collect();
    let converter = Converter {
        text: &text,
        references: &references,
    };
    blocks.extend(converter.blocks(tree.0, 1));
    let mut needs = Needs::default();
    collect_needs(&blocks, &mut needs);
    let mut line_starts = vec![0];
    line_starts.extend(text.match_indices('\n').map(|(at, _)| at.saturating_add(1)));
    PreparedNote {
        blocks,
        line_starts,
        first_line,
        needs,
        template: false,
    }
}

/// The byte ranges of code in `text`: code blocks, and with `inline` also code spans and math.
pub(crate) fn code_ranges(text: &str, inline: bool) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    for (event, range) in Parser::new_ext(text, options()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => ranges.push(range),
            Event::Code(_) | Event::InlineMath(_) | Event::DisplayMath(_) if inline => ranges.push(range),
            _ => {}
        }
    }
    ranges
}

/// A reference label as `CommonMark` matches it: case and runs of whitespace do not matter.
fn reference_key(label: &str) -> String {
    label.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Every link reference definition in `text`, so a reference in one fence finds a definition in another.
fn references(text: &str) -> HashMap<String, (String, String)> {
    let parser = Parser::new_ext(text, options());
    parser
        .reference_definitions()
        .iter()
        .map(|(label, definition)| {
            (
                reference_key(label),
                (
                    definition.dest.to_string(),
                    definition.title.as_ref().map(ToString::to_string).unwrap_or_default(),
                ),
            )
        })
        .collect()
}

struct Converter<'a> {
    text: &'a str,
    references: &'a HashMap<String, (String, String)>,
}

impl Converter<'_> {
    fn blocks(&self, blocks: Vec<Block>, depth: usize) -> Vec<PreparedBlock> {
        let mut prepared = Vec::with_capacity(blocks.len());
        for block in blocks {
            match block {
                Block::Markdown(range) => prepared.push(PreparedBlock::Markdown(self.segment(range))),
                Block::Fence(fence) if depth > MAX_FENCE_DEPTH => {
                    prepared.push(PreparedBlock::Flag(
                        "Fences nested deeper than eight are shown as text.".to_owned(),
                    ));
                    prepared.push(PreparedBlock::Markdown(self.segment(fence.range)));
                }
                Block::Fence(Fence {
                    name,
                    spec,
                    attributes,
                    problems,
                    children,
                    ..
                }) => prepared.push(PreparedBlock::Fence {
                    head: FenceHead {
                        name,
                        spec,
                        attributes,
                        problems,
                    },
                    children: self.blocks(children, depth.saturating_add(1)),
                }),
            }
        }
        prepared
    }

    /// The events of one segment, with their ranges in the whole body.
    fn segment(&self, range: Range<usize>) -> Vec<Spanned> {
        let Some(segment) = self.text.get(range.clone()) else {
            return Vec::new();
        };
        let references = self.references;
        let callback = |link: BrokenLink<'_>| {
            references
                .get(&reference_key(&link.reference))
                .map(|(dest, title)| (CowStr::from(dest.clone()), CowStr::from(title.clone())))
        };
        Parser::new_with_broken_link_callback(segment, options(), Some(callback))
            .into_offset_iter()
            .map(|(event, span)| {
                let span = span.start.saturating_add(range.start)..span.end.saturating_add(range.start);
                (without_table_escape(event).into_static(), span)
            })
            .collect()
    }
}

/// A wikilink inside a table cell writes its `|` as `\|`, and the parser leaves that backslash on the destination.
/// A vault path never ends with a backslash, so one there is always that escape.
fn without_table_escape(event: Event<'_>) -> Event<'_> {
    match event {
        Event::Start(Tag::Link {
            link_type: link_type @ LinkType::WikiLink { .. },
            dest_url,
            title,
            id,
        }) if dest_url.ends_with('\\') => Event::Start(Tag::Link {
            link_type,
            dest_url: CowStr::from(dest_url.trim_end_matches('\\').to_owned()),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type: link_type @ LinkType::WikiLink { .. },
            dest_url,
            title,
            id,
        }) if dest_url.ends_with('\\') => Event::Start(Tag::Image {
            link_type,
            dest_url: CowStr::from(dest_url.trim_end_matches('\\').to_owned()),
            title,
            id,
        }),
        other => other,
    }
}

/// Whether a code block's info string names Mermaid.
pub(crate) fn is_mermaid(kind: &CodeBlockKind<'_>) -> bool {
    matches!(kind, CodeBlockKind::Fenced(info)
        if info.split_whitespace().next().is_some_and(|language| language.eq_ignore_ascii_case("mermaid")))
}

/// The text of the code block whose start is at `events[start]`, and the index of its end.
pub(crate) fn code_block_text(events: &[Spanned], start: usize) -> (String, usize) {
    let mut text = String::new();
    let mut index = start.saturating_add(1);
    while let Some((event, _)) = events.get(index) {
        match event {
            Event::End(TagEnd::CodeBlock) => break,
            Event::Text(part) => text.push_str(part),
            _ => {}
        }
        index = index.saturating_add(1);
    }
    (text, index)
}

fn collect_needs(blocks: &[PreparedBlock], needs: &mut Needs) {
    for block in blocks {
        match block {
            PreparedBlock::Markdown(events) => {
                for (index, (event, _)) in events.iter().enumerate() {
                    if let Event::Start(
                        Tag::Link {
                            link_type, dest_url, ..
                        }
                        | Tag::Image {
                            link_type, dest_url, ..
                        },
                    ) = event
                    {
                        let target = LinkTarget::from(LinkDestination {
                            text: dest_url,
                            percent_encoded: !matches!(link_type, LinkType::WikiLink { .. }),
                        });
                        if !matches!(target, LinkTarget::External(_) | LinkTarget::Anchor(_)) {
                            needs.refers_to_vault = true;
                        }
                        if let LinkTarget::Vault { path, .. } = target {
                            needs.linked.insert(path);
                        }
                    }
                    match event {
                        Event::Start(Tag::Image {
                            link_type: LinkType::WikiLink { .. },
                            dest_url,
                            ..
                        }) => {
                            let target = LinkTarget::from(LinkDestination {
                                text: dest_url,
                                percent_encoded: false,
                            });
                            let LinkTarget::Vault { path, fragment } = target else {
                                continue;
                            };
                            match FileKind::from(path.as_str()) {
                                FileKind::Note => {
                                    needs.embeds.insert(EmbedKey { path, fragment });
                                }
                                FileKind::Base => {
                                    let view = match fragment {
                                        Some(Fragment::Heading(view)) => Some(view),
                                        _ => None,
                                    };
                                    needs.bases.insert(BaseViewKey { path, view });
                                }
                                _ => {}
                            }
                        }
                        Event::Start(Tag::CodeBlock(kind)) if is_mermaid(kind) => {
                            needs.diagrams.insert(code_block_text(events, index).0);
                        }
                        _ => {}
                    }
                }
            }
            PreparedBlock::Fence { children, .. } => collect_needs(children, needs),
            PreparedBlock::Flag(_) => {}
        }
    }
}
