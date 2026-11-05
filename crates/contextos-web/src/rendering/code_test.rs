use super::*;

fn spans(html: &str, class: &str) -> Vec<String> {
    let open = format!("<span class=\"tok-{class}\">");
    html.split(&open)
        .skip(1)
        .filter_map(|rest| rest.split("</span>").next())
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_language_follows_the_file_extension_case_insensitively() {
    assert_eq!(Language::for_file_name("md-to-pdf.py"), Language::Python);
    assert_eq!(Language::for_file_name("deploy.SH"), Language::Shell);
    assert_eq!(Language::for_file_name("Setup.ps1"), Language::PowerShell);
    assert_eq!(Language::for_file_name("app.js"), Language::JavaScript);
    assert_eq!(Language::for_file_name("app.ts"), Language::JavaScript);
    assert_eq!(Language::for_file_name("lib.rs"), Language::Rust);
    assert_eq!(Language::for_file_name("manifest.toml"), Language::Toml);
    assert_eq!(Language::for_file_name("tasks.base"), Language::Yaml);
    assert_eq!(Language::for_file_name("board.canvas"), Language::Json);
    assert_eq!(Language::for_file_name("README"), Language::Plain);
}

#[test]
fn every_line_is_wrapped_so_line_numbers_line_up_with_the_source() {
    let html = highlight("a = 1\n\nb = 2\n", Language::Python);
    assert_eq!(html.matches("<span class=\"code-line\">").count(), 3);
}

#[test]
fn source_text_is_escaped_never_interpreted_as_markup() {
    let html = highlight("print(\"<script>alert(1)</script>\") # <b>", Language::Python);
    assert!(!html.contains("<script>"), "{html}");
    assert!(html.contains("&lt;script&gt;"), "{html}");
    assert!(html.contains("&lt;b&gt;"), "{html}");
}

#[test]
fn python_keywords_strings_comments_and_numbers_are_distinguished() {
    let html = highlight("def run(x):\n    return 'a#b' + 42  # done\n", Language::Python);
    assert_eq!(spans(&html, "keyword"), ["def", "return"]);
    assert_eq!(spans(&html, "string"), ["&#39;a#b&#39;"]);
    assert_eq!(spans(&html, "number"), ["42"]);
    assert_eq!(spans(&html, "comment"), ["# done"]);
}

#[test]
fn a_triple_quoted_string_spanning_lines_is_one_string_split_per_line() {
    let html = highlight("x = \"\"\"one\ntwo\"\"\"\n", Language::Python);
    assert_eq!(
        spans(&html, "string"),
        ["&quot;&quot;&quot;one", "two&quot;&quot;&quot;"]
    );
}

#[test]
fn an_escaped_quote_does_not_end_a_string() {
    let html = highlight("let s = \"a\\\"b\"; // c", Language::JavaScript);
    assert_eq!(spans(&html, "string"), ["&quot;a\\&quot;b&quot;"]);
    assert_eq!(spans(&html, "comment"), ["// c"]);
}

#[test]
fn shell_variables_and_powershell_keywords_ignore_case_where_the_language_does() {
    let shell = highlight("if [ -n \"$HOME\" ]; then echo $USER; fi", Language::Shell);
    assert_eq!(spans(&shell, "keyword"), ["if", "then", "fi"]);
    assert_eq!(spans(&shell, "variable"), ["$USER"]);
    let powershell = highlight("FUNCTION Get-It { Param($Path) }", Language::PowerShell);
    assert_eq!(spans(&powershell, "keyword"), ["FUNCTION", "Param"]);
    assert_eq!(spans(&powershell, "variable"), ["$Path"]);
}

#[test]
fn a_block_comment_runs_until_its_closing_delimiter() {
    let html = highlight("/* a\n b */ fn x() {}", Language::Rust);
    assert_eq!(spans(&html, "comment"), ["/* a", " b */"]);
    assert_eq!(spans(&html, "keyword"), ["fn"]);
}

#[test]
fn an_identifier_containing_a_keyword_is_not_a_keyword() {
    let html = highlight("define = format_if", Language::Python);
    assert!(spans(&html, "keyword").is_empty(), "{html}");
}

#[test]
fn plain_text_is_escaped_and_wrapped_but_never_tokenised() {
    let html = highlight("if 1 # x", Language::Plain);
    assert!(!html.contains("tok-"), "{html}");
    assert_eq!(html.matches("<span class=\"code-line\">").count(), 1);
}

#[test]
fn toml_keys_and_yaml_comments_are_marked() {
    let toml = highlight("[server]\nport = 8080 # local\n", Language::Toml);
    assert_eq!(spans(&toml, "number"), ["8080"]);
    assert_eq!(spans(&toml, "comment"), ["# local"]);
    let yaml = highlight("filters: file.ext == \"md\" # only notes\n", Language::Yaml);
    assert_eq!(spans(&yaml, "comment"), ["# only notes"]);
    assert_eq!(spans(&yaml, "string"), ["&quot;md&quot;"]);
}
