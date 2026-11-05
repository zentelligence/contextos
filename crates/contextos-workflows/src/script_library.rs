//! The built-in library: modules of the server's own functions, imported as `contextos:<module>`.
//!
//! They cover what scripts otherwise each write for themselves, in Rust and tested here, so a script imports one rather
//! than copying it. A built-in module only computes: none reads or writes anything, so the only effects a script has
//! are still its declared calls. Their behaviour is the server's, so it is not part of a job's version.

use std::fmt::Write as _;

use rhai::{Array, Dynamic, Module, Shared};

/// Every built-in module's import name, so validation can tell one from a misspelling.
pub(crate) const BUILT_IN_MODULES: [&str; 3] = ["contextos:path", "contextos:text", "contextos:toml"];

/// The built-in module imported as `name`, if there is one.
pub(crate) fn built_in_module(name: &str) -> Option<Shared<Module>> {
    let module = match name {
        "contextos:path" => path_module(),
        "contextos:text" => text_module(),
        "contextos:toml" => toml_module(),
        _ => return None,
    };
    Some(Shared::new(module))
}

/// `contextos:text`: string functions that return a new string rather than changing the one given, as Rhai's own do.
/// Positions count characters, as Rhai's `index_of` and `sub_string` do.
fn text_module() -> Module {
    let mut module = Module::new();
    module.set_native_fn("replaced", |text: &str, find: &str, with: &str| {
        Ok(replaced(text, find, with))
    });
    module.set_native_fn("last_index_of", |text: &str, find: &str| Ok(last_index_of(text, find)));
    module.set_native_fn("trimmed", |text: &str| Ok(text.trim().to_owned()));
    module.set_native_fn("trimmed_start", |text: &str| Ok(text.trim_start().to_owned()));
    module.set_native_fn("trimmed_end", |text: &str| Ok(text.trim_end().to_owned()));
    module.set_native_fn("is_blank", |text: &str| Ok(text.trim().is_empty()));
    module.set_native_fn("lines", |text: &str| Ok(lines(text)));
    module
}

/// `contextos:path`: `/`-separated vault and workflow paths, taken as text, never looked up.
fn path_module() -> Module {
    let mut module = Module::new();
    module.set_native_fn("parent", |path: &str| Ok(parent(path).to_owned()));
    module.set_native_fn("name", |path: &str| Ok(name(path).to_owned()));
    module.set_native_fn("stem", |path: &str| Ok(stem(path).to_owned()));
    module.set_native_fn("extension", |path: &str| Ok(extension(path).to_owned()));
    module.set_native_fn("join", |base: &str, relative: &str| Ok(join(base, relative)));
    module
}

/// `contextos:toml`: writing values into TOML a script builds as text.
fn toml_module() -> Module {
    let mut module = Module::new();
    module.set_native_fn("quoted", |value: &str| Ok(quoted(value)));
    module
}

/// `text` with every `find` replaced by `with`. An empty `find` changes nothing, rather than inserting `with` between
/// every character.
pub(crate) fn replaced(text: &str, find: &str, with: &str) -> String {
    if find.is_empty() {
        text.to_owned()
    } else {
        text.replace(find, with)
    }
}

/// The character position of the last `find` in `text`, or -1 when there is none.
pub(crate) fn last_index_of(text: &str, find: &str) -> i64 {
    text.rfind(find)
        .and_then(|byte| i64::try_from(text[..byte].chars().count()).ok())
        .unwrap_or(-1)
}

/// `text`'s lines, without their `\n` or `\r\n` endings. A final line ending does not start another line.
pub(crate) fn lines(text: &str) -> Array {
    text.lines().map(|line| Dynamic::from(line.to_owned())).collect()
}

/// Everything before the last `/`, or nothing when there is none.
pub(crate) fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

/// Everything after the last `/`.
pub(crate) fn name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// The name without its extension. A name that only begins with a dot, such as `.gitignore`, has no extension.
pub(crate) fn stem(path: &str) -> &str {
    let name = name(path);
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

/// The name's extension without its dot, or nothing when it has none.
pub(crate) fn extension(path: &str) -> &str {
    let name = name(path);
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[dot + 1..],
        _ => "",
    }
}

/// `relative` beneath `base`, with one `/` between them. An empty `base` gives `relative` alone.
pub(crate) fn join(base: &str, relative: &str) -> String {
    let base = base.trim_end_matches('/');
    let relative = relative.trim_start_matches('/');
    if base.is_empty() {
        relative.to_owned()
    } else {
        format!("{base}/{relative}")
    }
}

/// `value` as a TOML basic string, quoted, with every character TOML requires escaped.
pub(crate) fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            control if control.is_control() => {
                let _ = write!(out, "\\u{:04X}", u32::from(control));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
#[path = "script_library_test.rs"]
mod tests;
