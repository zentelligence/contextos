# Configuration

`contextos` reads a single TOML file, resolved in this order: the `--config` flag, then the `CONTEXTOS_MCP_CONFIG` environment variable, then `~/.config/contextos/config.toml`. [`config.example.toml`](config.example.toml) is a complete, annotated example covering every currently supported field; this page summarises the shape.

## `[server]`

| Field | Purpose |
| --- | --- |
| `transports` | Which transports to run: `"stdio"`, `"http"`, or both |
| `log_level` | `error`, `warn`, `info`, `debug`, or `trace` |
| `log_file` | Empty logs to stderr only. Safe to share between processes; see [Logging to a file](#logging-to-a-file) |
| `log_max_size_mb` | Rotates `log_file` once it reaches this size, in megabytes. Rejected while `log_file` is empty |
| `log_rotate_daily` | Rotates `log_file` on every UTC calendar-day boundary. Combinable with `log_max_size_mb`: whichever condition is reached first triggers a rotation. Rejected while `log_file` is empty |
| `log_retention_days` | Deletes a rotated log file once it is older than this many days. Rejected while `log_file` is empty or neither rotation trigger is set |
| `log_retention_files` | Keeps only the N most recently rotated log files, deleting older ones. Same rejection rules as `log_retention_days` |
| `resource_link_threshold_kb` | Size at or above which a text-reading tool attaches a resource link with a bounded preview, rather than returning the full content inline. It also caps the length of a byte part of `fs_read_text_file`, so it must be at least 1: a value of 0 is refused when the configuration loads (`config/invalid-limit`) |
| `timezone` | The IANA time zone (such as `Australia/Sydney`) every time written into a vault is shown in: a note's `created` and `updated` stamps, managed index stamps, the day an operation log entry is filed under, agent events, and, through `vault_info`, every time `contextos-web` writes: the dates it gives new notes, journal entries, tasks, and decisions, and workflow run records (their dated folder and `started` and `completed` stamps), job state stamps, and shared run notes. Workflow schedules are read as wall-clock time in this zone too, so `0 7 * * *` runs at 07:00 local time all year. Each instant takes the offset its zone had then, daylight saving included. Omitted, the host's own zone is used; a fixed offset such as `+10:00` and a zone the time zone database does not know are refused at load. `doctor`'s `configuration` check names the zone in effect, and fails when none is set and the host's cannot be determined, since UTC then stands in |
| `astro` | Registers the ephemeris tools (moon phase, solar events, Wheel-of-the-Year, personal-year period) into the advertised tool catalogue. Off by default; every handler is compiled in regardless, this only controls visibility, and it can be overridden per run with `--astro` |

### Logging to a file

`log_file` may be shared by any number of `contextos` and `contextos-web` processes, which is the normal case because every stdio `contextos` loads the same `config.toml`. Rotation is coordinated through the filesystem, so nothing needs configuring:

- `log_max_size_mb` applies to the file's real size, not to what each process wrote. A file can exceed it by at most about one line per process writing at that instant.
- One process rotates; the others notice before their next write, reopen the path, and carry on in the new file, so no line is lost into a rotated file. A daily rotation happens once per UTC day however many processes run.
- An empty lock file, `.{log file name}.lock`, beside the log serialises rotation. It is safe to ignore, is never deleted, and retention never counts or removes it.
- If the lock cannot be created or taken (a read-only folder, say), logging carries on without rotating until it can. Removing an old rotated file another process still has open never stops that process logging.

`contextos config` asks for the log file and, only once one is chosen, its maximum size, daily rotation, and retention, each prefilled from `config.toml`. While no `log_file` is set it offers one where a background service would otherwise lose stderr: `%LOCALAPPDATA%\contextos\logs\contextos.log` on Windows and `~/Library/Logs/contextos/contextos.log` on macOS, rotated at 10 MB and kept for 7 days. On Linux it offers stderr, which `systemd` keeps in its journal. Deleting the prefilled path logs to stderr and drops the rotation and retention settings. The log folder is created only when the interview completes. `contextos-web config` offers `contextos-web.log` in the same folder by the same rule.

### `[server.http]`

Only used when `"http"` is in `transports`, or when `--http` is given (as `contextos service` does; see [`mcp-service.md`](mcp-service.md)), in which case `--no-stdio` also drops `"stdio"` for that run. `bind` defaults to `127.0.0.1:3737`; a non-loopback bind refuses to start unless a token is configured, and a configured token is enforced on every request once bound. Supply the token one of two ways: write it in `token`, or name an environment variable holding it in `token_env`. Setting both is refused with `config/conflicting-secret`. The variable is read only by the process that serves HTTP, from its own environment, when it starts; if it is unset or empty that process refuses to start (`config/missing-secret-variable`) rather than serve without the token you asked for, while stdio-only processes sharing the file never read it. `CONTEXTOS_MCP_TOKEN`, when set, replaces either form. A loopback bind accepts only requests naming a loopback host (`localhost`, `127.0.0.1`, or `::1`) in their `Host` header, its defence against DNS rebinding; a non-loopback bind, protected by its token, accepts any host name or address clients use to reach it. `max_body_kb` caps request body size; it defaults to 14336 (14 MiB), the shared request limit derived from the 10 MiB file limit `fs_write_file` and `fs_attach_file` use, so the largest permitted write fits in one request once base64 encoded.

## `[[vault]]`

One entry per vault. `path` is the vault root; `name` addresses it as `name://relative-path` on every path-accepting tool parameter and defaults to the root directory's basename (it must be unique and a valid URI scheme token). `managed` controls whether the server runs its services for the vault: an unmanaged vault is filesystem-only, so the tools still read and write its files, but there is no `index.md` maintenance, operation log, Git recovery, or search. `hidden` and `resources_list_include` control what the enumeration and resource-listing surfaces show, and a file `hidden` hides is also no part of the link graph, so no link reaches it; `state_directory` overrides where derived state (indexes, embeddings) is stored.

A vault cannot be named `api`, `mcp`, `server`, `shell`, or `static`: the web interface uses those addresses itself, and a vault's pages are at `/{name}/`. `contextos` refuses to start with one (`path/reserved-vault-name`), and `contextos config` refuses to add one. A vault with no `name` takes its folder's, so a folder called `server` needs a `name` of its own.

### `[vault.limits]`

`max_read_mb` and `max_batch_files` bound single-call resource use.

### `[vault.index_md]`

`enabled` toggles managed `index.md` reconciliation; `exclude` lists paths it skips, by default `.contextos`, `.git`, `.obsidian`, `memory/log`, `memory/sessions`, `memory/jobs`, and `memory/events`. How the links and frontmatter are generated, and how each index's `entity` is derived from the vault's `.contextos/entities.toml`, is described under [Managed `index.md` maintenance](mcp-tools.md#managed-indexmd-maintenance).

### `[vault.oplog]`

`enabled` toggles the append-only daily operation log; `path` is its location within the vault (typically `memory/log`).

### `[vault.git]`

`enabled` toggles local Git recovery. `commit_debounce_s` is the quiet period after the last mutation before pending changes are committed together (see [How mutations are committed](mcp-tools.md#how-mutations-are-committed)), and is also how long an MCP-made change may stay uncommitted before `doctor` reports it; `author_name`/`author_email` label them; `destructive_delete` controls whether `fs_delete_file` hard-deletes instead of moving to trash; `restore_exclude` lists paths `git_restore` never overwrites, by default `memory/log`, `memory/sessions`, `memory/coding`, `memory/jobs`, and `memory/events` (the operation log's path and the server-owned `memory/events` and `.contextos/packages` folders are always added automatically, on top of whatever you list here).

### `[vault.events]`

`retention_days` is how many days an `info` event under `memory/events/` is kept before the sweep deletes it, 90 by default; zero is refused at load with `config/invalid-retention`. An event kind can declare its own `retention_days` in its definition, which replaces this for that kind. `keep_attention` (default `true`) keeps a `warning` or `error` event for as long as it is open, by the rules the Signals attention panel uses; once something has cleared it, it ages out by the window counted from its own time, and an event that clears a warning that is still kept is itself kept. Set it to `false` and they age out by the same window whether open or not. The sweep runs when the server starts serving and once a day while it serves, and only ever deletes notes whose `type` is `event`; see [Events](mcp-tools.md#events).

### `[vault.search]`

`text` and `graph` enable the corresponding indexes; `graph_backend` picks the link graph's persistence backend: `sqlite` (the default) in WAL mode, which stays usable when more than one `contextos` process opens the same vault at once; `fjall`, an embedded key-value store, fastest when only one process ever opens the vault, where every later process runs without a link graph; or `serde`, a single JSON file with no cross-process guarantee. Changing the backend migrates nothing: the new store starts empty until `query_index_rebuild` (or `contextos index`) repopulates it. `semantic` enables opt-in vector search. `exclude` scopes the search corpus independently of `index_md.exclude`, with the same default list. `rebuild_budget_seconds` is the default time budget for `query_index_rebuild`'s semantic phase.

### `[vault.links]`

`exclude` names the folders whose notes are not checked as link sources: `doctor`'s `link_integrity` check and `query_graph`'s `unresolved` and `unlinked` operations skip them, and a link from one does not make a note linked. The default is `["templates", "memory/events", "memory/jobs", ".contextos"]`. A configured list replaces the default, and `[vault.search] exclude` is always added to it, since a note outside the search corpus is not in the link graph. Links into these folders still resolve. Each entry must be a plain vault-relative folder, written with forward slashes and no leading or trailing slash, empty segment, `.`, `..`, backslash, or colon; anything else is refused at load with `config/invalid-link-exclude`, naming the entry. An unknown key in the table is refused as in every other table. `vault_info` reports the effective list as `config_summary.links.exclude`.

`ignore_targets` names the folders whose files are not checked as link targets: `link_integrity` and `unresolved` leave out a link when the file it resolves to lies under one, or, when it resolves to no file, when the path it names does. That path is read from the vault root, except a Markdown link's relative path, which is read from its note's folder; a bare name that finds no file names no folder and is still reported. An ignored link is neither listed nor counted as checked, and `link_integrity` says how many it left out. An ambiguous link is still reported with every candidate, wherever they lie, because Obsidian may open either file; give it a path. The default is empty and nothing is added to it. Use it for folders whose contents come and go, such as an inbox or an outbox, and keep `exclude` for folders whose notes hold links that are not meant to resolve, such as templates: a link into `templates/` (a base sidecar's `template` link, say) is still checked. Neither setting changes link resolution, the link graph, `links_read`, `backlinks`, link expansion, or `unlinked`. Entries follow the same rules as `exclude`, and anything else is refused at load with `config/invalid-link-ignore-target`, naming the entry. `vault_info` reports the list as `config_summary.links.ignore_targets`. A server older than this setting refuses a configuration that uses it, so upgrade every server that shares the configuration file before adding it.

### `[vault.search.embedding]`

`provider` is `local` (offline ONNX inference via `fastembed`, no network access; run `contextos model` to manage the shared model cache) or `openai-compatible` (any endpoint speaking the OpenAI embeddings API shape, configured via `model`, `endpoint`, and its API key). `model_directory` points at the local provider's model cache, defaulting to the platform cache location.

Supply the key one of two ways: write it in `api_key`, or name an environment variable holding it in `api_key_env`. Both are first-class: the server runs locally, so anyone who can read this file already has access to it, and many installations cannot set environment variables. Setting both is refused at startup with `config/conflicting-secret`; setting neither with `provider = "openai-compatible"` makes semantic search unavailable for the vault, which `doctor` reports. A key written in the file is never echoed: `vault_info` reports `api_key` as `"<redacted>"`, and `doctor`, logs, and errors never include it.

## `[projection]`

Agent host projection keeps Claude Code's and Codex's user-level agent and skill folders in step with every vault's `registry/agents/` and `registry/skills/` definitions, so a definition written in a vault is available to both hosts without copying. A write, move, or delete of a definition is projected as it completes, and the server compares everything with the vaults when it starts, so edits made in Obsidian or on the filesystem while it was stopped reach the hosts at the next start. A projection failure arrives as a warning on the write that caused it (for example `projection/invalid-definition`), never as a failed write, and a failure at start-up is logged at `WARN` and never stops the server.

Whether projection runs depends on where the configuration came from:

- With no `[projection]` section, a configuration file that exists at the platform default location (`~/.config/contextos/config.toml`) and lists its own vaults projects with the defaults below. With no file there (only `--vault` or environment input), projection is off and no host folder is created. A default-location file that lists no vaults still projects: with no vault there is nothing to project, so entries the server recorded are left in place and `doctor` reports them.
- A configuration named by `--config` or `CONTEXTOS_MCP_CONFIG` does not project unless it sets `enabled = true`. This keeps a test or demonstration configuration from claiming the operator's host folders.
- A run given vaults with `--vault` never projects, whatever the configuration file says (even `enabled = true`). Those vaults are a one-off selection, often a disposable one, while the host folders are shared by every configuration on the machine, so such a run reads, writes, and withdraws nothing there (no host folder, ownership record, or orientation block), and `doctor` reports no projection findings for it. To project a vault, list it in the configuration file.
- Otherwise `enabled` set explicitly, in either case, wins. A `[projection]` section that leaves `enabled` out follows the same rule as no section at all.

Each entry a configuration projects is recorded as owned by that configuration (identified by the digest of its file path), and a configuration leaves entries recorded by another configuration alone. Every server process on the host takes a host-level lock, `projection.lock` in the state root (`~/.contextos`), around each projection, so processes never interleave; a write that cannot get the lock within 5 seconds reports a `projection/busy` warning and changes nothing (the vault write itself always stands), while start-up refresh, `doctor`, and `doctor_resolve` wait up to 30 seconds. If the lock file cannot be opened at all (for example `projection.lock` is a folder), a write reports `projection/lock-failed` and `doctor` reports a failed `projection_lock_failed` check, in both cases without affecting any vault. Ownership records live in `<state root>/projection/`, never inside a host's folders.

A projected file you edit by hand is left alone by a write while its source definition is unchanged, and `doctor` reports it as `projection_hand_edited`. When the source definition changes, the next write replaces the edit and returns one `projection/hand-edited-replaced` warning naming the host and the definition. Start-up refresh and `doctor_resolve` replace a hand edit. An entry recorded as another configuration's is never changed and is reported as `projection_foreign_owner` whether or not this configuration defines the same name; the message names the other configuration by the first 12 hexadecimal characters of its identity, and `doctor`'s `configuration` check shows this configuration's own 12 characters when projection is on, so the two can be matched. An `index.md` inside a skill folder, at any depth, is the vault's own folder index and is not part of the skill: it is never projected, counted, or sized. For `doctor`, an installed host whose `agents` or `skills` folder does not exist has only that folder's entries left unassessed; the host's other folder and its orientation block are still assessed, and nothing is created.

| Key | Purpose |
| --- | --- |
| `enabled` | Turns projection on or off, overriding the rule above |
| `[projection.claude]` | Claude Code. `enabled` (default `true`) turns this host off alone |
| `[projection.codex]` | Codex. `enabled` (default `true`) turns this host off alone |

Each host table takes the same three targets, and Claude Code takes one more key:

| Key | Claude Code default | Codex default |
| --- | --- | --- |
| `agents` | `~/.claude/agents` | `~/.codex/agents` |
| `skills` | `~/.claude/skills` | `~/.agents/skills` |
| `instructions` | `~/.claude/CLAUDE.md` | `~/.codex/AGENTS.md` |
| `mcp_server` | `contextos` | (not applicable) |

`mcp_server` is the name ContextOS is registered under in Claude Code. A definition with `access: read-only` is projected to Claude Code with an allowlist of Claude's own read-only tools plus every ContextOS tool that does not change a vault, written as `mcp__<mcp_server>__<tool>`. The list is the server's own classification of its tools, so it follows the tool catalogue.

An agent definition's frontmatter needs a `description` and a `tier`, and takes an optional `access`. `tier` is `fast`, `balanced`, or `deep` (see below). `access` is `all` (the default when it is left out) or `read-only`. A definition must not carry `model` or `tools`: each host chooses its own model and tool names, so a definition naming either is refused with `projection/invalid-definition`, naming the field.

`access: read-only` is best effort, not a guarantee. MCP tools do not declare whether they change state, so the server can classify only its own ContextOS tools (and treats `query_index_rebuild`, which rebuilds derived stores and can move a damaged one aside, as mutating, so a read-only agent is not allowed it); third-party MCP tools are left out of a read-only Claude Code allowlist rather than guessed at. Codex can only switch off shell, apps, and plugins for a read-only agent, and its MCP tools stay available, so the restriction is only partial there and `doctor` reports `codex_read_only_unenforced`. `access` is expected to become a list of allowed tools with `*` wildcards in a later release, alongside `all` and `read-only`.

A definition names a tier (`fast`, `balanced`, or `deep`), never a vendor model, and `[projection.claude.tiers]` and `[projection.codex.tiers.<tier>]` say what each tier becomes on each host:

| Tier | Claude Code `model` | Codex reasoning effort | Codex model |
| --- | --- | --- | --- |
| `fast` | `haiku` | `low` | Codex's own default |
| `balanced` | `sonnet` | `medium` | Codex's own default |
| `deep` | `opus` | `high` | Codex's own default |

Claude Code tiers are plain model names (`fast = "haiku"`). A Codex tier is a table with an optional `model` and an optional `effort`, which may be any effort Codex documents: `minimal`, `low`, `medium`, `high`, `xhigh`, or `max`.

### Orientation block

The same projection keeps one shared block of instructions in each host's global instructions file. The source is `registry/instructions/global.md` in the first vault, in `[[vault]]` order, that has one. Its content is copied between a begin and an end marker line, `contextos:orientation:begin` and `contextos:orientation:end` (written as HTML comments), into `~/.claude/CLAUDE.md` and `~/.codex/AGENTS.md` (or the configured `instructions` files). Only the lines between the markers are ever changed: everything you wrote around the block stays exactly as it is, and when the file has no block yet it is added at the end. A file whose markers do not form exactly one block, and a source that itself holds a marker line, are reported by `doctor` and left unchanged.

Codex ignores `AGENTS.md` when an `AGENTS.override.md` sits beside it. The server detects such a file, reports `codex_override_hides_agents`, and never writes it. The instructions file is read again immediately before it is replaced, so an edit made by hand or by another tool while the server was working is kept, and the replacement is retried against the new content.

Targets are checked when the configuration loads. `~` expands from the home directory, a target must be absolute afterwards, and a target inside any configured vault root is refused with `projection/target-in-vault`; choose a folder outside every vault. The reverse is refused too: a vault root that lies inside, or is the same as, an `agents` or `skills` target folder is refused with `projection/vault-in-target`, because projecting a skill whose name matches the vault's folder would replace the vault. A vault inside the folder that merely holds a host's `instructions` file (`~/.codex/notes`, say) is accepted, because nothing else is written there; the instructions file itself inside a vault is still refused with `projection/target-in-vault`. Both checks resolve links, `..`, and folders that do not exist yet, with the one resolution that vault roots use, so any spelling of a location (a link, a different case on Windows, or a `\\?\` verbatim form) compares as the same location and a component-wise comparison never mistakes `agents-extra` for something inside `agents`. The state root (`~/.contextos`) is protected the same way: a target inside it is refused with `projection/target-in-state-root`, and a target folder that contains it (`~` as a `skills` folder, say) is refused with `projection/state-root-in-target`. No two targets may share, nest in, or contain each other's folder, and an `instructions` file may not be a target folder; such a pair is refused with `projection/targets-overlap`, which names both settings, so give every host and every setting its own folder. A relative target is refused with `projection/target-not-absolute`. An `agents` or `skills` folder that is itself a symbolic link, dangling or not, stops the configuration loading with `projection/linked-folder`, so the server does not start: the message names every such folder and the target its link points to, and says to switch projection off with `[projection] enabled = false` or `enabled = false` in that host's table. `contextos doctor` and the `doctor` tool do not stop for it: they report the same message as a failed `configuration` check and still run every vault check. Links in the folders above a target are followed, and an `instructions` file that is a link is written through (it must resolve to a regular file; a link to a pipe, socket, or device is refused and never opened). Any unknown key under `[projection]` is a configuration error, as elsewhere in the file. A target is also re-checked each time the server is about to write through it. A link you made yourself (a dotfiles repository, say) is followed once, when the server starts, but if the configured path later resolves somewhere else, nothing is written through it and a `projection/path-rejected` warning asks you to restore the link or restart the server. Entries already in a target are read only within the same limits a definition has in a vault (512 files, 8 levels of folders, and 32 MiB per entry, checked from the directory listing's file sizes before any file is read); a larger entry in a target is refused with `projection/path-rejected` and left alone, and a vault skill beyond the same limits is refused with `projection/invalid-definition`. On Windows, an entry that is a reparse point but not a link, such as a OneDrive cloud placeholder, is refused with `projection/reparse-point`: make it available offline or move it. A host counts as installed only when its home folder exists, meaning the folder holding its `instructions` file (`~/.claude` for Claude Code and `~/.codex` for Codex by default). The server never creates a host's home folder: a host that is not installed is skipped with an `INFO` log line naming only the host, and when no host is installed projection is off and nothing is created. For an installed host the server creates its missing `agents` and `skills` folders (for Codex, `~/.agents/skills`, the user skills location Codex documents). If a folder cannot be prepared (for example a file is in the way, or the home directory is read-only), that host is skipped with a warning in the log and the server still starts.

Outside the vault roots, the server writes only the configured projection targets (an entry per definition inside each `agents` and `skills` folder, and each `instructions` file) and its own state root. It follows no link below a target folder, replaces every file and skill folder atomically, and never changes an entry it did not project.

Which vault wins when several define the same agent or skill is decided by the order of the `[[vault]]` entries: the first vault that defines a name wins, for projection and for the plugin's lookups. `vault_info` reports the vaults in that order.

## Environment overrides

| Variable | Overrides |
| --- | --- |
| `CONTEXTOS_MCP_CONFIG` | The configuration file path |
| `CONTEXTOS_MCP_TOKEN` | `[server.http].token` or `token_env`, whichever the file uses |
| `CONTEXTOS_MCP_LOG_LEVEL` | `[server].log_level` |
