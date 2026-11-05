//! `POST /{{vault_name}}/inbox/{{*path}}/execute/upload` - the Inbox upload action: split out of
//! `routes::inbox_outbox` (already close to this workspace's own file-size guidance) once the
//! upload boundary grew a size/content-sniffed denylist alongside its existing filename and
//! target-directory checks, matching `routes::wiki_create`'s identical precedent of splitting a
//! screen's own creation/upload action into its own module.
//!
//! The route table entry, the GET/browsing views, and the Ingest action all stay in
//! `routes::inbox_outbox`, since that is where every Inbox response is rendered; this module only
//! ever calls back into it (`inbox_response`), never the reverse.

use axum::extract::Multipart;
use axum::response::Response;
use base64::Engine as _;
use serde_json::{Map, Value};

use crate::inbox_outbox::{self, InboxLocation};
use crate::inbox_outbox_config;
use crate::mcp_client::{McpCallError, McpClient};
use crate::routes::vault::unreachable;

use super::inbox_outbox::{InboxOutboxRoutesState, ensure_vault, inbox_response};

/// The upload endpoint's own hard cap: the shared file limit
/// ([`contextos_core::MAX_FILE_BYTES`], 10 MiB) `fs_write_file` and
/// `fs_attach_file` use, applied here to the raw byte count before any
/// encoding, so a file accepted here is never refused by the write behind it.
const MAX_UPLOAD_BYTES: u64 = contextos_core::MAX_FILE_BYTES;

enum UploadOutcome {
    Uploaded,
    Rejected(String),
}

/// Extensions this Inbox upload boundary refuses outright, independent of whether the content
/// happens to decode as UTF-8 text: executables, OS/shell/PowerShell scripts, and installer
/// formats. Deliberately scoped to what can itself run, not archives generally: a `.zip`,
/// `.tar`, or Office document is accepted outright, with no inspection of what it contains
/// (unpacking an uploaded archive to vet its own contents is out of scope for this boundary).
/// A script format with no magic-byte signature of its own (a `.sh`/`.ps1` renamed to an accepted
/// extension) cannot be caught by [`content_is_denylisted_executable`]'s own content-sniffing;
/// this extension list is the only defence against that specific disguise.
const DENYLISTED_UPLOAD_EXTENSIONS: &[&str] = &[
    "exe",
    "com",
    "bat",
    "cmd",
    "msi",
    "msix",
    "msixbundle",
    "msp",
    "mst",
    "scr",
    "pif",
    "gadget",
    "application",
    "hta",
    "jar",
    "jnlp",
    "vb",
    "vbs",
    "vbe",
    "js",
    "jse",
    "ws",
    "wsf",
    "wsh",
    "wsc",
    "ps1",
    "ps1xml",
    "ps2",
    "ps2xml",
    "psc1",
    "psc2",
    "psd1",
    "psdm1",
    "sh",
    "bash",
    "zsh",
    "csh",
    "ksh",
    "run",
    "bin",
    "elf",
    "out",
    "app",
    "command",
    "action",
    "workflow",
    "apk",
    "ipa",
    "dmg",
    "pkg",
    "deb",
    "rpm",
    "dll",
    "so",
    "dylib",
    "sys",
    "drv",
    "scf",
    "lnk",
    "inf",
    "reg",
    "url",
    "cpl",
    "chm",
    "diagcab",
    "diagcfg",
    "diagpkg",
    "ade",
    "adp",
    "ins",
    "isp",
    "shb",
    "shs",
    "nsh",
];

/// Whether `filename`'s own extension (case-insensitive) is in
/// [`DENYLISTED_UPLOAD_EXTENSIONS`]. A filename with no extension is never denylisted by this
/// check alone.
fn extension_is_denylisted(filename: &str) -> bool {
    std::path::Path::new(filename)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| DENYLISTED_UPLOAD_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
}

/// Content-sniffed defence in depth against a renamed native binary specifically, using the
/// just-uploaded bytes already in hand (unlike the Inbox listing's own extension-only MIME
/// display, which has no content to sniff). Deliberately narrower than "anything `infer`
/// classifies as `MatcherType::App`": that category also covers PEM/DER-encoded certificates and
/// keys (`infer`'s own `is_pem` matches any buffer starting with the literal `-----BEGIN `, and
/// `is_der` matches a generic 2-byte ASN.1 tag) and QEMU disk images, none of which are
/// executable content, and an operator dropping a certificate or key into the Inbox for later
/// reference is entirely plausible. Checking only the extensions real double-click-and-it-runs
/// native binaries use avoids denying that legitimate content while still catching a disguised
/// `.exe`/`.elf`/`.dll`/`.dylib`/`.so`/Mach-O binary renamed to an accepted extension.
fn content_is_denylisted_executable(bytes: &[u8]) -> bool {
    const NATIVE_BINARY_EXTENSIONS: &[&str] = &["exe", "elf", "dll", "mach"];
    infer::get(bytes).is_some_and(|kind| {
        kind.matcher_type() == infer::MatcherType::App && NATIVE_BINARY_EXTENSIONS.contains(&kind.extension())
    })
}

/// Upload-specific wording over the shared [`inbox_outbox::sanitised_path_segment`]
/// check: the underlying rule (non-empty, one plain segment) is identical to
/// the settings section's own directory-name fields, but the error text
/// here speaks to "the uploaded file's name" specifically.
fn sanitised_upload_name(raw: &str) -> Result<&str, String> {
    inbox_outbox::sanitised_path_segment(raw).map_err(|reason| match reason {
        "must not be empty" => "Choose a file to upload.".to_owned(),
        _ => "The uploaded file's name must be a plain filename, not a path.".to_owned(),
    })
}

/// The reason a submitted file never reaches `fs_write_file` at all: checked in this fixed order
/// (size before type) so a payload that is both oversized and a denylisted type always reports
/// the size problem, the cheaper and more fundamental of the two boundary violations.
fn upload_boundary_rejection(filename: &str, bytes: &[u8]) -> Option<String> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_UPLOAD_BYTES {
        return Some(format!("\"{filename}\" is larger than the 10 MB upload limit."));
    }
    if extension_is_denylisted(filename) || content_is_denylisted_executable(bytes) {
        return Some(format!(
            "\"{filename}\" is an executable or script file type, which cannot be uploaded to the Inbox."
        ));
    }
    None
}

async fn upload_field(
    client: &McpClient,
    vault_name: &str,
    directory: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<UploadOutcome, McpCallError> {
    if let Some(reason) = upload_boundary_rejection(filename, bytes) {
        return Ok(UploadOutcome::Rejected(reason));
    }

    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{directory}/{filename}")),
    );
    // Text content round-trips as `content`; anything that fails full UTF-8 validation goes
    // through `fs_write_file`'s own base64 binary write path instead, matching how the read side
    // (`fs_attach_file`) already tells text and binary content apart.
    if let Ok(text) = std::str::from_utf8(bytes) {
        args.insert("content".to_owned(), Value::String(text.to_owned()));
    } else {
        args.insert(
            "content_base64".to_owned(),
            Value::String(base64::engine::general_purpose::STANDARD.encode(bytes)),
        );
    }
    match client.call_tool("fs_write_file".to_owned(), args).await? {
        result if result.is_error != Some(true) => Ok(UploadOutcome::Uploaded),
        _ => Ok(UploadOutcome::Rejected(format!(
            "\"{filename}\" could not be uploaded (a file with that name may already exist there)."
        ))),
    }
}

/// `POST /{{vault_name}}/inbox/{{*path}}/execute/upload`: writes the submitted
/// file straight into whichever pending-tree directory `raw_path` names (the
/// screen's own root, `inbox_dir`, when absent), via `fs_write_file`, at most
/// [`MAX_UPLOAD_BYTES`] and outside [`DENYLISTED_UPLOAD_EXTENSIONS`]/
/// [`content_is_denylisted_executable`]; text content and binary content
/// (a scanned PDF, an image, an Office document, an audio recording, and
/// so on) both round-trip correctly through `fs_write_file`'s own
/// `content`/`content_base64` fields. Refuses outright when
/// `raw_path` names a location inside the processed tree: uploading there
/// would misrepresent an operator's own new file as already handled.
/// Omitting `expected_hash` means a same-named collision fails closed as a
/// conflict rather than overwriting the existing file.
pub(super) async fn upload_dispatch(
    state: &InboxOutboxRoutesState,
    vault_name: &str,
    raw_path: Option<&str>,
    mut multipart: Multipart,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, vault_name).await {
        return *response;
    }
    let config = inbox_outbox_config::load_inbox_outbox_config(&client, vault_name).await;

    let location = match inbox_outbox::resolve_inbox_location(&config, raw_path) {
        Ok(location) => location,
        Err(reason) => {
            return inbox_response(
                &client,
                vault_name,
                &config,
                raw_path,
                Some(format!("path {reason}.")),
                None,
            )
            .await;
        }
    };
    let target_dir = match &location {
        InboxLocation::MergedRoot => config.inbox_dir.clone(),
        InboxLocation::Tree { status, .. } if *status == "pending" => {
            location.directory().unwrap_or_else(|| config.inbox_dir.clone())
        }
        InboxLocation::Tree { .. } => {
            return inbox_response(
                &client,
                vault_name,
                &config,
                raw_path,
                Some("Upload is only available in the Inbox, not the processed folder.".to_owned()),
                None,
            )
            .await;
        }
    };

    let field = match multipart.next_field().await {
        Ok(Some(field)) => field,
        Ok(None) => {
            return inbox_response(
                &client,
                vault_name,
                &config,
                raw_path,
                Some("Choose a file to upload.".to_owned()),
                None,
            )
            .await;
        }
        Err(_) => {
            return inbox_response(
                &client,
                vault_name,
                &config,
                raw_path,
                Some("The upload request was malformed.".to_owned()),
                None,
            )
            .await;
        }
    };
    let raw_filename = field.file_name().unwrap_or_default().to_owned();
    let filename = match sanitised_upload_name(&raw_filename) {
        Ok(name) => name.to_owned(),
        Err(reason) => return inbox_response(&client, vault_name, &config, raw_path, Some(reason), None).await,
    };
    let Ok(bytes) = field.bytes().await else {
        return inbox_response(
            &client,
            vault_name,
            &config,
            raw_path,
            Some("The upload request was malformed.".to_owned()),
            None,
        )
        .await;
    };

    match upload_field(&client, vault_name, &target_dir, &filename, &bytes).await {
        Ok(UploadOutcome::Uploaded) => inbox_response(&client, vault_name, &config, raw_path, None, None).await,
        Ok(UploadOutcome::Rejected(reason)) => {
            inbox_response(&client, vault_name, &config, raw_path, Some(reason), None).await
        }
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}

#[cfg(test)]
#[path = "inbox_outbox_upload_test.rs"]
mod tests;
