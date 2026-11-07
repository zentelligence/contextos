//! Visual-system gate: the offline-asset guarantee,
//! skin selection, the no-emoji rule, and an automated WCAG contrast
//! assertion over the token pairs actually used for text and thin
//! indicators. Real `contextos-mcp` sessions against real temporary
//! vault fixtures, no mocking, per `testing.md`.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

fn static_dir() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("static");
    path
}

/// Every stylesheet the entry point imports (other than the token layer, which carries no rules), in import order,
/// followed by the entry point itself: the cascade order a browser applies, so a check over shared and screen rules
/// sees every rule a page actually gets.
fn interface_stylesheet() -> Result<String, BoxError> {
    let entry = std::fs::read_to_string(static_dir().join("contextos-web.css"))?;
    assert!(entry.contains("@import url(\"contextos-shell.css\");"));
    let mut sheets = Vec::new();
    for line in entry.lines() {
        let Some(imported) = line
            .trim()
            .strip_prefix("@import url(\"")
            .and_then(|rest| rest.strip_suffix("\");"))
        else {
            continue;
        };
        if imported != "contextos-tokens.css" {
            sheets.push(std::fs::read_to_string(static_dir().join(imported))?);
        }
    }
    sheets.push(entry);
    Ok(sheets.join("\n"))
}

fn templates_dir() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("templates");
    path
}

/// Every class on an element that also carries a bare `hidden` attribute
/// (`hidden`, never `hidden="..."`, which this scan deliberately does not
/// match: a value there is an Askama expression, not the literal always-
/// hidden-until-JS-toggles-it marker this check cares about), across every
/// `.html` template: a minimal, single-tag scan (`class="..." ... hidden`
/// or `hidden ... class="..."` on one `<...>` span), not a real HTML
/// parser, but sufficient for this codebase's own hand-written templates.
fn classes_on_elements_carrying_a_bare_hidden_attribute() -> Vec<String> {
    let mut classes = Vec::new();
    let Ok(entries) = std::fs::read_dir(templates_dir()) else {
        return classes;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("html") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for tag in content.split('<').skip(1) {
            let Some(end) = tag.find('>') else { continue };
            let tag = &tag[..end];
            let carries_bare_hidden = tag
                .split_whitespace()
                .any(|token| token == "hidden" || token.trim_end_matches('>') == "hidden");
            if !carries_bare_hidden {
                continue;
            }
            if let Some(class_start) = tag.find("class=\"") {
                let after = &tag[class_start + "class=\"".len()..];
                if let Some(class_end) = after.find('"') {
                    for class in after[..class_end].split_whitespace() {
                        classes.push(format!(".{class}"));
                    }
                }
            }
        }
    }
    classes
}

/// `css` with every `/* ... */` comment removed: both rule-parsing checks
/// below split a rule's own selector list on `,`, and this stylesheet's
/// comments are prose, which routinely contains its own commas - without
/// stripping comments first, a comment sitting between two rules (or above
/// one) becomes part of the "selectors" text the next `{` closes off,
/// corrupting exactly the split these checks depend on. An unterminated
/// `/*` (should never happen in a real stylesheet) drops everything after
/// it rather than looping forever.
fn strip_css_comments(css: &str) -> String {
    let mut result = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        result.push_str(&rest[..start]);
        match rest[start..].find("*/") {
            Some(end) => rest = &rest[start + end + 2..],
            None => return result,
        }
    }
    result.push_str(rest);
    result
}

/// Whether `class` (a bare selector, e.g. `.foo`) has its own rule in
/// minified, comment-stripped `css` (`selector{declarations}`,
/// `}`-separated) that sets `display:` - the one property that can
/// actually defeat `[hidden]`'s own `display:none` default, regardless of
/// specificity. A class with no `display:` declaration at all never
/// conflicts with `[hidden]`, so is not flagged even without an explicit
/// `[hidden]` override.
fn class_sets_display_unconditionally(css: &str, class: &str) -> bool {
    css.split('}').any(|rule| {
        let Some(brace) = rule.find('{') else {
            return false;
        };
        let (selectors, declarations) = rule.split_at(brace);
        declarations[1..].contains("display:") && selectors.split(',').any(|selector| selector.trim() == class)
    })
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// `web.toml` with an optional `[server.ui]` block, plus the mandatory
/// first `contextos` stdio entry pointed at `config_path`.
fn write_web_toml(dir: &Path, command: &str, config_path: &Path, server_ui: &str) -> std::io::Result<PathBuf> {
    let path = dir.join("web.toml");
    #[allow(clippy::unnecessary_debug_formatting)]
    let config_value = format!("{config_path:?}");
    std::fs::write(
        &path,
        format!(
            "{server_ui}[[mcp_server]]\nname = \"contextos\"\ntransport = \"stdio\"\n\
             command = {command:?}\nargs = [\"--config\", {config_value}]\n"
        ),
    )?;
    Ok(path)
}

async fn router_with(vault_dir: &Path, dir: &Path, server_ui: &str) -> Result<Router, BoxError> {
    let config_path = support::write_vault_config(dir, vault_dir)?;
    let command = support::contextos_mcp_binary()?;
    let web_toml = write_web_toml(dir, &command.to_string_lossy(), &config_path, server_ui)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router(clients, Some(dir), &web_toml))
}

async fn get(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

fn seed(vault_dir: &Path) -> std::io::Result<()> {
    write(vault_dir, "index.md", "# Root\n")?;
    write(
        vault_dir,
        "note.md",
        "# Note\n\nA [[target]] and a [[missing-one]] link.\n",
    )?;
    write(vault_dir, "target.md", "# Target\n")?;
    write(
        vault_dir,
        "view.base",
        "views:\n  - type: table\n    name: \"All\"\n    order:\n      - file.path\n",
    )?;
    write(
        vault_dir,
        "diagram.canvas",
        "{\"nodes\":[{\"id\":\"n1\",\"type\":\"text\",\"x\":0,\"y\":0,\"width\":160,\"height\":60,\"text\":\"Node\",\"color\":\"4\"}],\"edges\":[]}\n",
    )?;
    Ok(())
}

const PAGE_ROUTES: &[&str] = &[
    "/contract-fixture/",
    "/contract-fixture/note.md",
    "/contract-fixture/view.base",
    "/contract-fixture/diagram.canvas",
    "/contract-fixture/apps/",
    "/server/settings/",
    "/contract-fixture/does-not-exist.md",
];

// ---------------------------------------------------------------------------
// No external origin: everything is served from /static/
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_rendered_page_references_an_external_origin() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    seed(vault_dir.path())?;
    let router = router_with(vault_dir.path(), dir.path(), "").await?;

    // An external origin only matters where the browser would fetch it: a
    // link, script, style, image, or CSS reference. XML namespace URIs
    // (`xmlns="http://www.w3.org/..."`) on inlined SVG are declarations, not
    // fetches, and are not counted.
    let fetch_prefixes = ["src=\"http", "url(http", "@import \"http", "@import url(http"];
    for route in PAGE_ROUTES {
        let (_status, body) = get(&router, route).await?;
        for prefix in fetch_prefixes {
            assert!(
                !body.contains(prefix),
                "route {route} fetches an external origin ({prefix}):\n{body}"
            );
        }
        // An `href` to another site is allowed only on an `<a>` link, which is navigation the reader chooses rather
        // than a fetch (the footer's about line links the publisher's site); on any other tag it would be fetched.
        for (at, _) in body.match_indices("href=\"http") {
            let tag = &body[body[..at].rfind('<').unwrap_or(0)..at];
            assert!(
                tag.starts_with("<a "),
                "route {route} fetches an external origin from {tag}:\n{body}"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn the_bundled_stylesheets_reference_only_static_assets() -> Result<(), BoxError> {
    for name in ["contextos-web.css", "contextos-shell.css", "contextos-tokens.css"] {
        let css = std::fs::read_to_string(static_dir().join(name))?;
        assert!(css.len() <= 50_000, "{name} exceeds the stylesheet size limit");
        assert!(
            !css.contains("http://") && !css.contains("https://"),
            "{name} references an external origin"
        );
        // Every url(...) target resolves to a file that ships in static/.
        for reference in css.split("url(").skip(1) {
            let raw = reference
                .split(')')
                .next()
                .unwrap_or_default()
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            if raw.is_empty() || raw.starts_with("data:") {
                continue;
            }
            let relative = raw.strip_prefix("/static/").unwrap_or(raw);
            assert!(
                static_dir().join(relative).exists(),
                "{name} references {raw}, which does not ship in static/"
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Skin selection
// ---------------------------------------------------------------------------

async fn html_at_root(server_ui: &str) -> Result<String, BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    seed(vault_dir.path())?;
    let router = router_with(vault_dir.path(), dir.path(), server_ui).await?;
    let (status, body) = get(&router, &format!("/{VAULT_NAME}/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(body)
}

#[tokio::test]
async fn an_unset_theme_stamps_no_data_theme_attribute() -> Result<(), BoxError> {
    let body = html_at_root("").await?;
    assert!(body.contains("<html lang=\"en\">"), "{body}");
    assert!(!body.contains("data-theme"));
    Ok(())
}

#[tokio::test]
async fn blueprint_dark_and_light_each_stamp_their_data_theme() -> Result<(), BoxError> {
    let blueprint = html_at_root("[server.ui]\ntheme = \"blueprint\"\n\n").await?;
    assert!(blueprint.contains("data-theme=\"blueprint\""));
    let dark = html_at_root("[server.ui]\ntheme = \"dark\"\n\n").await?;
    assert!(dark.contains("data-theme=\"dark\""));
    let light = html_at_root("[server.ui]\ntheme = \"light\"\n\n").await?;
    assert!(light.contains("data-theme=\"light\""));
    Ok(())
}

#[tokio::test]
async fn an_unrecognised_appearance_value_renders_the_page_not_a_500() -> Result<(), BoxError> {
    // Reject-never-repair applies to writes, not to a display preference
    // already persisted; the render falls back to the default.
    let body = html_at_root("[server.ui]\ntheme = \"solarized\"\nfont = \"comic\"\nsize = \"huge\"\n\n").await?;
    assert!(body.contains("data-theme=\"solarized\""));
    assert!(body.contains("<main class=\"content\">"));
    Ok(())
}

// ---------------------------------------------------------------------------
// No emoji / decorative Unicode anywhere in a rendered page
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_rendered_page_contains_emoji_or_decorative_unicode() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let dir = tempfile::tempdir()?;
    seed(vault_dir.path())?;
    let router = router_with(vault_dir.path(), dir.path(), "").await?;

    for route in PAGE_ROUTES {
        let (_status, body) = get(&router, route).await?;
        for ch in body.chars() {
            let cp = ch as u32;
            let emoji = (0x1F000..=0x1FAFF).contains(&cp) || (0x2600..=0x27BF).contains(&cp) || cp == 0xFE0F;
            let decorative = (0x2190..=0x21FF).contains(&cp) || (0x25A0..=0x25FF).contains(&cp);
            assert!(!emoji, "emoji U+{cp:04X} on route {route}");
            assert!(!decorative, "decorative Unicode U+{cp:04X} on route {route}");
        }
    }
    Ok(())
}

#[tokio::test]
async fn the_stylesheet_carries_a_visible_keyboard_focus_rule() -> Result<(), BoxError> {
    let css = interface_stylesheet()?;
    assert!(
        css.contains(":focus-visible"),
        "no :focus-visible rule in the stylesheet"
    );
    assert!(
        css.contains("--focus-ring"),
        "focus rule does not use the focus-ring token"
    );
    Ok(())
}

#[tokio::test]
async fn a_display_flex_class_never_defeats_its_own_hidden_attribute() -> Result<(), BoxError> {
    // Author CSS always wins over the user-agent stylesheet's own
    // `[hidden]{display:none}` rule, regardless of selector specificity, so
    // a class that sets `display:` unconditionally on an element also
    // toggled via `el.hidden` permanently defeats that toggle: the element
    // renders regardless of the attribute's value, and the button that
    // toggles it appears to do nothing since nothing actually depends on
    // the attribute. A since-removed "New task" form
    // (`id="task-create-form" hidden`, shown/hidden via `form.hidden =
    // !form.hidden`) hit exactly this: `.task-create-form` set
    // `display:flex` with no `[hidden]` override, and the class that
    // guard once named went stale the moment that form was removed rather
    // than catching the next such mistake, so this scans every template's
    // own current markup instead of one hardcoded class: every class that
    // sets its own `display:` and is applied to an element carrying a bare
    // `hidden` attribute, anywhere in `templates/`, must carry an explicit
    // `[hidden]` override in the stylesheet. A class applied alongside
    // `hidden` that declares no `display:` at all is not flagged: nothing
    // there can fight `[hidden]`'s own UA-default `display:none`.
    let css = strip_css_comments(&interface_stylesheet()?);
    let classes = classes_on_elements_carrying_a_bare_hidden_attribute();
    assert!(
        !classes.is_empty(),
        "expected at least one template to exercise this check"
    );
    for class in classes {
        if !class_sets_display_unconditionally(&css, &class) {
            continue;
        }
        assert!(
            css.contains(&format!("{class}[hidden]")),
            "{class} sets its own unconditional `display:` and is applied to an element toggled via \
             `hidden`, but has no `{class}[hidden]` override in the stylesheet to keep that `display:` \
             from defeating the toggle"
        );
    }
    Ok(())
}

/// Whether comment-stripped `css` gives any of `classes` its own rule that
/// sets `display:flex`, matching [`class_sets_display_unconditionally`]'s
/// identical rule-parsing shape but checking the declaration's own value
/// rather than just its presence.
fn any_class_sets_display_flex(css: &str, classes: &[&str]) -> bool {
    css.split('}').any(|rule| {
        let Some(brace) = rule.find('{') else {
            return false;
        };
        let (selectors, declarations) = rule.split_at(brace);
        declarations[1..].contains("display:flex")
            && selectors
                .split(',')
                .any(|selector| classes.iter().any(|class| selector.trim() == format!(".{class}")))
    })
}

#[tokio::test]
async fn every_body_textarea_sits_inside_a_flex_display_form() -> Result<(), BoxError> {
    // `.notes-body-textarea`'s own `flex:1` (the property that makes it
    // fill the available panel instead of a bare `<textarea>`'s tiny
    // intrinsic default size) does nothing at all unless its immediate
    // parent is itself a flex container. `.wiki-edit-form` (shared by
    // Wiki, Insights, and Profile's own edit views) and
    // `.entity-docs-edit-form` both shipped with no `display:flex` rule of
    // their own at all, so every one of those four screens' own edit views
    // rendered at that tiny default size from the day each shipped - only
    // Notes' own `.notes-body-form` ever carried the rule. This scans every
    // template's own current markup for the wrapping `<form>` around each
    // `.notes-body-textarea`, rather than hardcoding the two classes that
    // happened to be missing it this time, so a future edit-form macro that
    // forgets the same rule fails this check immediately instead of
    // shipping undersized.
    let css = strip_css_comments(&interface_stylesheet()?);
    let entries = std::fs::read_dir(templates_dir())?;
    let mut checked = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("html") {
            continue;
        }
        let content = std::fs::read_to_string(&path)?;
        let mut search_from = 0;
        while let Some(offset) = content[search_from..].find("notes-body-textarea") {
            let textarea_at = search_from + offset;
            let before = &content[..textarea_at];
            let form_start = before.rfind("<form").ok_or_else(|| {
                format!(
                    "{}: expected a wrapping <form> before a .notes-body-textarea",
                    path.display()
                )
            })?;
            let form_tag_end = content[form_start..]
                .find('>')
                .map(|i| form_start + i)
                .ok_or_else(|| format!("{}: unterminated <form> tag", path.display()))?;
            let form_tag = &content[form_start..form_tag_end];
            let class_start = form_tag
                .find("class=\"")
                .ok_or_else(|| format!("{}: expected the wrapping <form> to carry a class", path.display()))?;
            let after = &form_tag[class_start + "class=\"".len()..];
            let class_end = after
                .find('"')
                .ok_or_else(|| format!("{}: unterminated class attribute", path.display()))?;
            let form_classes: Vec<&str> = after[..class_end].split_whitespace().collect();
            assert!(
                any_class_sets_display_flex(&css, &form_classes),
                "{}: <form class=\"{}\"> wraps a .notes-body-textarea but none of its own classes set \
                 display:flex, so that textarea's own flex:1 does nothing and it renders at a bare \
                 <textarea>'s tiny default size",
                path.display(),
                form_classes.join(" ")
            );
            checked += 1;
            search_from = textarea_at + "notes-body-textarea".len();
        }
    }
    assert!(checked > 0, "expected at least one template to exercise this check");
    Ok(())
}

#[tokio::test]
async fn no_reading_width_cap_constrains_any_content_child() -> Result<(), BoxError> {
    // A fixed-column prose-reading cap (`.content > *{max-width:72ch}`) once
    // applied to every direct child of `<main class="content">`, which
    // meant every new non-prose screen (`.settings-grid`, `.apps-list`,
    // `.app-embed`, and later the Tasks dashboard, its vault-dashboard
    // widgets, and vault settings) had to remember to add its own class to
    // a growing exemption list or render squeezed into a narrow column -
    // the exact bug this test now guards against recurring a further time.
    // This project already treats hard-wrapping authored Markdown prose to
    // a fixed width as wrong (`AGENTS.md`: "Markdown prose is not
    // hard-wrapped"); rendered content gets the identical treatment; no
    // page, note, or screen is column-capped.
    let css = interface_stylesheet()?;
    assert!(
        !css.contains("max-width:72ch"),
        "a reading-width cap has returned to the stylesheet"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Automated WCAG contrast over the token pairs used for text
// ---------------------------------------------------------------------------

fn channel_luminance(c: f64) -> f64 {
    let c = c / 255.0;
    if c <= 0.039_28 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn relative_luminance(hex: &str) -> Result<f64, BoxError> {
    let value = u32::from_str_radix(hex.trim_start_matches('#'), 16)?;
    let r = f64::from((value >> 16) & 0xFF);
    let g = f64::from((value >> 8) & 0xFF);
    let b = f64::from(value & 0xFF);
    Ok(0.2126 * channel_luminance(r) + 0.7152 * channel_luminance(g) + 0.0722 * channel_luminance(b))
}

fn contrast(a: &str, b: &str) -> Result<f64, BoxError> {
    let (la, lb) = (relative_luminance(a)?, relative_luminance(b)?);
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    Ok((hi + 0.05) / (lo + 0.05))
}

/// The value of `--name` inside the CSS block that starts at `block_start`
/// (a `:root` selector), reading up to the block's closing brace. Only
/// plain `#rrggbb` values are returned; `rgba()` and `var()` tokens are
/// skipped (the contrast assertion only needs the solid text/indicator
/// colours).
fn token_hex(css: &str, block_start: &str, name: &str) -> Option<String> {
    let block = &css[css.find(block_start)?..];
    let block = &block[..block.find('}').unwrap_or(block.len())];
    let needle = format!("{name}:#");
    let start = block.find(&needle)? + needle.len() - 1;
    let hex: String = block[start..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() || *c == '#')
        .collect();
    (hex.len() == 7).then_some(hex)
}

#[test]
fn text_and_indicator_token_pairs_meet_wcag_aa_in_every_skin() -> Result<(), BoxError> {
    let css = std::fs::read_to_string(static_dir().join("contextos-tokens.css"))?;

    for (label, block) in [
        ("blueprint", ":root{"),
        ("dark", ":root[data-theme=\"dark\"]{"),
        ("light", ":root[data-theme=\"light\"]{"),
    ] {
        let hex = |name: &str| -> Result<String, BoxError> {
            token_hex(&css, block, name).ok_or_else(|| format!("{label}: missing token {name}").into())
        };
        let bg = hex("--bg")?;
        let panel = hex("--bg-panel")?;

        // Normal text: 4.5:1.
        for (fg_name, surface) in [
            ("--fg", &panel),
            ("--fg", &bg),
            ("--fg-muted", &panel),
            ("--fg-faint", &panel),
            ("--accent-primary", &panel),
        ] {
            let ratio = contrast(&hex(fg_name)?, surface)?;
            assert!(
                ratio >= 4.5,
                "{label}: {fg_name} on {surface} is {ratio:.2}:1, below the 4.5:1 AA floor"
            );
        }

        // Thin non-text indicators (2px accent bars, icon outlines): 3:1.
        for accent in [
            "--accent-analytics",
            "--accent-warning",
            "--accent-surface",
            "--accent-relational",
        ] {
            let ratio = contrast(&hex(accent)?, &panel)?;
            assert!(
                ratio >= 3.0,
                "{label}: {accent} as an indicator on the panel is {ratio:.2}:1, below 3:1"
            );
        }
    }
    Ok(())
}

#[test]
fn interface_typography_never_bypasses_the_root_size_preference() -> Result<(), BoxError> {
    let css = strip_css_comments(&interface_stylesheet()?);
    assert!(css.contains("font:var(--font-size-base)/1.5 var(--font-family)"));
    let mut checked = 0;
    for block in css.split('{').skip(1) {
        let declarations = block.split('}').next().unwrap_or_default();
        for declaration in declarations.split(';') {
            let Some((property, value)) = declaration.trim().split_once(':') else {
                continue;
            };
            if matches!(property.trim(), "font" | "font-size") {
                checked += 1;
                for unit in ["px", "pt", "pc", "cm", "mm", "in"] {
                    assert!(
                        !value
                            .as_bytes()
                            .windows(unit.len() + 1)
                            .any(|part| { part[0].is_ascii_digit() && &part[1..] == unit.as_bytes() }),
                        "absolute typography ignores the size preference: {declaration}"
                    );
                }
            }
        }
    }
    assert!(checked > 0, "expected interface typography declarations");
    Ok(())
}
