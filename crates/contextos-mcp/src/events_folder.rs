//! Where events live in a vault, and the one rule about who may write there.
//!
//! [`crate::server_owned_folders`] gathers this folder's rule with the other server-owned folders'.

use contextos_fs::ServerOwnedFolder;

/// The vault-relative folder every event note lives under.
pub(crate) const EVENTS_FOLDER: &str = "memory/events";

/// The one base that is an events base: the vault's `memory/events.base`. A record created in it is an event, and its
/// sidecar names the kind registry. `event_kinds` on any other base's sidecar has no effect.
pub(crate) const EVENTS_BASE: &str = "memory/events.base";

/// The note in the events folder that is ordinary content: the static index the scaffold ships.
pub(crate) const EVENTS_INDEX: &str = "memory/events/index.md";

/// The stable code of a tool call refused for writing in the events folder.
pub(crate) const SERVER_OWNED_CODE: &str = "events/server-owned";

/// Whether `name` is a Markdown note's file name, whatever the case of its extension.
pub(crate) fn is_markdown(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

/// The events folder as a server-owned folder: everything below it except its index note.
pub(crate) fn server_owned_folder() -> ServerOwnedFolder {
    ServerOwnedFolder {
        folder: EVENTS_FOLDER.to_owned(),
        open: vec![EVENTS_INDEX.to_owned()],
        code: SERVER_OWNED_CODE,
        remediation: "Only the server writes there. Record an event with base_record_create against memory/events.base, \
                      or leave the folder to the server.",
    }
}
