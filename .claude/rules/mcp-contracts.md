# MCP Contract Rules

The MCP surface is a public compatibility boundary. Treat tool names, input schemas, result shapes, error codes, resource media types, and transport behaviour as versioned contracts.

## Tool implementation

- Keep one domain operation behind all transports. Stdio and streamable HTTP must expose identical catalogues and semantics.
- Deserialize into request DTOs, reject unknown or malformed fields as defined by the contract, then convert to validated domain types with `From` or `TryFrom`. Never use ad hoc conversion functions at this boundary.
- Return structured results. Do not make clients parse prose to obtain fields.
- Include non-fatal secondary-service failures in `warnings` after a successful mutation.
- Preserve stable machine-readable error codes and provide an actionable human message plus remediation hint.
- Enforce documented limits before allocating or reading full content.
- Batch tools isolate per-item failures when the specification promises partial success.

## Contract tests

For every tool, cover:

1. advertised name, description, and JSON schema;
2. smallest valid request and representative complete request;
3. response schema and content-block encoding;
4. every documented error code;
5. unknown fields, missing required fields, and incompatible options;
6. limits, conflicts, path rejection, and warning propagation; and
7. parity across transports once HTTP exists.

Tests should invoke the MCP adapter rather than calling only the underlying service. Keep separate service tests for domain behaviour.

## Plugin skill currency

The ContextOS plugin ships an `mcp-tools` skill (`plugins/context-os/skills/mcp-tools/` in the `marketplace` repository) that teaches agents to use these tools efficiently and safely. It is not a schema catalogue, because agents already read schemas from the advertised catalogue. It records what schemas cannot: which tool is cheapest for a job, which sequences are safe, and what the server maintains on its own. It is the agent-facing documentation of this public contract, so it changes in the same delivery as the behaviour it describes.

Update the skill when a change:

- adds, renames, or removes a tool;
- changes the meaning of a parameter, a default, or a limit or threshold the skill quotes;
- changes what a tool returns or omits, including a hash, warning, or error code that a workflow relies on;
- starts or stops work the server does automatically, such as index maintenance, operation logging, Git commits, or search refresh; or
- makes existing advice wrong, or makes a better practice possible.

Working rules:

1. State the skill impact in the change brief, even when it is none.
2. Edit the skill in the `marketplace` repository and follow that repository's authoring rules, including its manifest and version bump.
3. Run `.claude/scripts/check-plugin-skill.sh`, which `just ci` includes. It fails when a shipped tool is not named in the skill, the skill names a tool that no longer exists, or the fence vocabulary the plugin's vault workflow package ships (`workflows/vault/templates/shipped/.contextos/fences.toml`) differs from the renderer's default (`crates/contextos-markdown/default-fences.toml`). It reports a skip when no plugin checkout is available, and a skipped check is reported as skipped, never counted as passed. `CONTEXTOS_PLUGIN_DIR` points it at the plugin folder when the `marketplace` repository is not beside this one, and a value that is not a directory fails rather than skips.
4. Report what changed in the skill, or why no change was needed.

The check is mechanical and covers names only. Whether the advice is still right is a review question, answered through the review lenses.

Protocol revisions and SDK versions are temporally sensitive. Before changing them, consult the official MCP specification and official Rust SDK sources, record the protocol revision in the change, and add compatibility tests. Wrap pre-1.0 SDK types at the server boundary so SDK churn does not leak into core.
