//! Every URL the interface builds within a vault comes from one module, `urls.rs`, so a vault path, a screen's
//! folder, and the URL that shows it are mapped in one place: two places formatting the same kind of URL is how a link
//! ends up on a page outside every section. This test reads the sources and fails, naming each line, when code
//! outside that module formats a URL that starts with `/` and the vault's name, or a template builds one from it.

use std::path::{Path, PathBuf};

fn files(dir: &Path, extension: &str, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            files(&path, extension, found)?;
        } else if path.extension().is_some_and(|ext| ext == extension) {
            found.push(path);
        }
    }
    Ok(())
}

/// Whether `line` formats a vault URL: a string starting with `/` and the vault's name (`/{vault...}`, `/{name}`), or a
/// positional `"/{}/"` whose first argument is the name, which is how every `/{vault}/...` URL in this crate has been
/// written. A rooted vault path such as `format!("/{}", folder)`, which is not a URL, does not match.
fn formats_a_vault_url(line: &str) -> bool {
    line.contains("\"/{vault") || line.contains("\"/{name}") || line.contains("\"/{}/")
}

/// Lines outside the route module that format a vault URL.
fn code_offenders(root: &Path) -> std::io::Result<Vec<String>> {
    let mut sources = Vec::new();
    files(&root.join("src"), "rs", &mut sources)?;
    let mut offenders = Vec::new();
    for source in sources {
        let name = source.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        // `app.rs` and `app_routers.rs` register the route patterns these URLs are matched against: the router's half
        // of the same scheme, written in axum's pattern syntax.
        if name == "urls.rs" || name == "app.rs" || name == "app_routers.rs" || name.ends_with("_test.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&source)?;
        for (number, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if formats_a_vault_url(line) {
                offenders.push(format!("{}:{}: {}", source.display(), number + 1, trimmed));
            }
        }
    }
    Ok(offenders)
}

fn template_offenders(root: &Path) -> std::io::Result<Vec<String>> {
    let mut templates = Vec::new();
    files(&root.join("templates"), "html", &mut templates)?;
    let mut offenders = Vec::new();
    for template in templates {
        let text = std::fs::read_to_string(&template)?;
        for (number, line) in text.lines().enumerate() {
            let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
            if compact.contains("/{{vault_name}}") {
                offenders.push(format!("{}:{}: {}", template.display(), number + 1, line.trim()));
            }
        }
    }
    Ok(offenders)
}

#[test]
fn only_the_route_module_formats_a_vault_url() -> std::io::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = code_offenders(&root)?;
    offenders.extend(template_offenders(&root)?);
    assert!(
        offenders.is_empty(),
        "{} places format a vault URL outside src/urls.rs:\n{}",
        offenders.len(),
        offenders.join("\n")
    );
    Ok(())
}
