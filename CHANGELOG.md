# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html) once a first public release is tagged.

Entries summarise each change; `docs/` holds the detail of current behaviour. The record starts at 0.20.1, and every user-visible change belongs in `[Unreleased]` until it ships in a tagged release.

## [Unreleased]

## [0.23.8] - 2026-11-06

### Fixed

- The shared workflows and library pages have a title and tabs that lead back to each vault. They had no way back.
- The shared and settings pages show the sidebar of the vault you were last in, with a band saying which view is showing and which vault the menu belongs to. Their sidebars were empty, and the vault switcher named a vault they were not in.
- The alerts bell's count shows. Its request never ran, because the badge inherited a target from the panel around it.
- The footer status bar wraps on narrow screens rather than running off the side of every page.
- Wiki, Insights, and entity pages appear under Recently accessed. Recording the last-visited page replaced that cookie on the same response.
- A job's Status panel lays out its heading, fields, and actions in order, rather than in one row.
- A launcher saved under another name, such as a browser's `contextos-launcher (1)`, still installs and updates. It used to report that ContextOS was not installed.

### Changed

- Release jobs and the CI test jobs have time limits, so a hung job fails long before GitHub's six-hour default.
- Everything the server holds for all its vaults now lives under `/server`, leaving `shared` and `settings` free as vault names. Shared workflows move from `/shared` to `/server/workflows`, library releases to `/server/packages`, settings from `/settings/` to `/server/settings/`, and the host API from `/api/v1/shared` to `/api/v1/server`. The old addresses do not redirect.
- Every page's sidebar has a permanent Server section below Knowledge, with links to the shared Workflows, Jobs, and Libraries, and a new `/server/jobs` page lists every shared job.
- The OAuth callback moves to `/server/settings/oauth/{server}/callback`. A provider where you registered the old callback by hand needs the new address before you next sign in to it.
- `contextos` refuses to start with a vault named `api`, `mcp`, `server`, `shell`, or `static`, and `contextos config` refuses to add one, because the web interface uses those addresses itself. A vault with no name of its own takes its folder's name, so a folder with one of these names needs a `name` in the configuration.
- The workflows lists show a strip of job counts by state, which also filters the list, in place of plain links.
- The workflow, job, and library screens now follow the rest of the interface, with tidier job cards, an actions row, and styled forms and tables.

## [0.23.7] - 2026-11-06

### Added

- `contextos-launcher uninstall` removes services, the Claude Desktop registration, the commands, the `PATH` changes, and the installation, keeping vaults and settings.

### Changed

- Installing, updating, and rolling back move from `contextos` to `contextos-launcher`, so no one program owns the suite. `contextos-launcher install` needs no version, and sets up the commands and `PATH` itself, so installing is one command.
- `contextos-launcher update` also replaces the launcher copies, so fixes to the updater reach existing installations.
- `contextos --version` and `contextos-web --version` print `ContextOS 0.23.7` and `ContextOS Web 0.23.7`, in place of the lowercase names and a `v` before the version.
- Release tags no longer start with a `v`, as in `0.23.7`. Earlier releases keep their tags.
- A release now starts as a prerelease that no client can reach. It becomes the latest release only after a real install from it passes on Linux, Windows, and macOS.
- Release archives carry a README for people running the programs: installing them, setting up, connecting an assistant, and updating.
- The repository README and installation guide cover the web interface, apps, workflows, the launcher, release downloads, and the ContextOS plugin.

### Fixed

- Services and MCP host registrations created through the launcher record the launcher, so they follow updates and rollbacks rather than staying on a versioned folder that cleanup later deletes. On Linux and macOS the launcher now becomes the program it starts, so a service stop reaches the server and it shuts down cleanly.
- A retired successor job can no longer be re-activated once its predecessor has another successor. It was only stopped at its next validation.
- Installing and updating work. In 0.23.6 they failed on every platform: the updater could not read a version with a `v` in front of it, and the Linux and macOS archives held `release.json` inside a folder rather than at the top.

## [0.23.6] - 2026-11-05

### Upgrade notes

- **Job requirements:** every job file needs `timeout_secs`, and a vault job needs an `owner` who is a team member, in a vault whose `.contextos/team.toml` names exactly one chief of staff. The first scheduler pass after upgrading suspends enabled jobs that fall short (vault jobs raise a task, shared jobs do not), and a vault with saved jobs and no valid team has its scheduled jobs paused.
- **Job identity:** a job is now `{package}/{workflow}/{job}` in vaults and on the host, across pages, commands, claims, run records, and state files. Nothing is migrated, and the old paths and keys are not read.
- **Package commands and API:** `packages install` takes `<vault> <bundle>` or `--shared <bundle>`, the host API moved from `/api/v1/packages` to `/api/v1/shared/packages`, and `?force=true` became `PUT .../execute/remove`. `workflows run --shared --package-version` became `--release`, and a vault `workflows run` now runs the `current` release, with `--source` for the source. The old forms are refused.
- **Retention:** the host package store no longer keeps every installed release, so installing a newer release can remove an older one that nothing keeps.
- **Link graph:** `[vault.search] graph_backend` defaults to `sqlite` and nothing is migrated. Run `query_index_rebuild` or `contextos index` to populate it, or set `graph_backend = "fjall"` to keep an existing store. The graph store format also changed, so an existing graph is rebuilt on first open.
- **Configuration:** `[server] resource_link_threshold_kb = 0` is now refused; set it to at least 1.
- **Base sidecars:** any `template` now binds, and a single one binds every series. Give a sidecar whose `template` serves series of different kinds a `series_templates` entry for each before upgrading.
- **Journal and tasks:** the Journal no longer names fixed sections, so a vault relying on Loops and Claude session log sets `task_opened_section`, `task_closed_section`, and `read_only_sections`. `pending` is no longer a shipped task stage, and a task left in it is not counted as open until moved.
- **Tool results:** `links_read` returns `unresolved` entries as objects, a file given where a folder is needed fails with `path/not-a-directory` in place of `io/filesystem`, and the `base_property` doctor check is replaced by `record_conformance`.
- **Extension modules:** `ServerModule::handle` returns rmcp's `CallToolResponse`; an existing module returns its `CallToolResult` with `.into()`.

### Added

- **Workflow releases:** `workflows install` and `libraries install` turn a vault's source into versioned releases, each tracked as current, previous, future, or retired. Versions step by interface change, and an install with too small a step is refused. Runs can target a release or the source (`workflows run --release`, `--source`), and run records name the release and libraries they used.
- **Job chains:** `jobs successor` moves a job to another release through a successor, with one job in use per chain, and a job edited away from its record is held to it.
- **Package management:** `packages list`, `inspect`, `remove`, and `install` work on vault and host stores with matching API routes, including installing from a plain folder and rebuilding a lost ledger. Automatic retention removes releases nothing keeps.
- **Web release pages:** workflow, job, and library pages show releases, chains, checks, and dependants, and Vault Health reports damaged releases and store problems.
- **Run summaries:** every run command ends with one summary, as text or versioned JSON, with a graded exit status.
- **Workflow scripting:** typed outputs (`set_output`), declared modules and built-in libraries, read-only access to shipped files (`workflow_file`), and `--input` overrides on runs.
- **Job authoring:** `jobs create`, `show`, `replace`, and `list --shared` author shared job files from the command line, and job files may declare an `entity`.
- **Job supervision:** the scheduler raises a task when a vault job needs attention and records `job.failed`, `job.suspended`, `job.recovered`, and `job.retired` events. Suspensions are structured records, and `.contextos/team.toml` is validated strictly for jobs.
- **Agent host projection:** agents, skills, and global instructions in a vault's `registry/` are projected into Claude Code and Codex, configured by `[projection]`, with ownership tracking, confinement checks, and `doctor` reporting and repair.
- **Link integrity:** `query_graph` reports unresolved links and unlinked notes, `links_read` takes `direction`, `doctor` gains `link_integrity`, and `[vault.links] exclude` scopes the check.
- **Partial reads:** `fs_read_text_file` reads byte parts by `offset` and `length`, and reads report `size` and `version` so a caller can detect a file changing between parts.
- **Note and record tools:** `note_update`, `note_create` with `content_from`, `base_query` lookups by `ids` plus `containsAny` and `containsAll`, `base_record_create` with `unique_fields` and series templates, `base_read`'s `records_folder`, and `frontmatter_read` with `expand_links`. `expand_links` also handles canvases and Markdown reference definitions.
- **Web Signals:** a Signals screen, a live event stream over Server-Sent Events, and task and agent status driven by vault events.
- **Web vault management:** a Decisions register, Registry, Standards, Templates, and Activity log in Vault Settings, plus task types, priorities, and per-type stages in `.contextos/tasks.toml`.
- **Web connections:** `/settings/` shows each MCP server's connection with a **Reconnect** action, with no restart needed.
- **`contextos-markdown`:** a new pure crate rendering notes for `contextos-web`, covering Obsidian syntax, math, Mermaid, fences, and allowlisted HTML.
- **Doctor checks:** derived store health, record conformance, fence vocabulary, and team configuration. Every auto-fixable finding names a stable `remediation`.
- **Store recovery:** a damaged derived store is quarantined, recreated, and repopulated automatically.
- **Time zone:** `[server] timezone` sets the zone for every time written into a vault, reported by `vault_info`.
- **Logging and service:** `contextos` gains log rotation and retention through the new `contextos-logging` crate, and `contextos-web service` gains `start`, `stop`, and `restart`.

### Changed

- **MCP protocol:** the SDK moves to rmcp 3.5.1. `contextos` serves revision 2026-07-28 over stdio and streamable HTTP, and `contextos-web` opens sessions with `server/discover`, falling back to `initialize` for older servers.
- **Link resolution:** the link graph resolves links as Obsidian does, with one resolver shared with `expand_links`. Markdown links are graph edges, and a shared name is reported as ambiguous.
- **Concurrency:** mutations are exclusive across processes through a fair per-vault lock, failing with `io/vault-busy` after 30 seconds. Derived stores are shared between instances, and file locks use the standard library in place of `fs4`.
- **Stored text:** every time written into a vault is local rather than UTC, cron schedules run on wall-clock time, and every text write is stored with `\n` line endings, warning when it converts them.
- **Identity scope:** ids are unique within a base's scope, and a record's `base` property resolves like any other link.
- **Workflow safety:** scripts refuse `eval`, script output goes to the run's log, run messages no longer name absolute host paths or quote rejected `--input` values, and vault runs honour interrupts as shared runs do.
- **Scheduling:** only the latest of several missed cron ticks runs.
- **Web rendering:** one pipeline renders every note, each file opens on the screen that owns it, and the wiki, insights, and entity screens drop the `knowledge/` prefix from their URLs.
- **Web tasks and journal:** each task type has its own lifecycle, colours, and stages, with `review` and a Done today tile added. Journal tabs come from the day's entry, each section edited on its own.
- **Web resilience:** `contextos-web` starts whether or not its MCP servers connect, and saved `[[mcp_server]]` changes apply at once.
- **Windows service:** `service install` registers a per-user Startup App needing no Administrator terminal, with `--scheduled-task` as the alternative, and a process record lets `service` commands act on the right process.
- **HTTP limits:** `max_body_kb` is the one request limit, defaulting to 14 MiB so the largest permitted write fits, and a non-loopback bind serves clients reaching it by network address.
- **Status reporting:** `query_index_status` and `vault_info` report health for every store and never fail because one store cannot report.
- **Releases:** `contextos update` defaults to `github.com/zentelligence/contextos`, release archives carry `NOTICE` and `THIRD_PARTY_LICENSES.md`, and `just ci` fails on stale licence attribution.

### Removed

- The `PATCH /{vault}/journal/sections/{section_key}` route, replaced by per-index section routes.
- The startup diagnostic that wrote every invocation's arguments and environment to `contextos-startup-debug.log`.

### Fixed

- `doctor --resolve` and its `--dry-run` now write only what they report, take the vault write locks, report failed remediations as failed, and survive a vault whose search state cannot open. `doctor` no longer creates stores or misreports a repository with no commits.
- On Windows, a write no longer fails with access denied when a reader that allows deletion has the file open, as most modern programs do. This could also leave a workflow run's record stuck at `running`. A program that forbids deletion, as C's `fopen` does, still refuses the write, and the file is left whole.
- A vault write keeps the permissions of the file it replaces, and a new file takes the usual default. Every write used to leave the file private to its owner on Unix, dropping shared access and executable bits. Each write also now flushes its folder entry.
- Named pipes in a vault can no longer hang a server thread on Unix.
- Installing a package with files in folders no longer fails, on any platform, and `contextos-web service` no longer fails on Windows where nothing is registered yet.
- OAuth2 tokens now refresh for open sessions and workflow runs.
- Rebuilds and indexing can no longer overwrite a concurrent write's newer entry.
- Size limits are corrected: 10 MiB binary writes succeed, and Inbox uploads and the MCP proxy accept their documented limits.
- `query_index_rebuild` no longer loses its result over streamable HTTP when a request carries a progress token.
- Notes with a byte order mark, code fences in lists or callouts, or `%%` in frontmatter are read correctly.
- `[server].log_file` is now used, Journal saves preserve the entry's text and detect newer versions, and task priorities sort in their declared order.
- `contextos-web` commands close their MCP sessions before exiting, and a server outage no longer counts as an unreadable workflow file.

### Security

- Package names are validated before reaching the package store (`packages/invalid-name`), and bundles holding links, special files, or unsafe paths are refused.
- Files `contextos-web` serves carry `X-Content-Type-Options: nosniff`, and SVG, HTML, and XML files a sandboxing `Content-Security-Policy`.
- The text index moves to tantivy 0.27, clearing RUSTSEC-2026-0391, RUSTSEC-2026-0398, RUSTSEC-2026-0403, and RUSTSEC-2026-0253 with no index rebuild.

## [0.21.0] - 2026-09-05

### Added

- `contextos-web`, a second binary providing the web interface and an MCP proxy, with `service install`, `uninstall`, and `status` for per-user background running on Linux, macOS, and Windows.
- `base_query` gains `file.inFolder(folder)`, property formulas, and `file.asLink(display?)`.

### Changed

- The `contextos-server` crate is renamed `contextos-mcp`; the `contextos` binary and CLI are unchanged.
- `contextos-web`'s `--web-config` flag is now `--config`, and `web.toml`'s `[server] static_dir` is optional because static assets are embedded.

### Fixed

- `/static/` no longer 404s when launched away from a `static/` folder.
- `base_query` no longer drops matches by narrowing a scan through `or` and `not` filters.
- The Base card view no longer offers formula columns for editing, which wrote bogus frontmatter.
- `contextos-web` page rendering runs independent MCP calls concurrently and keeps blocking reads off the async executor.

## [0.20.2] - 2026-09-03

### Fixed

- `contextos config` loads an existing `config.toml` rather than treating every run as a fresh install.
- `Config::validate` rejects an empty transports list.
- `fs_write_file` separates an omitted `expected_hash` from a genuine conflict, and a conflict returns the current hash.

### Removed

- The unreleased `embedvec` evaluation prototype.

## [0.20.1] - 2026-08-30

### Added

- Filesystem parity tools: read, write, edit, list, tree, search, move, inspect, attach, and guarded delete within configured vault roots.
- Substrate services: managed `index.md` maintenance, append-only operation logging, and local Git recovery.
- Obsidian-aware operations: structured notes, frontmatter, Bases, Canvas, and wikilink handling with validation.
- Query layer: ranked text search, link-graph traversal, and opt-in local semantic search, exposed over MCP stdio and authenticated streamable HTTP.
- Extension API (`ServerModule`) for feature-gated, namespaced tool modules built on the same secured core services.
- Mermaid diagram parsing, layout, and SVG rendering support.
- Generalised resource capability: any-file `resources/read`, `hidden`-aware directory enumeration, and `resource_link` attachment for oversized results.
- Doctor support: `doctor` and `doctor_resolve` MCP tools, `--resolve`/`--dry-run` CLI flags, and a vault-wide frontmatter validity check.
