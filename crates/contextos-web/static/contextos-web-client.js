// contextos-web MCP proxy client.
//
// Wraps `POST /mcp/{server_name}/{tool_name}` as
// `callTool(serverName, toolName, args, options)`, returning a Promise: the
// same deterministic, non-model-mediated tool-calling surface Cowork live
// artefacts previously reached via `callMcpTool`, with no LLM step between
// an app's own JavaScript and the tool result.
//
// By default the resolved value is unwrapped down to the tool's actual
// payload rather than the raw MCP result envelope (`structuredContent` when
// the tool supplies it, otherwise the sole text content block, JSON-parsed
// when it parses), and an MCP-level tool error (`isError: true`) rejects
// the promise instead of resolving with it: matching the `Promise`
// contract every other JavaScript async API observes, and sparing every
// app author the same content-unwrapping and `isError`-checking
// boilerplate. Pass `options.raw = true` to bypass unwrapping and receive
// the untouched MCP result envelope exactly as the proxy returned it
// (never rejecting on `isError`), for a caller that genuinely needs the
// full protocol shape: multiple content blocks, non-text content, or
// `_meta`.
//
// Served as a plain script (no bundler assumed): including it via
// `<script src="/static/contextos-web-client.js"></script>` exposes
// `window.contextosWeb.callTool`. It also exports via `module.exports` for
// CommonJS consumers (used by this crate's own Node-based contract test).
(function (root) {
  "use strict";

  /**
   * Unwraps an MCP `CallToolResult` body down to the tool's actual payload:
   * `structuredContent` when present (every `contextos-mcp` tool supplies
   * this, on both success and failure), otherwise the sole text content
   * block, JSON-parsed when it parses as JSON and returned as the raw
   * string otherwise. A result shaped some other way (no content, more
   * than one block, or a non-text block) falls back to the raw `content`
   * array, since there is no single payload to unwrap to.
   */
  function unwrapResult(body) {
    if (body && Object.prototype.hasOwnProperty.call(body, "structuredContent")) {
      return body.structuredContent;
    }
    const blocks = (body && body.content) || [];
    if (blocks.length === 1 && blocks[0] && blocks[0].type === "text") {
      const text = blocks[0].text;
      try {
        return JSON.parse(text);
      } catch (parseError) {
        return text;
      }
    }
    return blocks;
  }

  /**
   * The message a rejected tool-error carries: an unwrapped `ToolFailure`
   * object's own `message` field when present, the unwrapped value itself
   * when it is already a non-empty string, or a generic fallback.
   */
  function errorMessageFrom(unwrapped) {
    if (unwrapped && typeof unwrapped === "object" && typeof unwrapped.message === "string") {
      return unwrapped.message;
    }
    if (typeof unwrapped === "string" && unwrapped.length > 0) {
      return unwrapped;
    }
    return "MCP tool call failed";
  }

  /**
   * Calls `toolName` on the configured MCP server `serverName` with `args`.
   *
   * Resolves with the tool's unwrapped payload (see `unwrapResult`) on
   * success. Rejects on an MCP-level tool error (`isError: true`) with an
   * `Error` carrying `code` and `remediation` (when the unwrapped failure
   * supplies them) and `result` (the raw MCP result envelope). Rejects on a
   * transport-level failure (unconfigured server, malformed request, or the
   * proxied server being unreachable): the rejection error carries `status`
   * (the HTTP status code) and `body` (the parsed JSON error body) fields.
   *
   * `options.raw = true` resolves with the untouched MCP result envelope
   * instead, exactly as `POST /mcp/{server_name}/{tool_name}` returned it,
   * and never rejects on `isError`.
   *
   * `options.baseUrl` overrides the request's origin (default: relative to
   * the page's own origin), for use outside a browser document context.
   */
  async function callTool(serverName, toolName, args, options) {
    const payload = args === undefined ? {} : args;
    const baseUrl = (options && options.baseUrl) || "";
    const raw = Boolean(options && options.raw);
    const path =
      "/mcp/" +
      encodeURIComponent(serverName) +
      "/" +
      encodeURIComponent(toolName);

    const response = await fetch(baseUrl + path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    });
    const body = await response.json();

    if (!response.ok) {
      const message =
        (body && body.error) ||
        "contextos-web proxy request failed (" + response.status + ")";
      const error = new Error(message);
      error.status = response.status;
      error.body = body;
      throw error;
    }

    if (raw) {
      return body;
    }

    const unwrapped = unwrapResult(body);
    if (body && body.isError === true) {
      const error = new Error(errorMessageFrom(unwrapped));
      if (unwrapped && typeof unwrapped === "object") {
        if (unwrapped.code !== undefined) {
          error.code = unwrapped.code;
        }
        if (unwrapped.remediation !== undefined) {
          error.remediation = unwrapped.remediation;
        }
      }
      error.result = body;
      throw error;
    }
    return unwrapped;
  }

  root.contextosWeb = { callTool: callTool };
  if (typeof module !== "undefined" && module.exports) {
    module.exports = { callTool: callTool };
  }
})(typeof window !== "undefined" ? window : globalThis);
