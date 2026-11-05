//! The size limits every surface that carries whole file content shares, so a file one surface accepts is never
//! refused by another: a tool's write and attachment caps, the HTTP transport's request body cap, and the web
//! server's upload and proxy routes.

/// The largest file a tool writes or attaches whole: 10 MiB.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Room in a request, beyond the file content it carries, for everything else: the JSON-RPC envelope (method, tool
/// name, path, and every other argument) or a multipart upload's boundaries and headers.
const REQUEST_ENVELOPE_BYTES: u64 = 512 * 1024;

/// The default largest request body a surface that receives file content accepts: a [`MAX_FILE_BYTES`] file once
/// base64 encoding (four bytes for every three) has grown it, plus [`REQUEST_ENVELOPE_BYTES`], rounded up to a whole
/// MiB. That is 14 MiB, so the largest file a tool accepts always fits in one request.
pub const MAX_REQUEST_BODY_BYTES: u64 = {
    let encoded = MAX_FILE_BYTES.div_ceil(3) * 4;
    (encoded + REQUEST_ENVELOPE_BYTES).div_ceil(1024 * 1024) * 1024 * 1024
};

#[cfg(test)]
#[path = "limits_test.rs"]
mod tests;
