//! `web.toml` editing for `/server/settings/`: structure- and comment-preserving
//! edits via `toml_edit`, mirroring `contextos-mcp`'s own `contextos
//! config` validate-then-write discipline directly as its precedent.
//!
//! Every mutating method re-parses its own rendered output through
//! [`WebConfig`]'s real schema validator ([`WebConfig::validate`], the same
//! check [`crate::config::load_web_config`] runs on startup) before
//! returning success, rolling back to the pre-mutation document on any
//! failure. A caller never observes, and this module never persists, a
//! document that would fail [`crate::config::load_web_config`] to re-load.
//!
//! This module knows nothing about HTTP or about registered apps;
//! [`crate::routes::settings`] is the thin adapter translating request
//! bodies to these methods, running the registered-app-dependency check
//! these methods cannot themselves perform (they have no MCP session), and
//! translating results back to HTTP responses.

use serde_json::{Map as JsonMap, Value as JsonValue};
use thiserror::Error;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value as TomlValue};

use crate::config::{McpServerConfig, WebConfig, WebConfigError};

/// A snapshot of `[server]`'s scalar values, for `contextos-web config` to
/// offer as editable defaults on a re-run. Integer and boolean keys keep
/// TOML's own `i64`/`bool` types.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WebServerSettingsView {
    pub bind: Option<String>,
    pub static_dir: Option<String>,
    pub log_level: Option<String>,
    pub log_file: Option<String>,
    pub log_max_size_mb: Option<i64>,
    pub log_rotate_daily: Option<bool>,
    pub log_retention_days: Option<i64>,
    pub log_retention_files: Option<i64>,
}

/// The `[server]` scalar values to write. Every optional field left `None`
/// removes its key rather than leaving a stale value behind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebServerSettingsInput {
    pub bind: String,
    pub static_dir: Option<String>,
    pub log_level: String,
    pub log_file: String,
    pub log_max_size_mb: Option<u64>,
    pub log_rotate_daily: bool,
    pub log_retention_days: Option<u32>,
    pub log_retention_files: Option<u32>,
}

/// A `web.toml` document under edit, preserving comments and formatting for
/// everything this type's methods do not themselves touch.
#[derive(Clone, Debug)]
pub struct WebConfigDocument {
    document: DocumentMut,
}

impl WebConfigDocument {
    /// Parses an existing `web.toml`'s text, preserving its comments and
    /// formatting for subsequent edits.
    ///
    /// # Errors
    ///
    /// Returns [`WebConfigWriterError::Toml`] when `source` is not valid
    /// TOML (not schema-checked here; a structurally valid but
    /// schema-invalid document, for example one carrying an already
    /// malformed `server.bind`, parses successfully and is caught by the
    /// next mutating call's own validation pass instead).
    pub fn parse(source: &str) -> Result<Self, WebConfigWriterError> {
        Ok(Self {
            document: source
                .parse::<DocumentMut>()
                .map_err(|source| WebConfigWriterError::Toml {
                    source: Box::new(source),
                })?,
        })
    }

    /// Renders the current document back to `web.toml` text.
    #[must_use]
    pub fn render(&self) -> String {
        self.document.to_string()
    }

    /// Every configured `[[mcp_server]]` name, in file order. The settings
    /// route diffs this before and after an edit to learn which names, if
    /// any, the edit removed: the trigger for the registered-app-dependency
    /// check.
    #[must_use]
    pub fn mcp_server_names(&self) -> Vec<String> {
        self.document
            .get("mcp_server")
            .and_then(Item::as_array_of_tables)
            .map(|array| array.iter().filter_map(mcp_server_name).collect())
            .unwrap_or_default()
    }

    /// Appends a new `[[mcp_server]]` entry (`POST /server/settings/`). `entry`'s
    /// keys and value shapes are otherwise unconstrained here: the real
    /// constraint is [`WebConfig`]'s own schema (`transport`, `name`, and
    /// the fields each transport requires), enforced by re-parsing the
    /// rendered document below, the same schema `web.toml`'s own TOML form
    /// is held to.
    ///
    /// # Errors
    ///
    /// Returns [`WebConfigWriterError::UnsupportedValue`] when `entry`
    /// contains a JSON value this module cannot represent in TOML (`null`,
    /// or an array containing anything but a scalar). Returns
    /// [`WebConfigWriterError::Invalid`] and leaves the document unchanged
    /// when the resulting configuration would be invalid (a duplicate
    /// name, an unrecognised `transport`, a missing required field, or a
    /// pre-existing invalid `server.bind` this edit did not itself
    /// introduce).
    pub fn add_mcp_server(&mut self, entry: &JsonMap<String, JsonValue>) -> Result<(), WebConfigWriterError> {
        let backup = self.document.clone();
        let table = json_object_to_table(entry)?;
        let array = self
            .document
            .entry("mcp_server")
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .ok_or(WebConfigWriterError::McpServerNotAnArrayOfTables)?;
        array.push(table);
        self.validate_or_rollback(backup)
    }

    /// Merges `patch`'s keys into the `[[mcp_server]]` entry currently named
    /// `name` (`PATCH /server/settings/`, `target: "mcp_server"`), leaving every
    /// other key on that entry untouched, except that a patch carrying an
    /// `auth` table and no `token_env` removes the entry's top-level
    /// `token_env`, which the table supersedes.
    ///
    /// # Errors
    ///
    /// Returns [`WebConfigWriterError::UnknownMcpServerName`] when no
    /// configured entry matches `name`. Returns
    /// [`WebConfigWriterError::UnsupportedValue`] or
    /// [`WebConfigWriterError::Invalid`] as [`Self::add_mcp_server`]
    /// documents.
    pub fn patch_mcp_server(
        &mut self,
        name: &str,
        patch: &JsonMap<String, JsonValue>,
    ) -> Result<(), WebConfigWriterError> {
        let backup = self.document.clone();
        let table = self.find_mcp_server_table_mut(name)?;
        merge_patch(table, patch)?;
        // Once an `[mcp_server.auth]` table supplies the token, the entry's top-level `token_env` no longer applies; a
        // stale one left behind would quietly apply again if the table were later removed by hand.
        if patch.contains_key("auth") && !patch.contains_key("token_env") {
            table.remove("token_env");
        }
        self.validate_or_rollback(backup)
    }

    /// Replaces the `[[mcp_server]]` entry currently named `current_name`
    /// with `entry` in full (`PUT /server/settings/`).
    ///
    /// # Errors
    ///
    /// Returns [`WebConfigWriterError::UnknownMcpServerName`] when no
    /// configured entry matches `current_name`. Returns
    /// [`WebConfigWriterError::UnsupportedValue`] or
    /// [`WebConfigWriterError::Invalid`] as [`Self::add_mcp_server`]
    /// documents.
    pub fn replace_mcp_server(
        &mut self,
        current_name: &str,
        entry: &JsonMap<String, JsonValue>,
    ) -> Result<(), WebConfigWriterError> {
        let backup = self.document.clone();
        let table = json_object_to_table(entry)?;
        let index = self.find_mcp_server_index(current_name)?;
        // `find_mcp_server_index` already proved the array exists; this
        // second lookup only re-borrows it mutably (`toml_edit`'s immutable
        // and mutable array accessors are separate methods).
        let array = self
            .document
            .get_mut("mcp_server")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| WebConfigWriterError::UnknownMcpServerName {
                name: current_name.to_owned(),
            })?;
        array.replace(index, table);
        self.validate_or_rollback(backup)
    }

    /// Removes the `[[mcp_server]]` entry named `name` (`DELETE
    /// /server/settings/`).
    ///
    /// # Errors
    ///
    /// Returns [`WebConfigWriterError::UnknownMcpServerName`] when no
    /// configured entry matches `name`.
    pub fn remove_mcp_server(&mut self, name: &str) -> Result<(), WebConfigWriterError> {
        let backup = self.document.clone();
        let target = name.to_ascii_lowercase();
        let array = self
            .document
            .get_mut("mcp_server")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| WebConfigWriterError::UnknownMcpServerName { name: name.to_owned() })?;
        let before = array.len();
        array.retain(|table| mcp_server_name(table).map(|name| name.to_ascii_lowercase()) != Some(target.clone()));
        if array.len() == before {
            return Err(WebConfigWriterError::UnknownMcpServerName { name: name.to_owned() });
        }
        self.validate_or_rollback(backup)
    }

    /// Merges `patch`'s keys into `[server.ui]` (`PATCH /server/settings/`,
    /// `target: "ui"`); `[server.ui]`'s own key set is intentionally
    /// unenumerated (it belongs to the rendering and theme code), so any JSON-representable value is accepted
    /// here.
    ///
    /// # Errors
    ///
    /// Returns [`WebConfigWriterError::UnsupportedValue`] or
    /// [`WebConfigWriterError::Invalid`] as [`Self::add_mcp_server`]
    /// documents. Returns [`WebConfigWriterError::ServerNotATable`] or
    /// [`WebConfigWriterError::UiNotATable`] when `[server]`/`[server.ui]`
    /// is present in the document but is not itself a table (a
    /// hand-corrupted `web.toml`).
    pub fn patch_ui(&mut self, patch: &JsonMap<String, JsonValue>) -> Result<(), WebConfigWriterError> {
        let backup = self.document.clone();
        let ui = self.server_ui_table_mut()?;
        merge_patch(ui, patch)?;
        self.validate_or_rollback(backup)
    }

    /// The current `[server]` scalar values, each `None` when its key is
    /// absent, for `contextos-web config` to offer back as editable defaults
    /// on a re-run. Integer and boolean keys keep TOML's own `i64`/`bool`
    /// types; the wizard narrows them to its own field types.
    #[must_use]
    pub fn server_settings(&self) -> WebServerSettingsView {
        let server = self.document.get("server").and_then(Item::as_table);
        let string = |key: &str| {
            server
                .and_then(|table| table.get(key))
                .and_then(Item::as_str)
                .map(str::to_owned)
        };
        let integer = |key: &str| server.and_then(|table| table.get(key)).and_then(Item::as_integer);
        let boolean = |key: &str| server.and_then(|table| table.get(key)).and_then(Item::as_bool);
        WebServerSettingsView {
            bind: string("bind"),
            static_dir: string("static_dir"),
            log_level: string("log_level"),
            log_file: string("log_file"),
            log_max_size_mb: integer("log_max_size_mb"),
            log_rotate_daily: boolean("log_rotate_daily"),
            log_retention_days: integer("log_retention_days"),
            log_retention_files: integer("log_retention_files"),
        }
    }

    /// Writes `input` into `[server]`: `bind`, `log_level`, and `log_file`
    /// are always set, and every optional key `input` leaves unset is
    /// removed rather than left stale. Re-validates the whole document and
    /// rolls back on any failure (a non-loopback `bind`, an unsatisfiable
    /// log-rotation combination, and so on), exactly as
    /// [`Self::add_mcp_server`] does.
    ///
    /// # Errors
    ///
    /// Returns [`WebConfigWriterError::ServerNotATable`] when a pre-existing
    /// `[server]` is not a table, [`WebConfigWriterError::UnsupportedValue`]
    /// when a size or retention count exceeds TOML's signed-integer range,
    /// or [`WebConfigWriterError::Invalid`] (document unchanged) when the
    /// result would fail to re-load.
    pub fn set_server_settings(&mut self, input: &WebServerSettingsInput) -> Result<(), WebConfigWriterError> {
        let backup = self.document.clone();
        let server = self
            .document
            .entry("server")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .ok_or(WebConfigWriterError::ServerNotATable)?;
        server.insert("bind", string_item(&input.bind));
        server.insert("log_level", string_item(&input.log_level));
        server.insert("log_file", string_item(&input.log_file));
        set_or_remove_string(server, "static_dir", input.static_dir.as_deref());
        set_or_remove_integer(server, "log_max_size_mb", input.log_max_size_mb)?;
        set_or_remove_integer(server, "log_retention_days", input.log_retention_days.map(u64::from))?;
        set_or_remove_integer(server, "log_retention_files", input.log_retention_files.map(u64::from))?;
        if input.log_rotate_daily {
            server.insert("log_rotate_daily", Item::Value(TomlValue::from(true)));
        } else {
            server.remove("log_rotate_daily");
        }
        // A scalar inserted after `[server.ui]` has already been emitted
        // would render inside that sub-table; moving `ui` to the end of
        // `[server]` keeps every scalar above the sub-table header.
        if let Some(ui) = server.remove("ui") {
            server.insert("ui", ui);
        }
        self.validate_or_rollback(backup)
    }

    /// The current `[server.ui]` string value for `key`, if present and a
    /// string (a non-string value under an appearance key has no rendered
    /// meaning, matching [`crate::config::current_appearance`]'s own stance).
    #[must_use]
    pub fn ui_string(&self, key: &str) -> Option<String> {
        self.document
            .get("server")
            .and_then(Item::as_table)
            .and_then(|server| server.get("ui"))
            .and_then(Item::as_table)
            .and_then(|ui| ui.get(key))
            .and_then(Item::as_str)
            .map(str::to_owned)
    }

    /// Sets or clears individual `[server.ui]` string keys: `Some` writes
    /// the value, `None` removes the key. The shape `contextos-web config`
    /// needs for the appearance keys that have operator-visible effect,
    /// with the same validation and rollback as [`Self::patch_ui`].
    ///
    /// # Errors
    ///
    /// Returns [`WebConfigWriterError::ServerNotATable`] or
    /// [`WebConfigWriterError::UiNotATable`] when a pre-existing
    /// `[server]`/`[server.ui]` is not a table, or
    /// [`WebConfigWriterError::Invalid`] when the result would fail to
    /// re-load.
    pub fn set_ui_strings(&mut self, values: &[(&str, Option<&str>)]) -> Result<(), WebConfigWriterError> {
        let backup = self.document.clone();
        let ui = self.server_ui_table_mut()?;
        for (key, value) in values {
            match value {
                Some(text) => {
                    ui.insert(key, string_item(text));
                }
                None => {
                    ui.remove(key);
                }
            }
        }
        self.validate_or_rollback(backup)
    }

    /// Every configured `[[mcp_server]]` entry as a validated
    /// [`McpServerConfig`], for the wizard to prefill an edit from. Returns
    /// an empty vector when the document has none, or (a hand-corrupted
    /// file) when its `[[mcp_server]]` array does not parse against the
    /// schema; the wizard re-validates the whole document on its next write
    /// regardless.
    #[must_use]
    pub fn mcp_server_entries(&self) -> Vec<McpServerConfig> {
        #[derive(serde::Deserialize)]
        struct Entries {
            #[serde(default, rename = "mcp_server")]
            entries: Vec<McpServerConfig>,
        }
        toml::from_str::<Entries>(&self.document.to_string())
            .map(|parsed| parsed.entries)
            .unwrap_or_default()
    }

    fn server_ui_table_mut(&mut self) -> Result<&mut Table, WebConfigWriterError> {
        let server = self
            .document
            .entry("server")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .ok_or(WebConfigWriterError::ServerNotATable)?;
        server
            .entry("ui")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_mut()
            .ok_or(WebConfigWriterError::UiNotATable)
    }

    fn find_mcp_server_index(&self, name: &str) -> Result<usize, WebConfigWriterError> {
        let target = name.to_ascii_lowercase();
        self.document
            .get("mcp_server")
            .and_then(Item::as_array_of_tables)
            .and_then(|array| {
                array.iter().position(|table| {
                    mcp_server_name(table).map(|found| found.to_ascii_lowercase()) == Some(target.clone())
                })
            })
            .ok_or_else(|| WebConfigWriterError::UnknownMcpServerName { name: name.to_owned() })
    }

    fn find_mcp_server_table_mut(&mut self, name: &str) -> Result<&mut Table, WebConfigWriterError> {
        let target = name.to_ascii_lowercase();
        let array = self
            .document
            .get_mut("mcp_server")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| WebConfigWriterError::UnknownMcpServerName { name: name.to_owned() })?;
        array
            .iter_mut()
            .find(|table| mcp_server_name(table).map(|found| found.to_ascii_lowercase()) == Some(target.clone()))
            .ok_or_else(|| WebConfigWriterError::UnknownMcpServerName { name: name.to_owned() })
    }

    fn validate_or_rollback(&mut self, backup: DocumentMut) -> Result<(), WebConfigWriterError> {
        match render_and_validate(&self.document) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.document = backup;
                Err(error)
            }
        }
    }
}

/// Re-parses `document`'s rendered text through [`WebConfig`]'s own schema
/// deserializer and [`WebConfig::validate`], the identical pair
/// [`crate::config::load_web_config`] runs on startup: a document that
/// passes here is guaranteed loadable, never a divergent, writer-only
/// notion of validity.
fn render_and_validate(document: &DocumentMut) -> Result<(), WebConfigWriterError> {
    let text = document.to_string();
    let config: WebConfig = toml::from_str(&text).map_err(|source| WebConfigWriterError::Invalid {
        source: WebConfigError::Toml {
            source: Box::new(source),
        },
    })?;
    config
        .validate()
        .map_err(|source| WebConfigWriterError::Invalid { source })
}

/// The `name` field on one `[[mcp_server]]` table, if present and a string.
fn mcp_server_name(table: &Table) -> Option<String> {
    table.get("name").and_then(Item::as_str).map(str::to_owned)
}

fn json_object_to_table(map: &JsonMap<String, JsonValue>) -> Result<Table, WebConfigWriterError> {
    let mut table = Table::new();
    for (key, value) in map {
        table.insert(key, json_to_toml_item(value)?);
    }
    Ok(table)
}

/// Converts one submitted JSON value into the `toml_edit` item it
/// represents. `null` and a non-scalar array element have no TOML
/// representation this endpoint accepts and are rejected rather than
/// silently coerced (`rust-quality.md`: "reject malformed or unknown
/// input").
fn json_to_toml_item(value: &JsonValue) -> Result<Item, WebConfigWriterError> {
    match value {
        JsonValue::Null => Err(WebConfigWriterError::UnsupportedValue),
        JsonValue::Bool(flag) => Ok(Item::Value(TomlValue::from(*flag))),
        JsonValue::Number(number) => {
            if let Some(int) = number.as_i64() {
                Ok(Item::Value(TomlValue::from(int)))
            } else if let Some(float) = number.as_f64() {
                Ok(Item::Value(TomlValue::from(float)))
            } else {
                Err(WebConfigWriterError::UnsupportedValue)
            }
        }
        JsonValue::String(text) => Ok(Item::Value(TomlValue::from(text.clone()))),
        JsonValue::Array(items) => {
            let mut array = Array::new();
            for entry in items {
                let Item::Value(scalar) = json_to_toml_item(entry)? else {
                    return Err(WebConfigWriterError::UnsupportedValue);
                };
                array.push(scalar);
            }
            Ok(Item::Value(TomlValue::Array(array)))
        }
        JsonValue::Object(nested) => Ok(Item::Table(json_object_to_table(nested)?)),
    }
}

/// A TOML string item with default decoration, the value shape every scalar
/// `web.toml` key this module writes takes.
fn string_item(value: &str) -> Item {
    Item::Value(TomlValue::from(value))
}

/// Inserts `key = value` when `value` is `Some`, otherwise removes `key`.
fn set_or_remove_string(table: &mut Table, key: &str, value: Option<&str>) {
    match value {
        Some(text) => {
            table.insert(key, string_item(text));
        }
        None => {
            table.remove(key);
        }
    }
}

/// Inserts `key = value` (as a TOML signed integer) when `value` is `Some`,
/// otherwise removes `key`. Rejects a value beyond TOML's `i64` range rather
/// than truncating it.
fn set_or_remove_integer(table: &mut Table, key: &str, value: Option<u64>) -> Result<(), WebConfigWriterError> {
    if let Some(number) = value {
        let signed = i64::try_from(number).map_err(|_source| WebConfigWriterError::UnsupportedValue)?;
        table.insert(key, Item::Value(TomlValue::from(signed)));
    } else {
        table.remove(key);
    }
    Ok(())
}

fn merge_patch(table: &mut Table, patch: &JsonMap<String, JsonValue>) -> Result<(), WebConfigWriterError> {
    for (key, value) in patch {
        table.insert(key, json_to_toml_item(value)?);
    }
    Ok(())
}

/// Typed failures editing a `web.toml` document.
#[derive(Debug, Error)]
pub enum WebConfigWriterError {
    #[error("web configuration TOML is invalid")]
    Toml {
        #[source]
        source: Box<toml_edit::TomlError>,
    },
    #[error("the \"mcp_server\" key is not an array of tables")]
    McpServerNotAnArrayOfTables,
    #[error("the \"server\" key is not a table")]
    ServerNotATable,
    #[error("the \"server.ui\" key is not a table")]
    UiNotATable,
    #[error("no [[mcp_server]] entry named {name:?} is configured")]
    UnknownMcpServerName { name: String },
    #[error(
        "a submitted value cannot be represented in TOML (null, or a nested array containing \
         anything but a plain string, number, or boolean)"
    )]
    UnsupportedValue,
    #[error("the edited configuration is invalid")]
    Invalid {
        #[source]
        source: WebConfigError,
    },
}

#[cfg(test)]
#[path = "config_writer_test.rs"]
mod tests;
