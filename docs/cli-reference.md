# CLI reference

## Global flags

These apply to every invocation, including the bare server run:

| Flag | Effect |
| --- | --- |
| `--config <path>` | Load configuration from this TOML file instead of the default location |
| `--vault <path>` | Add an allowed vault directory; may repeat |
| `--log-level <level>` | Override the configured runtime log level (`error`, `warn`, `info`, `debug`, `trace`) |
| `--http [addr]` | Enable the HTTP transport for this run, optionally overriding the configured bind address (for example `--http 127.0.0.1:9000`); with no address, the configured or default bind is used unchanged |
| `--no-stdio` | Serve no stdio for this run, whatever `[server] transports` says. With `--http`, this process serves HTTP alone and keeps running with nothing on its standard input; without it, there is nothing to serve and the run is refused |
| `--astro` | Register the ephemeris tools for this run, overriding `[server] astro` in config; enable-only, there is no `--no-astro` |

## `contextos`

With no subcommand, starts the MCP server on the configured transport(s) and blocks until shut down (`Ctrl-C` or `SIGTERM`).

## `contextos service <install|uninstall|status|start|stop|restart>`

Registers and controls one per-user background `contextos` that serves MCP over HTTP alone, as `contextos --config <path> --http [ADDR] --no-stdio`. Give the global `--http ADDR` to `install` to serve on that address; without it the configured `[server.http] bind` applies. `install` refuses a configuration that cannot load or a bind the server would refuse. On Windows, `install --scheduled-task` registers a Scheduled Task instead of a Startup App. See [`mcp-service.md`](mcp-service.md).

## `contextos doctor [--resolve] [--dry-run]`

Reports actionable, read-only health checks: configuration validity, per-vault reachability, managed index staleness, Git recovery state, semantic search health, and whether each note's `base` property names a base that contains it. Never writes to a vault by itself.

A projection setting that cannot be applied (for example an agents or skills folder that is a symbolic link) does not stop `doctor`, with or without `--resolve`: it is reported as a failed `configuration` check carrying the same message the server would refuse to start with, and every vault check still runs. The server itself does not start over such a setting. When projection is on, the `configuration` check also shows this configuration's projection identity (12 hexadecimal characters).

- `--resolve`: additionally resolve every currently auto-fixable finding (a stale or missing managed index, MCP-made changes left uncommitted, a Git repository that is absent or has no commits yet, or an agent host projection that has drifted: re-projecting a stale or hand-edited entry, withdrawing an orphaned one, and restoring the orientation block), committing what it changed before it exits. Every write it makes is one of the outcomes it lists: unlike a starting server, it leaves Git's start-up work (committing changes an earlier process left pending, a first commit for a new repository) to the matching findings, so it does exactly what `--dry-run` previewed. Each outcome is printed as `Resolved:` or, when its remediation failed, `Not resolved:`, with the reason. A vault whose search state cannot be opened is still resolved, with a warning that its search indexes do not yet reflect the change; correct the cause and run `contextos index`. It holds every vault's write lock while it works, waiting while another process writes, and takes the host-level projection lock after them. A configuration that does not project never has a host folder touched. Mirrors the `doctor_resolve` MCP tool.
- `--dry-run`: with `--resolve`, report what would be resolved without writing anything.

## `contextos index`

Rebuilds every enabled vault search index (text and link graph) for the configured vaults.

## `contextos model <list|download>`

Manages the shared local embedding model cache. Vault-independent: works without `--config` or `--vault`.

- `list`: report the default local embedding model's cache status.
- `download`: fetch the default local embedding model into the shared cache.

## `contextos config`

With no further subcommand, runs the interactive guided-setup interview (vault selection, model download, host registration). Vault-independent: it edits `config.toml` directly and works against a not-yet-valid or not-yet-existing file.

### `contextos config vault <add|remove|list>`

Manages `[[vault]]` entries in `config.toml`.

- `add <name> <path> [--unmanaged]`: add a vault. `name` addresses it as `name://relative-path`; `--unmanaged` marks it filesystem-only (the tools still read and write its files, but managed indexes, the operation log, Git recovery, and search are disabled).
- `remove <name>`: remove a configured vault by name.
- `list`: list configured vaults.

### `contextos config mcp <register|deregister|status>`

Manages this server's entry in an MCP host's own configuration file. Currently supports `--host claude-desktop`; pass `--config-path` explicitly if host discovery reports more than one candidate configuration file, or none.

- `register --host <host> [--config-path <path>] [--force]`: add or update the entry. `--force` proceeds even though the host is detected running (logging a warning) instead of refusing.
- `deregister --host <host> [--config-path <path>] [--force]`: remove the entry.
- `status --host <host> [--config-path <path>]`: report whether the server is currently registered, without writing.
