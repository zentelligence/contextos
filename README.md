# ContextOS

ContextOS operates knowledge vaults: Obsidian-flavoured Markdown vaults with structured notes, Bases, and Canvas files, used by people and AI agents together. It ships as three programs built from this Rust workspace, and a plugin that teaches agents to use them.

| Program | Role |
| --- | --- |
| `contextos` | The MCP server: a vault-aware execution layer that AI hosts connect to, over stdio or authenticated streamable HTTP. It also holds the setup, health check, and update commands. |
| `contextos-web` | ContextOS Web: a local web interface that shows vaults, serves custom apps, and runs scheduled workflows, using `contextos` for every vault operation. |
| `contextos-launcher` | The stable launcher, installed on `PATH` as both commands. It starts the active release set, so updates apply without changing anything that calls them. |

The [ContextOS plugin](https://github.com/zentelligence/marketplace/tree/main/plugins/context-os), in the `zentelligence/marketplace` repository, gives Claude Code, Codex, and ChatGPT desktop the vault, app, workflow, and job skills and the vault scaffold. See [Plugin](#plugin).

## MCP server capabilities

`contextos` is not a generic filesystem MCP: every write is path-safe and atomic, and the server layers automatic `index.md` maintenance, append-only operation logging, local Git recovery, structured Obsidian operations, and text, graph, and semantic search on top.

| Area | Capability |
| --- | --- |
| Filesystem | Read, write, edit, list, tree, search, move, inspect, attach, and guarded delete within configured roots |
| Substrate | Preserve operator prose while maintaining directory indexes and append-only daily operation logs |
| Recovery | Stage mutations and create debounced local Git commits; restore historical content without rewriting history |
| Obsidian | Structured note/frontmatter, Bases, Canvas, and wikilink operations with validation |
| Query | Ranked text search, link-graph traversal, and opt-in local semantic search |
| Projection | Project a vault's agents, skills, and global instructions into Claude Code and Codex |
| Transport | MCP protocol revision 2026-07-28 (earlier revisions as far as the rmcp SDK serves them) over stdio and authenticated streamable HTTP from one service instance |
| Extension | Feature-gated, namespaced modules using the same secured core services |

See [`docs/mcp-tools.md`](docs/mcp-tools.md) for the full tool catalogue.

## Web interface capabilities

`contextos-web` binds to a loopback address, because it has no authentication of its own, and reaches vaults only through the MCP servers it is configured with.

| Area | Capability |
| --- | --- |
| Vaults | Render notes, Bases, Canvases, and diagrams; a daily Journal; vault settings; and a live Signals screen of vault events, tasks, and agents |
| Apps | Serve custom web apps registered in a vault under `registry/apps/{slug}/`, each described by a `manifest.toml` and able to call MCP tools through the interface |
| Workflows | Run Rhai workflows from a vault's `registry/workflows/` or the host's package store, on demand or from the command line |
| Jobs | Save a workflow's configuration as a job, schedule it with cron, review it before it runs unattended, and keep a record of every run |
| Packages | Install, version, and remove workflow and library packages per vault or for the whole host |
| Service | Run as a per-user background service, with a settings page for each server's connection and sign-in |

See [`docs/web-first-launch.md`](docs/web-first-launch.md) and [`docs/web-workflows.md`](docs/web-workflows.md).

## Installation

Download the archive for your platform from [Releases](https://github.com/zentelligence/contextos/releases). Each one holds all three programs and a README that walks through installing, setting up, and updating them; [`docs/installation.md`](docs/installation.md) covers the same ground.

To build from source instead, you need a recent stable Rust toolchain (see `rust-version` in `Cargo.toml`):

```sh
cargo build --workspace --release
```

or install one program onto your `PATH`:

```sh
cargo install --path crates/contextos-mcp
cargo install --path crates/contextos-web
```

## Quick start

Run the guided setup, which creates `config.toml`, adds a vault, and can register the server with an MCP host:

```sh
contextos config
```

Or run directly against a vault with no configuration file:

```sh
contextos --vault /path/to/vault
```

Check configuration, vault reachability, and recovery state at any time with:

```sh
contextos doctor
```

Then set up the web interface, add `contextos` as its first server, and start it:

```sh
contextos-web config
contextos-web
```

It serves on <http://127.0.0.1:7373/> by default. To have it start at login, use `contextos-web service install`.

## Plugin

The plugin is published in the [`zentelligence/marketplace`](https://github.com/zentelligence/marketplace) repository, and is developed alongside this one: its `mcp-tools` skill documents this server's tools, and `.claude/scripts/check-plugin-skill.sh` keeps the two in step. In Claude Code:

```text
/plugin marketplace add zentelligence/marketplace
/plugin install context-os@zentelligence-plugins
```

Then run `/vault init` in your vault. The marketplace README covers Codex and ChatGPT desktop.

## Design principles

- **Safe by construction:** every path is resolved through a root-scoped `VaultPath`; traversal and symlink escapes are rejected.
- **One mutation pipeline:** validation, conflict detection, atomic persistence, indexing, logging, Git staging, and search updates share one flow.
- **Operator content is authoritative:** machine-managed regions never overwrite prose outside their explicit markers.
- **Local first:** Git history and default embeddings stay on the operator's machine; remote Git operations are outside v1.
- **Graceful degradation:** plain directories retain filesystem tools while ContextOS services can be disabled per vault.
- **Explicit conversions:** project types convert only through Rust's `From` or `TryFrom` traits.

## Architecture

Hexagonal Cargo workspace; every dependency edge points towards `contextos-core`.

| Crate | Responsibility |
| --- | --- |
| `contextos-core` | Domain types, validated paths, errors, service traits, and write-pipeline contracts |
| `contextos-fs` | Atomic, conflict-aware filesystem operations |
| `contextos-obsidian` | Markdown, frontmatter, Bases, Canvas, and link codecs |
| `contextos-index` | Managed `index.md` reconciliation |
| `contextos-oplog` | Append-only operation logging |
| `contextos-events` | Vault event kinds, record shape, validation, task change tracking, and warning classification |
| `contextos-git` | Local repository status, staging, commit, diff, and recovery |
| `contextos-search` | Text, graph, vector, and embedding services |
| `contextos-markdown` | Pure rendering of vault notes to HTML: the dialect, fences, author HTML allowlist, and link routing port |
| `contextos-mermaid` | Mermaid diagram parsing, layout, and SVG rendering |
| `contextos-ephemeris` | Optional astronomical/astrological calculations (moon phase, solar events, personal-year periods) |
| `contextos-projection` | Projection of vault agents, skills, and orientation into agent hosts, with confined targets and ownership records |
| `contextos-packages` | Layout and checks for installed workflow package releases |
| `contextos-host` | The operator's home and platform data folders, shared by every binary |
| `contextos-logging` | Rotating, retention-bounded log destination shared by every binary |
| `contextos-service` | Per-user background service management (systemd, launchd, Windows) |
| `contextos-mcp` | MCP tool registry, configuration, stdio, and HTTP transports (the `contextos` binary) |
| `contextos-workflows` | Rhai workflow execution, saved-job scheduling rules, and run-record helpers |
| `contextos-web` | Web interface, saved-job discovery and scheduling, and the workflow engine's MCP caller (the `contextos-web` binary) |
| `contextos-launcher` | The stable launcher that resolves and runs the active release set (the `contextos-launcher` binary) |

Every mutation passes through one write pipeline: validate, check for conflicts, write atomically (temp file, fsync, rename), then route the resulting event to the index, operation log, Git, and search substrates. A downstream substrate failure becomes a typed warning; it never rolls back a completed write.

A workflow defines executable logic, a job saves its configuration, and a run records one invocation. `contextos-web` hosts `contextos-workflows`; the execution library has no dependency on its host. The Workflows navigation heading leads to saved-job management, where job-specific manifests, routes, and favourites retain their names.

## Documentation

- [`docs/installation.md`](docs/installation.md): installing, first run, and updating
- [`docs/configuration.md`](docs/configuration.md): `config.toml` reference
- [`docs/cli-reference.md`](docs/cli-reference.md): every `contextos` subcommand
- [`docs/mcp-tools.md`](docs/mcp-tools.md): the MCP tools the server registers
- [`docs/web-first-launch.md`](docs/web-first-launch.md): starting the web interface and connecting its servers
- [`docs/web-workflows.md`](docs/web-workflows.md): workflows, jobs, and packages
- [`docs/rendering.md`](docs/rendering.md): how the web interface renders notes, Base, Canvas, and diagram files
- [`docs/README.md`](docs/README.md): everything else
- API/code reference: run `cargo doc --workspace --no-deps --open`

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the development loop and quality gate. All contributors and coding agents must read [`AGENTS.md`](AGENTS.md), which governs trait-first design, test-driven delivery, and the security and quality gates every change must pass.

## Licence

Apache 2.0. Copyright Zentelligence Pty Ltd.
