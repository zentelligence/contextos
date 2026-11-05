use std::cell::Cell;
use std::collections::BTreeSet;
use std::convert::Infallible;

use super::*;

/// A vault of files that can be probed by exact path and looked up by name, as the real ones can. It has no way to
/// list the vault, so expansion cannot depend on one.
struct Vault {
    files: BTreeSet<String>,
    probes: Cell<usize>,
    searches: Cell<usize>,
}

impl Vault {
    fn of(paths: &[&str]) -> Self {
        Self {
            files: paths.iter().map(|path| (*path).to_owned()).collect(),
            probes: Cell::new(0),
            searches: Cell::new(0),
        }
    }
}

impl LinkTargets for Vault {
    type Error = Infallible;

    fn is_file(&self, path: &str) -> Result<bool, Self::Error> {
        self.probes.set(self.probes.get() + 1);
        Ok(self.files.contains(path))
    }

    fn files_ending_with(&self, suffix: &str) -> Result<Vec<String>, Self::Error> {
        self.searches.set(self.searches.get() + 1);
        let suffix = suffix.to_lowercase();
        let ending = format!("/{suffix}");
        Ok(self
            .files
            .iter()
            .filter(|path| {
                let path = path.to_lowercase();
                path == suffix || path.ends_with(&ending)
            })
            .cloned()
            .collect())
    }
}

fn expand(paths: &[&str], note: &str, source: &str) -> String {
    let vault = Vault::of(paths);
    expand_in(&vault, note, source)
}

fn expand_in(vault: &Vault, note: &str, source: &str) -> String {
    match ExpandedMarkdown::try_from(LinkExpansionInput {
        source,
        note,
        targets: vault,
        max_distinct_targets: 10_000,
        max_output_bytes: 100_000_000,
        declared_base: None,
    }) {
        Ok(expanded) => expanded.into(),
        Err(error) => format!("expansion failed: {error:?}"),
    }
}

const NOTE: &str = "wiki/a.md";

#[test]
fn a_link_in_an_indented_code_block_is_left_as_written_like_one_in_a_fenced_block() {
    assert_eq!(
        expand(&["wiki/Foo.md"], NOTE, "text\n\n    [[Foo]] in code\n\n[[Foo]]\n"),
        "text\n\n    [[Foo]] in code\n\n[[/wiki/Foo.md|Foo]]\n"
    );
}

#[test]
fn a_markdown_link_to_a_folder_is_rooted_at_the_index_note_inside_it() {
    assert_eq!(
        expand(&["wiki/raw/index.md"], NOTE, "[all](raw/)"),
        "[all](/wiki/raw/index.md)"
    );
}

#[test]
fn a_shortest_path_wikilink_becomes_a_rooted_path_that_keeps_its_original_text_as_display() {
    assert_eq!(
        expand(&["wiki/Foo.md"], NOTE, "See [[Foo]] now."),
        "See [[/wiki/Foo.md|Foo]] now."
    );
}

#[test]
fn a_wikilink_keeps_the_display_text_it_was_given() {
    assert_eq!(
        expand(&["wiki/Foo.md"], NOTE, "[[Foo|the foo]]"),
        "[[/wiki/Foo.md|the foo]]"
    );
}

#[test]
fn a_wikilink_heading_or_block_fragment_follows_the_rooted_path() {
    assert_eq!(
        expand(&["wiki/Foo.md"], NOTE, "[[Foo#Intro]] [[Foo#^b1]]"),
        "[[/wiki/Foo.md#Intro|Foo#Intro]] [[/wiki/Foo.md#^b1|Foo#^b1]]"
    );
}

#[test]
fn an_embed_gains_no_display_and_keeps_its_size_hint() {
    assert_eq!(
        expand(&["wiki/Foo.md", "wiki/pic.png"], NOTE, "![[Foo]] ![[pic.png|300]]"),
        "![[/wiki/Foo.md]] ![[/wiki/pic.png|300]]"
    );
}

#[test]
fn a_wikilink_naming_nothing_is_marked_not_found_with_its_original_text() {
    assert_eq!(
        expand(&["wiki/Foo.md"], NOTE, "[[typo]] [[/memory/typo]] [[typo#H]]"),
        "[[/:!typo|typo]] [[/:!/memory/typo|/memory/typo]] [[/:!typo#H|typo#H]]"
    );
}

#[test]
fn a_wikilink_naming_more_than_one_file_is_marked_ambiguous() {
    assert_eq!(expand(&["a/dup.md", "b/dup.md"], NOTE, "[[dup]]"), "[[/:?dup|dup]]");
}

#[test]
fn an_anchor_only_wikilink_is_left_as_written() {
    assert_eq!(
        expand(&[], NOTE, "[[#Heading]] [[#^block]]"),
        "[[#Heading]] [[#^block]]"
    );
}

#[test]
fn a_vault_path_written_from_the_root_wins_over_a_longer_path_ending_the_same_way() {
    assert_eq!(
        expand(&["wiki/Foo.md", "other/wiki/Foo.md"], NOTE, "[[wiki/Foo]]"),
        "[[/wiki/Foo.md|wiki/Foo]]"
    );
}

#[test]
fn relative_wikilinks_resolve_against_the_folder_of_the_linking_note() {
    assert_eq!(
        expand(
            &["wiki/Foo.md", "img/pic.png", "wiki/sub/Bar.md"],
            NOTE,
            "[[./Foo]] ![[../img/pic.png]] [[sub/Bar]]"
        ),
        "[[/wiki/Foo.md|./Foo]] ![[/img/pic.png]] [[/wiki/sub/Bar.md|sub/Bar]]"
    );
}

#[test]
fn a_relative_link_that_climbs_above_the_vault_root_is_not_found() {
    assert_eq!(expand(&["Foo.md"], "a.md", "[[../Foo]]"), "[[/:!../Foo|../Foo]]");
}

#[test]
fn a_name_with_a_dot_that_is_not_an_extension_resolves_to_the_note_with_md_appended() {
    assert_eq!(
        expand(&["wiki/v1.2 notes.md"], NOTE, "[[v1.2 notes]]"),
        "[[/wiki/v1.2 notes.md|v1.2 notes]]"
    );
}

#[test]
fn a_standard_link_with_a_vault_path_becomes_rooted_and_keeps_its_text_and_fragment() {
    assert_eq!(
        expand(&["wiki/Foo.md"], "a.md", "[the foo](wiki/Foo.md#Intro)"),
        "[the foo](/wiki/Foo.md#Intro)"
    );
}

#[test]
fn a_bare_standard_link_resolves_against_the_note_folder_before_the_vault_root() {
    assert_eq!(
        expand(&["Foo.md", "wiki/Foo.md"], NOTE, "[x](Foo.md)"),
        "[x](/wiki/Foo.md)"
    );
}

#[test]
fn a_bare_standard_link_falls_back_to_the_vault_root_then_a_unique_suffix() {
    assert_eq!(
        expand(&["Foo.md", "deep/dir/Bar.md"], NOTE, "[x](Foo.md) [y](Bar.md)"),
        "[x](/Foo.md) [y](/deep/dir/Bar.md)"
    );
}

#[test]
fn a_standard_image_keeps_its_alt_text_and_title() {
    assert_eq!(
        expand(&["wiki/img/pic.png"], NOTE, "![alt](img/pic.png \"a title\")"),
        "![alt](/wiki/img/pic.png \"a title\")"
    );
}

#[test]
fn a_standard_destination_keeps_the_text_it_was_written_with_and_only_gains_the_path_prefix() {
    assert_eq!(
        expand(
            &["wiki/My Note.md"],
            NOTE,
            "[x](My%20Note.md#A%20B) [y](<My Note.md>) [z](/wiki/My%20Note.md)"
        ),
        "[x](/wiki/My%20Note.md#A%20B) [y](</wiki/My Note.md>) [z](/wiki/My%20Note.md)"
    );
}

#[test]
fn a_folder_prefix_containing_spaces_is_percent_encoded_for_a_bare_destination_only() {
    assert_eq!(
        expand(&["my notes/Foo.md"], "my notes/a.md", "[x](Foo.md) [y](<Foo.md>)"),
        "[x](/my%20notes/Foo.md) [y](</my notes/Foo.md>)"
    );
}

#[test]
fn a_relative_standard_destination_is_replaced_by_the_rooted_path_it_names() {
    assert_eq!(
        expand(&["img/pic.png"], NOTE, "[x](../img/pic.png) [y](<../img/pic.png>)"),
        "[x](/img/pic.png) [y](</img/pic.png>)"
    );
}

#[test]
fn a_standard_link_naming_nothing_is_marked_with_its_original_destination() {
    assert_eq!(
        expand(&[], NOTE, "[x](nope.md) [y](no%20pe.md#H) [z](<no pe.md>)"),
        "[x](/:!nope.md) [y](/:!no%20pe.md#H) [z](</:!no pe.md>)"
    );
}

#[test]
fn a_standard_link_naming_more_than_one_file_is_marked_ambiguous() {
    assert_eq!(
        expand(&["a/dup.md", "b/dup.md"], "root.md", "[x](dup.md)"),
        "[x](/:?dup.md)"
    );
}

#[test]
fn external_mail_anchor_protocol_relative_and_vault_uri_destinations_are_left_as_written() {
    let source = "[a](https://example.com/x) [b](mailto:me@example.com) [c](#top) [d](//host/x) [e](vault://Foo.md)";
    assert_eq!(expand(&["Foo.md"], NOTE, source), source);
}

#[test]
fn links_inside_code_fences_inline_code_and_comments_are_left_alone() {
    let source = "```\n[[Foo]]\n```\n`[[Foo]]` and %%[[Foo]]%% and\n%%\n[[Foo]]\n%%\n~~~\n[x](Foo.md)\n~~~\n";
    assert_eq!(expand(&["wiki/Foo.md"], NOTE, source), source);
}

#[test]
fn links_after_code_and_comments_on_the_same_line_are_still_expanded() {
    assert_eq!(
        expand(&["wiki/Foo.md"], NOTE, "`x` %%c%% [[Foo]]"),
        "`x` %%c%% [[/wiki/Foo.md|Foo]]"
    );
}

#[test]
fn frontmatter_link_values_are_expanded_in_place() {
    let source = "---\nrelated:\n- '[[a-022|A-022]]'\nparent: \"[[a-023]]\"\n---\nBody\n";
    assert_eq!(
        expand(&["wiki/a-022.md", "wiki/a-023.md"], NOTE, source),
        "---\nrelated:\n- '[[/wiki/a-022.md|A-022]]'\nparent: \"[[/wiki/a-023.md|a-023]]\"\n---\nBody\n"
    );
}

#[test]
fn expansion_is_idempotent() {
    let source = "[[Foo]] [[Foo|x]] ![[pic.png|300]] [[typo#H]] [[dup]] [[/wiki/Foo.md]] [a](My%20Note.md#H) \
                  [b](nope.md) [c](Foo.md)\n---\n[[#h]] [[/memory/typo]]\n";
    let paths = ["wiki/Foo.md", "img/pic.png", "a/dup.md", "b/dup.md", "My Note.md"];
    let once = expand(&paths, NOTE, source);
    assert_ne!(once, source);
    assert_eq!(expand(&paths, NOTE, &once), once);
}

#[test]
fn multibyte_text_around_links_survives_unchanged() {
    assert_eq!(
        expand(&["wiki/Foo.md"], NOTE, "héllo ✓ [[Foo]] 日本語 [x](Foo.md)"),
        "héllo ✓ [[/wiki/Foo.md|Foo]] 日本語 [x](/wiki/Foo.md)"
    );
}

#[test]
fn unclosed_brackets_and_lookalike_text_are_left_as_written() {
    let source = "[[unclosed and [text] (not a link) and [a]( b and ]] stray";
    assert_eq!(expand(&["b"], NOTE, source), source);
}

#[test]
fn a_shortest_path_name_matches_regardless_of_case_and_still_reports_the_real_path() {
    assert_eq!(
        expand(&["tasks/a-029.md"], NOTE, "[[A-029]] [x](A-029.MD)"),
        "[[/tasks/a-029.md|A-029]] [x](/tasks/a-029.md)"
    );
}

#[test]
fn names_that_differ_only_by_case_are_ambiguous() {
    assert_eq!(expand(&["a/Foo.md", "b/foo.md"], NOTE, "[[FOO]]"), "[[/:?FOO|FOO]]");
}

#[test]
fn a_link_is_probed_in_the_note_folder_then_each_ancestor_and_the_nearest_hit_wins() {
    let out = expand(
        &["Foo.md", "a/Foo.md", "a/b/c/Bar.md", "a/b/Foo.md"],
        "a/b/c/note.md",
        "[[Foo]] [[Bar]]",
    );
    assert_eq!(out, "[[/a/b/Foo.md|Foo]] [[/a/b/c/Bar.md|Bar]]");
}

#[test]
fn a_target_outside_the_note_tree_is_found_by_searching_note_names() {
    assert_eq!(
        expand(
            &["elsewhere/deep/Foo.md"],
            "a/b/note.md",
            "[[Foo]] [[deep/Foo]] [x](Foo.md)"
        ),
        "[[/elsewhere/deep/Foo.md|Foo]] [[/elsewhere/deep/Foo.md|deep/Foo]] [x](/elsewhere/deep/Foo.md)"
    );
}

#[test]
fn a_rooted_target_is_exact_and_never_searched_for() {
    let vault = Vault::of(&["elsewhere/memory/typo.md"]);
    assert_eq!(
        expand_in(&vault, NOTE, "[[/memory/typo]]"),
        "[[/:!/memory/typo|/memory/typo]]"
    );
    assert_eq!(vault.searches.get(), 0);
}

#[test]
fn a_file_that_is_not_a_note_is_found_by_name_outside_the_tree_and_ambiguity_applies_to_it_too() {
    assert_eq!(
        expand(&["assets/pic.png"], "wiki/deep/a.md", "![[pic.png|200]] ![p](pic.png)"),
        "![[/assets/pic.png|200]] ![p](/assets/pic.png)"
    );
    assert_eq!(
        expand(&["a/pic.png", "b/pic.png"], NOTE, "![[pic.png]]"),
        "![[/:?pic.png]]"
    );
}

#[test]
fn a_name_without_an_extension_finds_its_note_before_a_file_of_the_same_name_with_none() {
    assert_eq!(expand(&["x/Foo", "y/Foo.md"], NOTE, "[[Foo]]"), "[[/y/Foo.md|Foo]]");
    assert_eq!(expand(&["x/Foo"], NOTE, "[[Foo]]"), "[[/x/Foo|Foo]]");
}

#[test]
fn repeating_a_link_probes_and_searches_once_and_never_enumerates_the_vault() {
    let vault = Vault::of(&["far/Foo.md"]);
    let source = "[[Foo]] ".repeat(500);
    let expanded = expand_in(&vault, "a/b/c/note.md", &source);
    assert_eq!(expanded, "[[/far/Foo.md|Foo]] ".repeat(500));
    // Folder, two ancestors and the root, each with and without `.md`: eight probes, and one lookup, since the note
    // name is tried first and found.
    assert_eq!(vault.probes.get(), 8);
    assert_eq!(vault.searches.get(), 1);
}

struct Failing;

#[derive(Debug, Eq, PartialEq)]
struct LookupFailed;

impl LinkTargets for Failing {
    type Error = LookupFailed;

    fn is_file(&self, _path: &str) -> Result<bool, Self::Error> {
        Ok(false)
    }

    fn files_ending_with(&self, _suffix: &str) -> Result<Vec<String>, Self::Error> {
        Err(LookupFailed)
    }
}

#[test]
fn a_failed_lookup_fails_the_expansion_instead_of_marking_the_link_not_found() {
    let result = ExpandedMarkdown::try_from(LinkExpansionInput {
        source: "[[Foo]]",
        note: NOTE,
        targets: &Failing,
        max_distinct_targets: 10,
        max_output_bytes: 1_000,
        declared_base: None,
    });
    assert_eq!(result, Err(ExpansionError::Lookup(LookupFailed)));
}

#[test]
fn an_escaped_pipe_in_a_table_row_separates_the_display_and_is_kept() {
    assert_eq!(
        expand(&["wiki/Foo.md"], NOTE, "| [[Foo\\|Alias]] | [[Foo]] | x |\n"),
        "| [[/wiki/Foo.md\\|Alias]] | [[/wiki/Foo.md\\|Foo]] | x |\n"
    );
}

#[test]
fn an_escaped_pipe_link_naming_nothing_is_marked_with_the_unescaped_target_and_stays_idempotent() {
    let once = expand(&[], NOTE, "| [[typo\\|Alias]] |\n");
    assert_eq!(once, "| [[/:!typo\\|Alias]] |\n");
    assert_eq!(expand(&[], NOTE, &once), once);
}

#[test]
fn a_fence_is_closed_only_by_its_own_character_at_least_as_long() {
    let vault = ["wiki/Foo.md"];
    let mixed = "```\n~~~\n[[Foo]]\n```\n[[Foo]]\n";
    assert_eq!(
        expand(&vault, NOTE, mixed),
        "```\n~~~\n[[Foo]]\n```\n[[/wiki/Foo.md|Foo]]\n"
    );
    let longer = "````\n```\n[[Foo]]\n````\n[[Foo]]\n";
    assert_eq!(
        expand(&vault, NOTE, longer),
        "````\n```\n[[Foo]]\n````\n[[/wiki/Foo.md|Foo]]\n"
    );
    let tilde = "~~~\n```\n[[Foo]]\n~~~\n[[Foo]]\n";
    assert_eq!(
        expand(&vault, NOTE, tilde),
        "~~~\n```\n[[Foo]]\n~~~\n[[/wiki/Foo.md|Foo]]\n"
    );
}

#[test]
fn a_fence_may_carry_an_info_string_and_an_inline_span_on_a_fence_like_line_is_not_a_fence() {
    let vault = ["wiki/Foo.md"];
    assert_eq!(
        expand(&vault, NOTE, "```rust title=\"x\"\n[[Foo]]\n```\n"),
        "```rust title=\"x\"\n[[Foo]]\n```\n"
    );
    assert_eq!(
        expand(&vault, NOTE, "```rust``` [[Foo]]\n[[Foo]]\n"),
        "```rust``` [[/wiki/Foo.md|Foo]]\n[[/wiki/Foo.md|Foo]]\n"
    );
}

#[test]
fn an_unclosed_fence_leaves_the_rest_of_the_document_alone() {
    assert_eq!(expand(&["wiki/Foo.md"], NOTE, "```\n[[Foo]]\n"), "```\n[[Foo]]\n");
}

#[test]
fn a_note_whose_path_a_wikilink_cannot_spell_is_marked_not_found() {
    assert_eq!(
        expand(
            &["we|rd/Foo.md", "a#b/Bar.md", "a[b]/Baz.md"],
            NOTE,
            "[[Foo]] [[Bar]] [[Baz]]"
        ),
        "[[/:!Foo|Foo]] [[/:!Bar|Bar]] [[/:!Baz|Baz]]"
    );
}

#[test]
fn a_standard_link_to_a_folder_containing_a_hash_encodes_it_in_a_bare_destination() {
    assert_eq!(
        expand(&["a#b/Foo.md"], NOTE, "[x](Foo.md) [y](<Foo.md>)"),
        "[x](/a%23b/Foo.md) [y](</a#b/Foo.md>)"
    );
}

fn expand_with_limit(source: &str, limit: usize) -> Result<String, ExpansionError<Infallible>> {
    let vault = Vault::of(&["wiki/a.md", "wiki/b.md", "wiki/c.md"]);
    ExpandedMarkdown::try_from(LinkExpansionInput {
        source,
        note: NOTE,
        targets: &vault,
        max_distinct_targets: limit,
        max_output_bytes: 100_000_000,
        declared_base: None,
    })
    .map(String::from)
}

#[test]
fn a_note_naming_more_distinct_targets_than_the_limit_fails_instead_of_being_partly_expanded() {
    assert_eq!(
        expand_with_limit("[[a]] [[b]] [[c]]", 2),
        Err(ExpansionError::TooManyTargets { maximum: 2 })
    );
}

#[test]
fn repeated_targets_and_links_left_as_written_do_not_count_towards_the_limit() {
    assert_eq!(
        expand_with_limit("[[a]] [[a]] [[b]] [[a]]", 2).as_deref(),
        Ok("[[/wiki/a.md|a]] [[/wiki/a.md|a]] [[/wiki/b.md|b]] [[/wiki/a.md|a]]")
    );
    let external = "[x](https://example.com) [[#top]] ".repeat(100);
    assert_eq!(expand_with_limit(&external, 1).as_deref(), Ok(external.as_str()));
}

#[test]
fn unclosed_link_openers_on_one_very_long_line_are_scanned_in_linear_time() {
    let vault = Vault::of(&[]);
    for unit in ["[[", "[", "[x](", "![", "[a](<", "[[a", "[x](y ", "[[a|"] {
        let source = unit.repeat(120_000 / unit.len());
        let started = std::time::Instant::now();
        let expanded = expand_in(&vault, NOTE, &source);
        let elapsed = started.elapsed();
        assert!(elapsed.as_secs() < 3, "{unit:?} took {elapsed:?}");
        assert_eq!(expanded, source, "{unit:?}");
    }
}

#[test]
fn backtick_runs_of_every_length_and_unclosed_titles_are_scanned_in_linear_time() {
    let vault = Vault::of(&[]);
    let runs = (1..1_200).fold(String::new(), |mut runs, length| {
        runs.push_str(&"`".repeat(length));
        runs.push(' ');
        runs
    });
    let titles = "[x](y (".repeat(15_000);
    let quoted = "[x](y \"".repeat(15_000);
    for (name, source) in [("runs", runs), ("titles", titles), ("quoted", quoted)] {
        let started = std::time::Instant::now();
        let expanded = expand_in(&vault, NOTE, &source);
        let elapsed = started.elapsed();
        assert!(elapsed.as_secs() < 3, "{name} took {elapsed:?}");
        assert_eq!(expanded, source, "{name}");
    }
}

#[test]
fn expansion_that_would_grow_beyond_the_output_limit_stops_at_the_limit() {
    let long_folder = "d".repeat(100);
    let target = format!("{long_folder}/a.md");
    let vault = Vault::of(&[target.as_str()]);
    let source = "[[a]] ".repeat(10_000);
    let result = ExpandedMarkdown::try_from(LinkExpansionInput {
        source: &source,
        note: NOTE,
        targets: &vault,
        max_distinct_targets: 10,
        max_output_bytes: 20_000,
        declared_base: None,
    });
    assert_eq!(result, Err(ExpansionError::OutputTooLarge { maximum: 20_000 }));
}

#[test]
fn output_exactly_at_the_limit_is_allowed() {
    let vault = Vault::of(&["wiki/a.md"]);
    let expanded = ExpandedMarkdown::try_from(LinkExpansionInput {
        source: "[[a]]",
        note: NOTE,
        targets: &vault,
        max_distinct_targets: 10,
        max_output_bytes: "[[/wiki/a.md|a]]".len(),
        declared_base: None,
    });
    assert_eq!(expanded.map(String::from).as_deref(), Ok("[[/wiki/a.md|a]]"));
}

/// Fragments of link syntax and of the text around it, in the combinations a hostile or careless note produces.
const SOUP: &[&str] = &[
    "[[",
    "]]",
    "[",
    "]",
    "(",
    ")",
    "![[",
    "![",
    "|",
    "\\|",
    "#",
    "^",
    "!",
    "`",
    "``",
    "~~~",
    "```",
    "%%",
    "\\",
    "\n",
    " ",
    "  ",
    "\"",
    "'",
    "<",
    ">",
    "Foo",
    "foo.md",
    "wiki/Foo.md",
    "../Foo",
    "./Foo",
    "/wiki/Foo.md",
    "/:!",
    "/:?",
    "typo",
    "dup",
    "pic.png",
    "a b",
    "a%20b",
    "é",
    "日本",
    "https://example.com",
    "mailto:x@y.z",
    "//host",
    "| ",
    " |",
];

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(400))]

    #[test]
    fn expansion_never_panics_and_expanding_twice_changes_nothing(
        parts in proptest::collection::vec(proptest::sample::select(SOUP), 0..60)
    ) {
        let vault = Vault::of(&["wiki/Foo.md", "Foo.md", "a b/Bar.md", "img/pic.png", "x/dup.md", "y/dup.md"]);
        let source = parts.concat();
        let once = expand_in(&vault, NOTE, &source);
        let twice = expand_in(&vault, NOTE, &once);
        proptest::prop_assert_eq!(&twice, &once, "source {:?}", source);
    }

    #[test]
    fn text_with_no_brackets_is_never_changed(
        parts in proptest::collection::vec(proptest::sample::select(SOUP), 0..60)
    ) {
        let vault = Vault::of(&["wiki/Foo.md"]);
        let source: String = parts.concat().replace(['[', ']'], "");
        proptest::prop_assert_eq!(expand_in(&vault, NOTE, &source), source);
    }
}

#[test]
fn a_reference_definition_destination_becomes_a_rooted_path() {
    assert_eq!(
        expand(&["wiki/Foo.md"], NOTE, "See [the foo][f].\n\n[f]: Foo.md \"Title\"\n"),
        "See [the foo][f].\n\n[f]: /wiki/Foo.md \"Title\"\n"
    );
}

#[test]
fn a_bracketed_reference_definition_keeps_its_brackets_and_fragment() {
    assert_eq!(
        expand(&["wiki/My Foo.md"], NOTE, "   [f]: <My Foo.md#Intro>\n"),
        "   [f]: </wiki/My Foo.md#Intro>\n"
    );
}

#[test]
fn a_reference_definition_naming_no_file_is_marked() {
    assert_eq!(expand(&[], NOTE, "[f]: nowhere.md\n"), "[f]: /:!nowhere.md\n");
}

#[test]
fn an_external_reference_definition_and_a_footnote_definition_are_left_as_written() {
    let source = "[f]: https://example.org/a\n[^1]: Foo.md is mentioned here.\n";
    assert_eq!(expand(&["wiki/Foo.md"], NOTE, source), source);
}

#[test]
fn a_reference_definition_indented_four_spaces_is_code_and_left_as_written() {
    let source = "    [f]: Foo.md\n";
    assert_eq!(expand(&["wiki/Foo.md"], NOTE, source), source);
}

#[test]
fn a_reference_definition_inside_a_code_fence_is_left_as_written() {
    let source = "```\n[f]: Foo.md\n```\n";
    assert_eq!(expand(&["wiki/Foo.md"], NOTE, source), source);
}

fn expand_with_base(paths: &[&str], note: &str, base: RecordsFolder<'_>, source: &str) -> String {
    let vault = Vault::of(paths);
    match ExpandedMarkdown::try_from(LinkExpansionInput {
        source,
        note,
        targets: &vault,
        max_distinct_targets: 10_000,
        max_output_bytes: 100_000_000,
        declared_base: Some(base),
    }) {
        Ok(expanded) => expanded.into(),
        Err(error) => format!("expansion failed: {error:?}"),
    }
}

#[test]
fn a_name_several_files_answer_to_resolves_to_the_only_one_in_the_declared_base() {
    let paths = ["tasks/web/a-1.md", "tasks/mcp/a-2.md", "archive/a-2.md"];
    let base = RecordsFolder {
        path: "tasks",
        includes_subfolders: true,
    };
    assert_eq!(
        expand_with_base(&paths, "tasks/web/a-1.md", base, "[[a-2]]"),
        "[[/tasks/mcp/a-2.md|a-2]]"
    );
}

#[test]
fn a_name_several_files_in_the_declared_base_answer_to_stays_ambiguous() {
    let paths = ["tasks/web/a-1.md", "tasks/mcp/a-2.md", "tasks/old/a-2.md"];
    let base = RecordsFolder {
        path: "tasks",
        includes_subfolders: true,
    };
    assert_eq!(
        expand_with_base(&paths, "tasks/web/a-1.md", base, "[[a-2]]"),
        "[[/:?a-2|a-2]]"
    );
}

#[test]
fn a_declared_base_holding_records_directly_ignores_a_match_in_a_subfolder() {
    let paths = ["tasks/a-1.md", "tasks/sub/a-2.md", "archive/a-2.md"];
    let base = RecordsFolder {
        path: "tasks",
        includes_subfolders: false,
    };
    assert_eq!(
        expand_with_base(&paths, "tasks/a-1.md", base, "[[a-2]]"),
        "[[/:?a-2|a-2]]"
    );
}

#[test]
fn a_wikilink_whose_target_looks_like_a_url_scheme_is_marked_not_found_and_not_left_alone() {
    assert_eq!(
        expand(&["a.md"], "a.md", "[[Re: meeting]] and [m](https://example.com)\n"),
        "[[/:!Re: meeting|Re: meeting]] and [m](https://example.com)\n"
    );
}
