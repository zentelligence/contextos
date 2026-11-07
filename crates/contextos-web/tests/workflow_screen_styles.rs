//! Every class the workflow, job, and library screens use has a rule in the stylesheets. An unstyled class renders as
//! plain text and still passes every behaviour test, which is how these screens first shipped without their filters
//! looking like anything at all.

use std::collections::BTreeSet;
use std::path::Path;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Whether `name` is one of these screens' templates.
fn is_workflow_screen(name: &str) -> bool {
    ["job_", "workflow", "library_"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || ["packages_list.html", "vault_packages.html"].contains(&name)
}

/// The class names `template` spells out in full. A template expression between them ends a name, and a name it completes,
/// such as `lifecycle-{{ state }}`, cannot be read here, so it is left out. Expressions go first, because a quote inside
/// one would otherwise end the attribute early and leave the classes after it unchecked.
fn classes_in(template: &str) -> BTreeSet<String> {
    let plain = without_expressions(template);
    let mut classes = BTreeSet::new();
    for attribute in plain.split("class=\"").skip(1) {
        classes.extend(
            attribute
                .split('"')
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .filter(|class| !class.ends_with('-'))
                .map(str::to_owned),
        );
    }
    classes
}

/// `text` with each `{% %}` and `{{ }}` replaced by a space, or by `- ` after a name it completes, which marks the name
/// for [`classes_in`] to leave out. An expression that never closes is a template that does not compile.
fn without_expressions(text: &str) -> String {
    let mut plain = String::new();
    let mut rest = text;
    while let Some(start) = [rest.find("{{"), rest.find("{%")].into_iter().flatten().min() {
        plain.push_str(&rest[..start]);
        let close = if rest[start..].starts_with("{{") { "}}" } else { "%}" };
        let Some(end) = rest[start..].find(close) else {
            return plain;
        };
        let completes_a_name = plain.ends_with(|character: char| character.is_ascii_alphanumeric() || character == '-');
        plain.push_str(if completes_a_name && rest[start..].starts_with("{{") {
            "- "
        } else {
            " "
        });
        rest = &rest[start + end + close.len()..];
    }
    plain.push_str(rest);
    plain
}

/// Every class a selector in `css` names.
fn classes_styled_by(css: &str) -> BTreeSet<String> {
    css.split('.')
        .skip(1)
        .map(|after_dot| {
            after_dot
                .chars()
                .take_while(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
                .collect::<String>()
        })
        .filter(|class| !class.is_empty())
        .collect()
}

fn folder_text(folder: &Path, wanted: impl Fn(&str) -> bool) -> Result<Vec<(String, String)>, BoxError> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if wanted(&name) {
            files.push((name, std::fs::read_to_string(&path)?));
        }
    }
    Ok(files)
}

#[test]
fn a_name_an_expression_completes_is_left_out_and_a_conditional_name_is_kept() {
    let template = r#"<a class="btn{% if on %} primary{% endif %} state-{{ state }}"></a>"#;

    let classes = classes_in(template);

    assert!(classes.contains("btn") && classes.contains("primary"), "{classes:?}");
    assert!(!classes.iter().any(|class| class.starts_with("state")), "{classes:?}");
}

#[test]
fn a_quote_inside_an_expression_does_not_hide_the_classes_after_it() {
    let template = r#"<a class="first{% if state == "on" %} second{% endif %} third"></a>"#;

    let classes = classes_in(template);

    for class in ["first", "second", "third"] {
        assert!(classes.contains(class), "{class}: {classes:?}");
    }
}

#[test]
fn every_class_on_the_workflow_screens_has_a_rule() -> Result<(), BoxError> {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let styled: BTreeSet<String> = folder_text(&crate_root.join("static"), |name| {
        Path::new(name).extension().is_some_and(|extension| extension == "css")
    })?
    .iter()
    .flat_map(|(_, css)| classes_styled_by(css))
    .collect();

    let templates = folder_text(&crate_root.join("templates"), is_workflow_screen)?;
    assert!(
        templates.len() > 10,
        "the screens' templates were not found: {}",
        templates.len()
    );

    let mut unstyled = Vec::new();
    for (name, template) in &templates {
        for class in classes_in(template).difference(&styled) {
            unstyled.push(format!("{name}: {class}"));
        }
    }
    assert!(unstyled.is_empty(), "these classes have no rule: {unstyled:#?}");
    Ok(())
}
