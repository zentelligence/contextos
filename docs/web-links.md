# Links in the web interface

`contextos-web` shows every note, and every Base, Canvas, and diagram file, inside the section that owns it, and sends every link to the vault through one route table (`vault_links`), whose URLs all come from one module (`urls::VaultUrls`): links in a rendered note body, the metadata panel, and the task link chips, and the rows of a base view, search results, and the search link panels (`screen_routes::ScreenRoutes`). The web never resolves a link's text itself. The note body and the metadata panel take each link as the server resolves it (`fs_attach_file` and `frontmatter_read` with `expand_links`, see [link expansion](link-expansion.md)); the task chips try the task rows already fetched first, as a fast path, and otherwise take the server's resolution of the same field. A link can therefore be stored in the form that is most reliable for Obsidian, and still be shown short and open the right screen. How a note is rendered is described in [rendering](rendering.md).

## Written forms

A link may be written in any form the server resolves (see [link expansion](link-expansion.md#resolution)): a vault path, with `.md` optional, a path from the linking note's folder or any ancestor of it, or the shortest unique file name, as Obsidian accepts. Inside a base, a record's links are written relative to the base: a bare key such as `a-028` for a keyed base, or a path within the base's folder for a base keyed by file path. A note whose `base` property names its base has a name that several notes answer to settled by that base's records folder. A name that more than one note still answers to is shown as ambiguous rather than guessed, and a name no file answers to as not found.

## Screens

A resolved note, Base, Canvas, or diagram opens on the screen that owns its folder, derived from the same configuration each screen already reads.

| Target | Route |
| --- | --- |
| A task note (Markdown under `memory/tasks/`, other than an `index.md`) with an `id` | `/{vault}/tasks/{id}` |
| The tasks base, `memory/tasks.base` | `/{vault}/tasks` |
| The Decisions register's base (`.contextos/vault-management.toml`) | `/{vault}/settings/decisions/` |
| Under the configured wiki folder | `/{vault}/wiki/{path within the folder}` |
| Directly inside one of the configured insights folder's one-character shards | `/{vault}/insights/{shard}/{file}` |
| Under the configured notes folder | `/{vault}/notes/{path within the folder}` |
| Under an entity's folder | `/{vault}/entity/{vault path}` |
| A note under the journal folder, named by the journal's date format | `/{vault}/journal/{date}` |
| A Decisions register record, directly in the register's folder (`.contextos/vault-management.toml`) | `/{vault}/settings/decisions/{stem}` |
| Under a registry type's folder (`.contextos/vault-management.toml`) | `/{vault}/settings/registry/{type}/{path within the folder}` |
| Under `standards/` or `templates/` | `/{vault}/settings/standards/{path}` or `/{vault}/settings/templates/{path}` |
| Under `memory/log/` or `memory/sessions/` | `/{vault}/settings/activity/log/{path}` or `/{vault}/settings/activity/sessions/{path}` |
| Any other file that is not a note, Base, Canvas, or diagram | `/{vault}/{vault path}` |
| A folder's managed `index.md`, which the interface never shows, any other note, a task note with no `id`, and a Base, Canvas, or diagram under the journal or tasks folder or outside every screen's folder | None |

The longest matching configured folder wins. A link with a heading or block fragment stays on its target's own screen, which renders the body through the same pipeline and so carries the anchor. A Base, Canvas, or diagram is shown read-only through the shared file view (see [rendering](rendering.md#base-canvas-and-diagram-files)), with none of a note's edit, rename, or metadata controls; a Vault Settings folder shows such a file in its own library view, as it shows any file there.

A file that is not a page (an image, a PDF) is served at its vault path, `/{vault}/{vault path}`, with its content type, by the generic route. A screen's route takes every address beneath it, so each screen also answers a file in its own folder at the screen's address, through one shared rule (`routes::file_response`): a file that is not a page is answered as itself; `?raw=1` answers with any file as it is stored, text as plain text and never rendered; and an htmx request naming a Base's `view` gets that view alone. A `raw` other than `1`, or `raw` or `view` given twice, is refused with `400` (`route/malformed-query`). A screen looks only in its own folder, so a top-level vault folder whose name is also an address the interface uses (`inbox`, `outbox`, `notes`, `wiki`, `insights`, `entity`, `tasks`, `journal`, `favourites`, and the rest) is reached by its vault path only when it is that screen's own folder, as the default `inbox/`, `outbox/`, and `notes/` are: the Inbox and Outbox answer a path that names a file in them with the file. Any other top-level folder by one of those names cannot be opened by its vault path.

A note, Base, Canvas, or diagram no screen owns has no route: a link to it is shown as unavailable ("No screen shows this note yet."), in a note body, the metadata panel, a base view's rows, and search results alike, rather than pointing at a page outside every section. The generic `/{vault}/{vault path}` route shows no note, Base, Canvas, diagram, or folder of its own. One a section owns, reached from a bookmark or an older link, redirects to that section (`303 See Other`, or `HX-Redirect` for an htmx request), keeping any `?entity=`: an entity's folder to its entity browser, a Vault Settings folder to its settings page, and a wiki, insights, notes, journal, or tasks folder to that screen. Anything else, a folder's `index.md` included, is answered as not found and logged as a warning with the page that linked to it, since every link the interface builds goes through the route table and one that reaches here was missed. `/{vault}/` renders the vault root's Overview dashboard, and a file's address answers an htmx request for a Base with that Base's view alone, which is how a Base's view tabs switch it in place on whichever screen shows it.

Files these routes serve are sent with `X-Content-Type-Options: nosniff`, and an HTML file or any XML document (SVG, RSS, Atom, XSL, and the rest) is also sent with a `Content-Security-Policy` that sandboxes it, so opening one shows it without letting it run script in the interface's origin. Apps (`registry/apps`) are served by their own route, which deliberately lets an app carry its own HTML and scripts.

## File pages

A page that shows one file (a wiki page, an insight, a note, a file in an entity's folder, a task, or a journal day with an entry) has a title bar naming its screen, with two controls for the file: a download link, the page's own address with `?raw=1`, and a star that favourites the file. A favourite file is listed under Favourite pages in the header's Favourites panel and opens on the screen that shows it, so a file someone keeps returning to is one click away; a screen is already one click away in the sidebar, and the star on a screen's own page still favourites the screen. Favourite files are kept by vault path in `.contextos/views.toml`, under `[pages]`, beside the favourite screens under `[views]`, and set by `PATCH /{vault}/favourites/{vault path}` with `{"favourite": true}` or `false`. Only a file that exists can be made a favourite; one that no longer exists can still be removed.

A folder's managed `index.md` is never shown: no screen, tree, base view, search result, or link panel lists one, and a link to one is not followed. The sidebar's Signals entry has no screen yet, so it is listed without a destination.

## Display

The metadata panel shows a link field whose every item is a wikilink as short chips: the link's own display text, else the last segment of its target. The full value stays editable under the chip list and is what is saved; a saved list is split between links only, never on a comma inside one. A dead chip has a dotted edge and text for a screen reader. The task screen resolves a link to a task by its key, its path within the tasks base, or its full vault path, against the task rows it has already fetched, and shows the task's id and title. A link that names no task row takes the server's resolution of the same field (the note's frontmatter read once with `expand_links`), so a link to a requirement or a record of another base opens that note, and is kept exactly as written when the field is saved; a link chip saved from the picker is written as the bare-key form. The task hierarchy resolves a `parent` link the same way.
