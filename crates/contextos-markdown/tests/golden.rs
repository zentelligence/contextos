//! Golden fixtures for each layer of the dialect: standard Markdown, Obsidian Flavoured Markdown, and the fence
//! conventions, including a vault whose fence vocabulary differs from the built-in one. Each fixture renders twice
//! and must match its checked-in HTML exactly, so output is deterministic and any change to it is reviewed.
//!
//! Set `UPDATE_GOLDEN=1` to rewrite the HTML after an intended change, then review the diff.

use std::path::{Path, PathBuf};

use contextos_markdown::{
    BaseViewKey, Diagram, EmbedKey, FenceVocabulary, LinkRouter, Note, PrepareInput, PreparedNote, RenderInput,
    RenderedNote, Resources,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Routes;

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

fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative)
}

fn prepared(source: &str, vocabulary: &FenceVocabulary) -> Result<PreparedNote, Box<dyn std::error::Error>> {
    let note = Note::try_from(source)?;
    Ok(PreparedNote::from(PrepareInput {
        body: note.body(),
        vocabulary,
        template: false,
    }))
}

fn resources(vocabulary: &FenceVocabulary) -> Result<Resources, Box<dyn std::error::Error>> {
    let mut resources = Resources::default();
    resources.embeds.insert(
        EmbedKey {
            path: "wiki/Embedded.md".to_owned(),
            fragment: None,
        },
        Some(prepared(
            "## Embedded heading\n\nEmbedded text with a note.[^1]\n\n[^1]: Embedded footnote.\n",
            vocabulary,
        )?),
    );
    resources.bases.insert(
        BaseViewKey {
            path: "memory/tasks.base".to_owned(),
            view: Some("Open".to_owned()),
        },
        "<table class=\"base-view\"><tr><td>A task</td></tr></table>".to_owned(),
    );
    resources.diagrams.insert(
        "graph TD\nA-->B\n".to_owned(),
        Diagram::Svg("<svg class=\"diagram\"></svg>".to_owned()),
    );
    Ok(resources)
}

fn check(markdown: &str, golden: &str, vocabulary: &FenceVocabulary) -> TestResult {
    let source = std::fs::read_to_string(fixture(markdown))?;
    let resources = resources(vocabulary)?;
    let render = || -> Result<String, Box<dyn std::error::Error>> {
        let prepared = prepared(&source, vocabulary)?;
        Ok(RenderedNote::from(RenderInput {
            prepared: &prepared,
            resources: &resources,
            router: &Routes,
        })
        .html)
    };
    let first = render()?;
    assert_eq!(first, render()?, "{markdown} renders differently the second time");
    let golden_path = fixture(golden);
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(&golden_path, &first)?;
    }
    let expected = std::fs::read_to_string(&golden_path)?;
    assert_eq!(first, expected, "{markdown} no longer matches {golden}");
    Ok(())
}

#[test]
fn standard_markdown_matches_its_golden_html() -> TestResult {
    check(
        "standard/commonmark.md",
        "standard/commonmark.html",
        &FenceVocabulary::default(),
    )
}

#[test]
fn obsidian_flavoured_markdown_matches_its_golden_html() -> TestResult {
    check("obsidian/ofm.md", "obsidian/ofm.html", &FenceVocabulary::default())
}

#[test]
fn the_fence_conventions_match_their_golden_html() -> TestResult {
    check(
        "fences/conventions.md",
        "fences/conventions.html",
        &FenceVocabulary::default(),
    )
}

#[test]
fn a_vault_with_its_own_fence_vocabulary_renders_by_it() -> TestResult {
    let manifest = std::fs::read_to_string(fixture("fences/other-vocabulary.toml"))?;
    let vocabulary = FenceVocabulary::try_from(manifest.as_str())?;
    check(
        "fences/conventions.md",
        "fences/conventions-other-vocabulary.html",
        &vocabulary,
    )
}
