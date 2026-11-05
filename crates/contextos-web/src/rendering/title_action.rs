//! The action controls a page shows in its title bar, beside the favourite star: the primary "make a new one" or
//! "bring something in" control of the destination (a new task, a new note, an upload to the Inbox, a rescan of the
//! app registry), and the download of the file a page shows. Each is rendered here, once, so the destination's own
//! content carries no second copy, and it targets the destination's own content by selector because the control sits
//! outside the swapped region.

use askama::Template;

/// One title-bar action, carrying whatever the control's own target needs.
pub enum TitleAction<'a> {
    /// A link to the new-task form of the named vault.
    NewTask(&'a str),
    /// A button that loads the new-note form of the named vault into the notes screen.
    NewNote(&'a str),
    /// A button that rescans the named vault's app registry.
    RescanApps(&'a str),
    /// A file picker that uploads to the given Inbox `POST` target and swaps the Inbox screen with the result.
    UploadToInbox(&'a str),
    /// A link that downloads the file named `name` from `href`, the page's own address asking for the file itself.
    Download { href: &'a str, name: &'a str },
}

#[derive(Template)]
#[template(path = "title_action.html")]
struct TitleActionTemplate<'a> {
    action: &'a TitleAction<'a>,
}

/// Renders `action` as the HTML fragment the title bar embeds.
#[must_use]
pub fn render(action: &TitleAction<'_>) -> String {
    TitleActionTemplate { action }
        .render()
        .unwrap_or_else(|_| "<span class=\"hint error\">An action could not be rendered.</span>".to_owned())
}

#[cfg(test)]
#[path = "title_action_test.rs"]
mod tests;
