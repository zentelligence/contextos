# Security and Data-Integrity Rules

Assume all tool parameters, vault content, paths, frontmatter, Git refs, HTTP headers, and extension-module requests are untrusted.

## Path confinement

- Allowed roots come only from trusted configuration or CLI input, never a tool call.
- `VaultPath::try_new` is the sole construction boundary for tool paths.
- Resolve roots and targets consistently, including intermediate symlinks and non-existent final components needed for create operations.
- Reject traversal, root-prefix confusion, symlink escapes, Windows verbatim path tricks, drive/UNC mismatches, and alternate data streams.
- Revalidate confinement at the point of mutation to reduce check/use races.
- Never use lexical `starts_with` on unnormalised user input as an authorisation check.

## Host projection

Agent host projection is the only write outside the vault roots and the state root. It is confined as strictly as a vault.

- Projection targets come only from the `[projection]` configuration, never a tool call. Each target root is resolved once at load; a target inside a vault root is a configuration error.
- Build every projected path from a validated slug and validated relative paths. Below a target root, reject symbolic links, Windows junctions and other reparse points, traversal, drive or UNC prefix changes, and alternate data streams; never follow a link in a source skill folder either.
- Write, replace, or remove only entries the target's ownership manifest records as this configuration's, plus the bytes between the server's own markers in an instructions file. An unrecorded file is the operator's: report it, never touch it.
- Revalidate confinement at the point of each write, and hold the host-level projection lock across every change, since several server processes share the targets.
- Log the host and slug only; never definition content or full home paths at INFO and above.

## Persistence

- Write a uniquely named temporary file in the target directory, flush it as required by the platform contract, then atomically replace the target.
- Conflict checks compare the version the caller observed with current content. `force` must be explicit and must not bypass root or schema validation.
- Multi-operation Base and Canvas updates are transactional.
- Deletes default to recoverable platform trash semantics where supported; hard deletion is separately configured and explicit.
- A Git restore creates a new mutation and commit. Never rewrite history.

## Secrets and exposure

- Secrets, bearer tokens and provider API keys alike, may be stored as values in local configuration and set through the web interface's Settings. The servers run locally, so anyone who can read the configuration already has access to them, and the people installing ContextOS mostly cannot set environment variables. An environment variable, where a field accepts one, is an alternative, never a requirement.
- Never echo a secret back: redact it in `vault_info`, diagnostics, logs, and errors, and never return it through a tool result. The one exception is the web interface's Settings, which shows each secret obscured by default and reveals the raw value only when the operator asks for that field.
- Require bearer authentication for every non-loopback HTTP bind and compare tokens without timing leakage.
- Default to no CORS and a bounded request body.
- Never return filesystem content through metadata, diagnostics, or logs beyond the explicit read/attach contract.
- Test redaction and rejection behaviour, including error paths.

Security-relevant defects require a regression test at the lowest affected layer and an end-to-end rejection test at the tool boundary.

