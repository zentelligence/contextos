# Running workflows from the command line

`contextos-web` runs a workflow from the command line in two ways: a saved job (`jobs run`) or one workflow once with no saved job (`workflows run`). Each works for a workflow registered in a vault and for a workflow installed in the host package store, which this document calls a shared workflow. Every command connects its own transient MCP sessions using `web.toml`, and the primary server is the first `[[mcp_server]]` entry.

| Run | Vault-registered | Shared |
| --- | --- | --- |
| A saved job | `contextos-web jobs run <vault> <package> <workflow-slug> <job-slug>` | `contextos-web jobs run --shared <package> <workflow-slug> <job-slug>` |
| One workflow once | `contextos-web workflows run <vault> <package> <workflow-slug> [--release <release> \| --source] --input name=value` | `contextos-web workflows run --shared <package> <workflow-slug> [--release <release>] --input name=value` |

Every run command ends with one [run summary](#the-run-summary) on standard output, as labelled text or, with `--format json`, as one versioned JSON document, and a graded exit status. Nothing else is written to standard output, and the commands write nothing of their own to standard error: a script's `print` and `debug`, progress, and the failure itself go to the log (see [Logging and warnings](#logging-and-warnings)). Callers, including agents, should use `--format json` and read the summary's fields, never the text.

## The run summary

`jobs run`, `jobs dry-run`, and `workflows run` take `--format text` (the default) or `--format json`. The summary is built from what the run reported, never from message text, and its exit status is derived from the summary, so the two cannot disagree.

**JSON.** One object, written once after the run has ended and followed by a newline. Every field is always present, with `null` where it does not apply, so a reader never tests for absence. A run summary is told apart from the `{"error": {...}}` document the authoring commands print by `summary_version`, which only a run summary has.

```json
{
  "summary_version": 1,
  "command": "jobs run",
  "scope": { "kind": "shared", "vault": null, "package": "acme", "package_version": "1.2.0", "digest": "sha256:...", "workflow": "sync", "job": "nightly" },
  "dry_run": false,
  "scenario": null,
  "outcome": "succeeded",
  "exit_code": 0,
  "code": null,
  "message": null,
  "owner": null,
  "overridden_inputs": ["target"],
  "run": { "id": "R-12", "record": "runs/acme/sync/nightly/2026/10/R-12.md" },
  "registration": null,
  "outputs": { "created": 3 },
  "libraries": [{ "name": "crm", "origin": "release", "version": "1.4.0", "digest": "sha256:..." }],
  "warnings": { "kept": [], "omitted": 0 },
  "host_warnings": [],
  "timings": { "started": "2026-10-21T18:00:00+11:00", "ended": "2026-10-21T18:00:03.214+11:00", "duration_ms": 3214 }
}
```

| Field | Meaning |
| --- | --- |
| `command` | `jobs run`, `jobs dry-run`, or `workflows run`. `jobs run --dry-run` reports `jobs run` with `dry_run: true` |
| `scope` | `kind` is `vault` or `shared`. A vault scope names `vault`, `package`, `workflow`, and `job` (`null` for `workflows run`). A shared scope names `package`, `workflow`, and `job`. Either scope carries `package_version` and `digest` of the release the run read, once it had one (`null` for a run of source) |
| `scenario` | The scenario a dry run used, including the sole one chosen when none was named |
| `outcome` | `succeeded`, `degraded` (succeeded with warnings, the run record's word), `failed`, `cancelled`, or `refused`. `refused` means the script never started, decided by what the run reports and never by its code |
| `exit_code` | The process exit status |
| `code`, `message` | Both `null` for `succeeded` and `degraded`. Otherwise a stable `workflow/` code and a message, never repeating the code, cut at 1 KiB. A cancelled run has `workflow/cancelled` and the message `the run was cancelled; calls it had already made were not undone`, in text and JSON alike |
| `owner` | The job's owner (`owner` in a vault job's file, a member of the vault's `.contextos/team.toml`, or `[owner]` in `web.toml` for a shared job) when the outcome is `failed`, or `refused` for a saved shared job, so the reader knows whom to ask. It is no longer appended to the message |
| `overridden_inputs` | The names given with `--input`, in declaration order. Never their values |
| `run` | `null` when no run number was taken, as for a one-off run. Otherwise `id` (`R-<n>`) and `record`: `{vault}://memory/jobs/{package}/{workflow}/{job}/{YYYY}/{MM}/R-{n}.md` for a vault job, `runs/{package}/{workflow}/{job}/{YYYY}/{MM}/R-{n}.md` relative to the host app-data folder for a shared job, or `null` when no record was written (`--no-record`, or it could not be written) |
| `registration` | For a dry run of a saved job, `{"registered": true, "reason": null}` or `{"registered": false, "reason": ...}` with one of `dry_run_not_completed`, `scenario_calls_unused`, `not_validated`, `version_changed`, `inputs_overridden`, or `state_unavailable`. `null` for a live run and a one-off run |
| `outputs` | The declared outputs the script set, in declaration order, for `succeeded` and `degraded` only. `null` otherwise |
| `libraries` | The libraries the run used, in name order, each as `name`, `origin`, `version`, and `digest`. `origin` is `release` for an installed library release, with its `version` and `digest`: the release's lock for a run of a saved job or of a release that has one, and the releases chosen from those installed for a one-off run of a release with no lock or, for a library the source names with a requirement, of source. It is `source` for a library a run of source read from its own source in `registry/libraries/`, with `version` and `digest` both `null`. `[]` when there were none. Text adds one `Library: {name} {version}` line each, or `Library: {name} (source)` |
| `warnings` | The script's reasons it is degraded: `kept` (at most 20, each at most 512 bytes) and `omitted` (a count) |
| `host_warnings` | `{code, message}` for things the host could not do that do not change the outcome: `workflow/record-unavailable` (the record could not be written or finished; a record that could not be started yields one warning, and a dry run record that was created but could not be finished keeps its location in `run.record` and says it "could not be finished") and `workflow/state-unavailable` (the job's registration after a dry run, or the settling of a first run's trial, could not be saved, for vault and shared jobs alike) |
| `timings` | `started` and `ended` as RFC 3339 in the host's zone, and `duration_ms`. A refusal is timed in the same zone as a run that finished, once the host's zone is known; a refusal before the servers are connected, such as an unusable `web.toml`, is timed in UTC |

A refusal is a summary with `outcome: "refused"`. A failure before the command knows anything about the run, such as an unusable `web.toml`, is still a summary, with its scope built from the arguments as given. No field, and no message, holds an absolute host path: a record is named in the forms above, and a claim, state file, or manifest is named by the job or by its place under the app-data folder.

**Text.** The default form is a fixed order of `Label: value` lines, each present only when it applies: the headline (`Run succeeded.`, `Run succeeded with warnings.`, `Run failed.`, `Run cancelled. Calls it had already made were not undone.` (and a `Message:` line saying the same), or `Run refused before it started.`, with `Dry run` in place of `Run` for a dry run), `Code`, `Message`, `Owner`, the `Job` or `Workflow` line, `Scenario`, `Inputs overridden`, `Run`, `Record`, `Registration`, one `Output <name>: <compact JSON>` line for each output, `Warning` lines (then `Warning: and <n> more not shown`), `Host warning` lines, and `Duration`.

```text
Dry run succeeded with warnings.
Job: nightly (workflow sync, package acme 1.2.0)
Scenario: normal (call results were mocked, so nothing was written)
Run: R-13
Record: runs/acme/sync/nightly/2026/10/R-13.md
Registration: registered
Output created: 3
Warning: one source note was unreadable
Duration: 0.412 s
```

**Exit status.**

| Ending | `outcome` | Exit |
| --- | --- | --- |
| Completed, with or without warnings | `succeeded` or `degraded` | 0 |
| Failed (any code but `workflow/timed-out`) | `failed` | 1 |
| Command line not understood | none, and no summary | 2 |
| Refused before the script started (any code but `workflow/job-busy`) | `refused` | 3 |
| Refused because the job's claim is held | `refused`, `workflow/job-busy` | 75 |
| Timed out | `failed`, `workflow/timed-out` | 124 |
| Cancelled by a signal or a cancel request | `cancelled`, `workflow/cancelled` | 130 |
| A second signal | none, and no summary | 130 |

A usage error is always clap's own: `--scenario` needs `--dry-run`, `--release` takes `current`, `future`, `previous`, or an exact version, `--source` cannot be combined with `--release` or `--shared`, `--no-record` cannot be combined with `--shared` (a shared run always leaves a record), and `--format` takes `text` or `json`. A run that ends with a non-zero status logs one `error` event carrying its outcome, code, and exit status, and nothing else: the failure message can quote the job's data, so it is logged at `debug` only. The event reaches standard error or `[server] log_file` through the configured subscriber. A run whose `web.toml` cannot be used, or for which no `web.toml` path can be determined (no `--config`, no `CONTEXTOS_WEB_CONFIG`, and no home folder), is a `refused` summary with `workflow/config-invalid` and exit 3, and logs through the default standard error subscriber instead. A task of the host that is lost after a shared run has started is a `failed` summary with `workflow/internal-error` and exit 1, since the run had begun, while one lost before the claim is taken (loading the job, checking that it can run, loading the scenario, taking the claim, or preparing a one-off run) is a `refused` summary with the same code and exit 3, since nothing had begun. The signal handlers are installed before `web.toml` is read, so a signal that arrives while the configuration is being read or the servers connected still ends the run as `cancelled` with exit 130; handlers that cannot be installed are logged once logging is installed, and the run goes ahead without them. If the summary cannot be rendered as JSON, a fallback document with every field of the summary is written instead, `null` where it knows nothing. A cancel before the script starts, during connection or claiming, is still `cancelled`.

## Inputs

A workflow declares its inputs and a run supplies their values. A workflow declares them in its `workflow.toml`.

```toml
# workflow.toml
[[input]]
name     = "since"
kind     = "date"
required = false
```

The kinds are `string`, `number`, `bool`, `null`, `date`, `time`, `datetime`, `timestamp`, `vault`, `array` (with `items`), and `object` (with `properties`). An input is required unless it declares `required = false`, and no default is ever applied. A workflow registered in a vault cannot declare a `vault` input, because its scope is that vault alone.

A saved job supplies values in an `[inputs]` table. `--input name=value` supplies one value and is repeated once per value. A value is plain text for a `string` or `vault` input and JSON for every other kind, so `--input count=3` is a number and `--input tags='["a","b"]'` is an array. An undeclared, missing, or invalid value ends the run with `workflow/input-invalid` before any MCP call. The script reads the values from a read-only `inputs` map.

### Overriding a saved job's inputs

`jobs run` and `jobs dry-run`, for vault and shared jobs and live and dry alike, take `--input name=value`, repeated once per value, parsed exactly as for `workflows run`: text for a `string` or `vault` input and JSON for every other kind. The value replaces the job's own value for that name whole, an optional input the job omits may be supplied, and nothing can be removed. The merged values are validated as the job's own are, and for a shared job each `vault` name is checked against the primary server's vault list. A missing `=` (the refusal names the argument by its position, such as `input argument at position 2 has no =`, and never repeats it, since a mistyped secret is still a secret), an undeclared name, a repeated name, or an invalid value refuses the run with `workflow/input-invalid` before any script step and before any record. For a vault job the claim taken to find the job is released, and the run number it took is not reused, which leaves a gap in the numbers.

Run records and the summary name the overridden inputs and never their values. An override run is for an ad hoc run only: a dry run with overrides never registers the job (`registration.reason` is `inputs_overridden`), and a live run with overrides never settles the first-run trial, because the job's version covers its own values and a run with other values says nothing about the version that was validated. A job that is always run with overrides is registered by a plain `jobs dry-run`, and its trial ends on a run without overrides.

### A refusal never changes a job

A first-run trial is settled only by a live run whose script started. Every refusal (outcome `refused`, the script never started) leaves the trial pending and the job's lifecycle unchanged, for vault and shared jobs alike, whether the cause is a disconnected primary server, a missing or uninstalled release, a vault that is gone, or a task lost before the claim: an outage or a missing release is never judged a defect of the job. This includes a vault live run that fails after its record was created but before its script started, such as an unreadable entry script: its record says `failed`, the trial stays pending, and the job stays enabled. The only side effects of a refusal are a vault override refusal using up a run number, and a vault run refused after its record was created leaving a `failed` record. A job that is not on trial has nothing to settle, so a live run of it never warns that the job's state is unavailable.

### Scheduled and shared runs without a record

A scheduled vault run whose record cannot be created is not run: a vault run's record is its visibility in the vault, so the tick is recorded as fired, one warning is logged naming no folder, and the next tick tries again. A shared run, by hand or scheduled, whose record cannot be started goes ahead without one and reports one `workflow/record-unavailable` host warning, because the host's record is a convenience beside the vault and not the only trace of the run.

## Outputs

A workflow declares typed outputs, and its script sets them with `set_output(name, value)`. It declares `[[output]]` tables, matching its own `[[input]]` tables, with the same fields (`name`, `kind`, `required`, `items`, `properties`). Names are unique within a workflow, and a workflow run from a vault may not declare a `vault`-kinded output.

```toml
# workflow.toml
[[output]]
name = "created"
kind = "number"
```

`set_output` converts the Rhai value as `call_mcp` arguments are converted, checks the name is declared, checks the JSON form is at most 64 KiB, and validates and normalises the value against its declared kind (a datetime with `+00:00` comes back with `Z`). The last call for a name wins. A refusal is an error the script can catch, beginning with `workflow/output-invalid` and naming the output and the kind it expects, never the value, and an uncaught one fails the run with that code. When a script completes, each required output it did not set fails the run with `workflow/output-invalid`, naming them all. A dry run applies the same rules.

Outputs reach the summary only for `succeeded` and `degraded` runs, so a failed run's partial values are never acted on as if they were complete. A record lists the names that were set (`outputs`) and never the values, and neither does the operation log or any log line. Whatever the script prints, standard output carries nothing it controls except these declared, validated values.

## What a run may reach

Every MCP call a script makes passes a check before it is sent, and a refused call is never issued.

- **Servers and tools.** A workflow declares what a run of it may call in `workflow.toml`, in the same four lists wherever it runs. Anything the lists do not permit fails with `workflow/effect-denied`, naming the list that refused it, and is never issued.
- **Vaults.** A call to the primary server may name only the vaults the run is scoped to: its own vault for a vault-registered workflow, and the vaults given in its `vault` inputs for a shared one. A string is treated as naming a vault when the text before its first `://` matches a configured vault, without regard to letter case, at any depth in the arguments. Naming any other configured vault fails with `workflow/vault-out-of-scope`.
- **Vault inputs.** Each `vault` input must name a vault the primary server reports, otherwise the run ends with `workflow/vault-unavailable`. A vault removed or renamed during a run also fails with that code.

### Declaring what a run may call

```toml
# workflow.toml
[mcp_servers]
allow = ["contextos", "crm"]   # absent: every configured server
deny  = ["mail"]

[mcp_tools]
allow = [{ server = "crm", tool = "list_contacts" }]   # restricts only the servers it names
deny  = [{ server = "contextos", tool = "fs_delete_file" }]
```

A call to server S, tool T is permitted when all four of these hold:

1. S is not on the `[mcp_servers]` `deny` list.
2. S is on the `[mcp_servers]` `allow` list, or that list is absent.
3. (S, T) is not on the `[mcp_tools]` `deny` list.
4. The `[mcp_tools]` `allow` list names no tool on S, or names (S, T).

A workflow that declares none of the four lists permits every call. A `deny` always wins, and no tool `allow` lifts a server `deny`. An empty `allow` list under `[mcp_servers]` permits no server. A file that contradicts itself is refused when it is read, as `validation/artefact-invalid`: a server on both server lists, a pair on both tool lists, or a tool `allow` that names a denied server. A `*` tool name is refused in these lists, since only a library's `mcp_tools` may declare every tool on a server. Validation also requires every server an `allow` list names, in either table, to be configured in `web.toml` (`validation/server-not-configured`), and for a shared workflow checks that each tool the `[mcp_tools]` `allow` list names is offered by its server.

The library check below uses the same rule, so what validation says a library may call is what a run permits.


A script sees a failed call as an error whose message begins `workflow/<kind>:`, and an uncaught one becomes the run's failure.

## What a script receives from a call

A script gets the tool's clean JSON result, never the protocol envelope. A result with structured content is that value, and a result that is one JSON text block is that JSON. A result that embeds a file, as `fs_attach_file` does, arrives as `{ "content": "...", "content_hash": "..." }`, the shape `fs_read_text_file` returns, plus any marker fields the server sends, such as `links_expanded`. A binary file arrives as `{ "blob": "<base64>", "mime_type": "..." }`. The resource's URI is never included, because it holds a host filesystem path.

This follows the shape of the result, not the tool, so it applies to every tool on every configured server. A result with no defined script form is refused with `workflow/result-unreadable` and never partly delivered: a resource link, an image or audio block, more than one embedded resource, more than one text block, and text that is not JSON.

## Modules and the built-in library

A workflow can split its code into modules, `.rhai` files of functions beside its entry script, and import the server's built-in library. A shared workflow can also import the exported modules of library packages it depends on. Nothing else can be imported: an import of any other name, whether a path in the vault, an absolute path on the host, or a file the workflow did not declare, is refused without any file being looked for.

**Declaring.** A package lists its modules in `modules` in its `workflows.toml` or `library.toml`, relative to the package root, and every workflow of the package may import them. A workflow's own `workflow.toml` declares none, so an import name means exactly one file. Each is a plain relative path to a `.rhai` file, at most 100 of them, and none may be in `contextos/`, so no module can be mistaken for the built-in library. A declared module that does not exist fails validation, and a package bundle that does not hold one as a file is refused at install.

```toml
# workflows.toml
workflows = ["sync"]
modules = ["lib/vault.rhai", "lib/text.rhai"]
```

**Importing.** A script imports a module by its path without `.rhai`, relative to the package root whichever file the import is in, and names it with a string literal, so validation can check every import before anything runs:

```rhai
import "lib/vault" as vault;
let info = vault::info();
```

A module holds functions, the imports they need, and exported constants whose values are written out in full, and nothing else, so importing one defines its functions and values and runs none of its code. A constant is `export const NAME = <value>;` where the value is literals only (text, numbers, `true` and `false`, characters, `()`, and arrays and maps of them), such as `export const FOLDERS = ["inbox", "memory"];`. A script reads it as `layout::FOLDERS`, and a function in the same module as `global::FOLDERS`, which is always that module's own. A constant that is not exported, a `let`, or a value that calls anything is reported against the module with its line. Its imports go at its top level, above its functions, never inside a function: Rhai resolves an import inside a function against whichever script called it, not the module it is in. The entry script follows the same rule, because a function it hands to a library runs in that library's code: an import inside a function of the entry script is refused too, at validation (`validation/script-syntax`, with its line) and again at run time before any statement runs. Functions in a module call each other directly, and make calls with `call_mcp` like the entry script, under the same declared effects. Modules may import other modules, but not in a loop. Validation reports the first problem in each file against that file, with its line: a syntax error, an import that is not a string literal or names no declared or built-in module, a statement in a module that is not a function, an import, or an exported constant written out in full, an import inside a module's function, or modules that import each other.

**Import forms.** Every file belongs to one owner, a package or a vault library, and an import is checked against what its own file's owner may import:

| Form | Names | Allowed when |
| --- | --- | --- |
| `import "lib/dates"` | A module of the importing file's own owner, by its path without `.rhai` | The owner's `workflows.toml` or `library.toml` declares it in `modules` |
| `import "contextos:text"` | A built-in module | It is one of `contextos:text`, `contextos:path`, and `contextos:toml` |
| `import "crm:client"` | An exported module of a vault library: the library name, exactly one colon, and the module's path without `.rhai` | The importing file is a package's or a vault library's, and its `workflows.toml` or `library.toml` names `crm` directly in `libraries`, and `crm` lists the module in `exports` |
| `import "acme-text:strings"` | An exported module of another package: the package name, exactly one colon, and the module's path without `.rhai`, which may contain `/` | The importing file's package names `acme-text` directly in its `libraries`, and `acme-text` lists the module in `exports` |

A package does not import its own modules qualified with its own name, a module a package keeps out of its `exports`, or a library that is only a library of one of its libraries, so a library can change its own libraries without breaking the packages that use it. A name that is not one of these forms names no module.

Two owners can each have a module of the same name: the package's `util` and a library's `util` are different modules, and a library module's own imports always resolve against the library, never against the workflow that called it.

**The version.** Every declared module's path and text are part of a vault job's version, so installing a release with a changed module sends every job of that workflow back through validation, as editing its entry script does. A shared job's version already covers every file in its package. A workflow that declares no modules has the version it had before modules existed.

**The built-in library.** These modules are the server's own and compute only, and are imported with a colon, such as `import "contextos:text" as text;`: none reads or writes anything. Positions count characters, as Rhai's `index_of` and `sub_string` do.

| Module | Functions |
| --- | --- |
| `contextos:text` | `replaced(text, find, with)` returns a copy with every `find` replaced; `last_index_of(text, find)` returns the position of the last `find` or -1; `trimmed(text)`, `trimmed_start(text)`, and `trimmed_end(text)` return trimmed copies; `is_blank(text)`; `lines(text)` returns the lines without their `\n` or `\r\n` endings |
| `contextos:path` | `parent(path)`, `name(path)`, `stem(path)`, `extension(path)` (without its dot), and `join(base, relative)`, on `/`-separated paths taken as text |
| `contextos:toml` | `quoted(value)` returns `value` as a quoted TOML string with every character TOML requires escaped |

**No `eval`.** A script cannot evaluate text as code: `eval` is refused as a syntax error that validation reports with its line, because text a script reads while it runs, such as a workflow file or a note, is part of neither validation nor the job's version. Keep rules a job should change without a new version as data, such as TOML or JSON a script reads and applies.

Every build of the server allows the same nesting: an expression may nest 64 levels deep at the top level of a script and 32 inside a function, where each pair of parentheses counts as two, and calls may go 64 deep. A script that nests deeper fails validation with `validation/script-syntax`, and a call chain that goes deeper fails the run with `workflow/script-error`. Split a long chained expression into named steps with `let`.

### Vault libraries

A vault library is shared code kept in the vault itself, so it travels with the vault and is the same on every host that reads it. Use one when several of a vault's workflows talk to the same server and should share one set of functions, such as a client for a CRM. Code shared across vaults or hosts is a library package instead.

```text
registry/libraries/
└── crm/                   # the folder name is the library's name
    ├── library.toml       # required
    ├── readme.md          # optional, never read
    ├── client.rhai        # a module, exported below
    └── lib/
        └── paging.rhai    # a module only the library's own modules import
```

```toml
# registry/libraries/crm/library.toml
modules     = ["client.rhai", "lib/paging.rhai"]
exports     = ["client.rhai"]         # required, non-empty, a subset of modules
mcp_tools   = [{ server = "crm", tool = "list_contacts" }]   # optional: the tools its functions call, or { server = "crm", tool = "*" } for every tool on a server; grants nothing

[[libraries]]                         # optional: other libraries its modules import, each with a semver requirement
name        = "http-helpers"
requirement = "^1"
```

```toml
# registry/workflows/sync/workflows.toml
workflows = ["sync"]

[[libraries]]                         # optional: the libraries whose exports the package's files import
name        = "crm"
requirement = "^1"                    # a bare 1.4.2 means ^1.4.2
```

A library has no `name` key: its name is its folder's name, by the rules of a package name: lower-case ASCII letters, digits, and hyphens, starting with a letter or digit, at most 64 characters, never a name Windows reserves for a device, and never `contextos`. `modules` and `exports` follow the rules of a package's: plain relative `.rhai` paths within the library's folder, at most 100 modules, none in `contextos/`, and `exports` a non-empty subset of `modules`. A library that exports nothing is refused because nothing could use it. Unknown keys are refused, and a `library.toml` that does not match is reported against `{library}:library.toml` as `validation/artefact-invalid`.

A package's files import the exports of the libraries its `workflows.toml` names in `libraries`, as `import "crm:client" as crm;`, and a library imports the exports of the libraries it names in its own `libraries`. A library reached only through another library is not importable unless the importer names it too. A library in source has no version: it is whatever its folder holds, and installing it makes a release (see "Installing from source"). Imports inside a library resolve against the library, never against the workflow that called it, so a workflow's `helpers` module and a library's `helpers` module do not meet. A library function runs inside the calling job's run, and its calls are checked against the calling workflow's `[mcp_servers]` and `[mcp_tools]` lists and vault scope like any call the workflow makes; a library widens nothing.

**A run of source.** A one-off run of a workflow's source (`workflows run --source`) switches the whole run to source, so what is tested is what installing the workflow deploys. The workflow's code comes from `registry/workflows/{package}/`. A library the package's `workflows.toml` names without a `requirement` is read from its own source in `registry/libraries/{library}/`, as it stands, even when an older release of it is installed or none is. A library named with a `requirement` comes from the newest installed library release that satisfies it, and one that no installed release satisfies, or that is not installed, ends the run before its script starts with `workflow/package-unavailable`, naming the library and the requirement. The same rule applies to the libraries a library names: a library read from source applies it to the libraries its own `library.toml` names, and a library taken from an installed release resolves its own dependencies over installed releases, as a release does. A library that would be read from source by one package and taken from an installed release by another is refused the same way, since a run cannot use two versions of it. The run records no lock and installs nothing, and the summary lists each library as its release (`version` and `digest`) or as `source`.

**Finding libraries.** The primary server lists `registry/libraries/` and a library is the folder whose name matches exactly. No path is built from a name until it has matched a listed folder, so a folder `CRM/` is not taken for `crm`, including on a case-insensitive file system, and two folders that differ only in case are both reported, naming each. A named library with no matching folder, and two folders that differ only in case, end a run of source before its script starts with `workflow/entry-unreadable`, naming the library. Only a `path/not-found` refusal of the listing means there are no library folders: any other refusal, and an answer that is not a listing, leave the libraries unknown. That is an outage, reported as the server being unavailable, never a report that every named library is missing.

**One snapshot.** A run of source reads the workflow's entry script, its modules, and every library it reaches from source (`library.toml` and each module) through the primary server into one snapshot, from which the run is made. The reads are asynchronous calls to the server, and the checks that compile the code run on a blocking thread, never on the async executor. Each library is read once however many libraries name it, and a validation pass reads a library once and shares it between every job it reaches, so a pass sees one state of each library. A file's size is checked again after it is read, so a file that grows past the per-file limit in between is refused as unreadable. At most 32 libraries are reached, at most 8 levels deep, a library the workflow names being the first level, and at most 500 module files and 10 MiB of module text are read across the workflow and every library. A file's size is checked from its details before it is read, so a file past a limit is refused unread, and each module is also limited to 1 MiB. A cap ends the run before its script starts with `workflow/entry-unreadable`, and its message names the cap. Libraries that name each other in a loop end it the same way, naming the loop, as does a library file that cannot be read, against `{library}:{path}`.

**The version.** A library read from source has no version, and a saved job never uses one: the version of a job that resolves libraries is made from its release and from each library release it resolves, by name, version, digest, and manifest, so a new library release is a new version of the job.

**Failures.** A problem in a library module is reported against `{library}:{path}`, for example `crm:client.rhai`, with its line, and the rules for a workflow's own modules apply (functions, imports, and exported constants only; imports only at the top level; no `eval`; no modules importing each other). `validation/artefact-invalid` is reported against the library's `library.toml`, naming the library and the tool, for a tool declared in the `mcp_tools` of a library the workflow imports from (or that a module it imports from imports from) which the workflow's `[mcp_servers]` and `[mcp_tools]` lists do not permit, by the rule a run applies: the declaration grants nothing, so the workflow permits what it can reach, and validation says so before a run does. A library the workflow names and never imports from adds nothing. The walk starts at the entry script and follows the imports it makes, so a declared module that nothing imports adds no tools. A check that could not run, because its thread could not be started or it panicked, is reported as a failure against the entry script and never as a pass. Validation also reads every `call_mcp` in a library release whose server is a string literal and reports, against `{library}:library.toml` and naming the module, one that names a tool the library declares neither exactly nor with a `*` for its server. A tool that is computed is accepted only under a `*` for its server, since nothing else says which tool it is. A call whose server is computed is caught at run time as `workflow/effect-denied` when the workflow's lists do not permit it.

**Declaring every tool on a server.** An `mcp_tools` entry whose `tool` is `*` declares every tool on its server, for a library that calls tools it cannot name in advance. It is accepted only in a library's `library.toml`: a `*` in a workflow's `[mcp_tools]` allow or deny list is refused as `validation/artefact-invalid`, because a server list already covers every tool on a server. A `*` beside exact entries for the same server in one `library.toml` is refused as redundant, and a `library.toml` that does so is reported against `{library}:library.toml` as `validation/artefact-invalid`. A workflow accepts a library that declares `*` on a server only when it permits every tool on that server: the server is not on `[mcp_servers] deny`, is on `[mcp_servers] allow` or that list is absent, has no tool on the `[mcp_tools] deny` list, and is named by no entry of the `[mcp_tools] allow` list. Otherwise validation reports the `*` against `{library}:library.toml`, naming the library and the server.

**Runs.** A run reads its own snapshot when it starts. A scheduled run of an `enabled` job whose snapshot's version is not the version that was reviewed does not start: it ends with `workflow/job-not-runnable` before its script begins, and the change is reviewed on the next pass. A live run by hand, from the job page or `jobs run`, is not held back: it starts on the code as it stands. The code of a scheduled run is reviewed, unreviewed, or unknown, and only reviewed code starts: a server that cannot be asked for the job's files while the run checks them ends the run with `workflow/server-unavailable`, and a job state that cannot be read, for any reason including a refusal or a dropped connection, or that does not parse ends it with `workflow/state-unavailable`, rather than the run starting unchecked or being reported as changed. A run by hand reads no job state to start. A run whose workflow release or locked library release is gone, whether it is a run of a saved job or a one-off run of a release, ends before its script starts with `workflow/package-unavailable`, naming the library; only a run of source ends with `workflow/entry-unreadable` for a library whose source folder is missing, as "Finding libraries" says. Every live run settles a first-run trial against the version of the snapshot it read, and reads the job's files once. A dry run reads the same snapshot without the review gate, and registers the job at that snapshot's version. A completed run's record carries `package`, `version`, and `digest` of the release it read, and `libraries`, one entry per locked library release in name order, each as `name`, `version`, and `digest`; the run logs each library at INFO with its name and version and never its content.

### The shape of a package release

A release is the same in the host store and in a vault's store, and each file answers one question:

```text
manifest.toml              # the release only: name, version, digest, [[files]]
workflows.toml             # a workflow package: workflows (names only), modules, templates, libraries
library.toml               # a library package instead: modules, exports, libraries, mcp_tools
{workflow}/workflow.toml   # one workflow: name, entry, timeout_secs, entity, [mcp_servers], [mcp_tools], [[input]], [[output]], [[required_tool_contracts]], log_level
{workflow}/...             # that workflow's scripts and files
templates/  modules/       # package-level files every workflow of the package may read or import
```

`manifest.toml` holds the release's `name`, `version`, `digest`, and `[[files]]`, and no other key. A package lists exactly one of `workflows.toml` and `library.toml` at its top level, and that file says what kind of package it is; a package that lists neither or both is refused. `workflows.toml` lists the names of the package's workflows and nothing about them, `workflows = ["scaffold", "update"]`, each a lower-case slug, none twice, and declares `modules`, `templates`, and `libraries` once for the package, with paths relative to the package root. A workflow is the listed folder `{workflow}/` that holds a `{workflow}/workflow.toml`, and its slug is the folder's name. A `workflow.toml` describes that one workflow only, and declares no `modules`, `templates`, or `libraries`, so an import name or a template path means exactly one file whichever workflow uses it. It is read by one parser, whether it comes from a release in either store or from a vault's source. A `workflow.toml` in a folder `workflows.toml` does not list is refused, at install and when a vault's source is read.

A job's workflow is the folder of that name in the release it names, matched exactly; a release that lists no `{workflow}/workflow.toml` is `validation/artefact-missing`, naming that file, and no other workflow stands in. A missing file of a release, including a `workflow.toml` a release lists and does not hold, is `validation/artefact-missing` naming the file.

In a vault, the source of a package is `registry/workflows/{package}/workflows.toml`, with `registry/workflows/{package}/{workflow}/workflow.toml` and its scripts, the package-level `modules/` and `templates/` beside `workflows.toml`, and each job at `registry/workflows/{package}/{workflow}/jobs/{job}.toml`. A package's name is its source folder's name, and installing it writes its `manifest.toml`. A job is found by listing the job files where they live, and its workflow is the folder it sits in. Vault libraries stay at `registry/libraries/{library}/library.toml` with their modules.

A folder under `registry/workflows/` is refused as a package, and nothing in it registers, when its name is not a package name (the same rule as an installed package: lower-case letters, digits, and hyphens, at most 64 characters, no Windows reserved device name, and not `contextos`), when it has no `workflows.toml`, when `workflows.toml` is too large, is a link out of the vault, is not text, or does not parse, and when the server cannot be asked at all, which is an outage and is never reported as a defect in the vault. A folder with no `workflows.toml` is refused for that reason alone, so a vault still laid out as `registry/workflows/{workflow}/workflow.toml` is refused, and no message about any older layout is given. Inside a package, a folder holding a `workflow.toml` that `workflows.toml` does not list is refused, a listed workflow with no `{workflow}/workflow.toml` is `validation/artefact-missing` naming that file, a `{workflow}/workflow.toml` that exists but cannot be read is `validation/artefact-invalid` saying why. Two packages of a vault may each hold a workflow of the same name, because a job is identified by its package, its workflow, and its own key, and nothing about one is shared with the other. A workflow name follows the rules of a job key: a lower-case slug, no Windows reserved device name, and at most 64 characters.

Each refusal is shown, with its reason: the Workflows page lists a refused package folder with the badge "Not a package", and a workflow folder that does not register with the badge "Does not register", and the validation of a job under either reports the package refusal as a failure of the job (`validation/artefact-missing` for a missing `workflows.toml`, `validation/artefact-invalid` otherwise), whichever release the job names. The scheduler pass visits only package folders with a valid package name.

In source, a `[[libraries]]` entry in `workflows.toml` or `library.toml` may leave `requirement` out, because source is for development and one-off runs; an installed release must state it. A one-off run of source reads a library whose requirement the source leaves out from its own source, and takes one with a requirement from the newest installed library release that satisfies it, as "Vault libraries" describes. An install of source follows the same rule, as "Installing from source" describes: a stated requirement is resolved against the installed releases, and a library with no requirement is read from source and installed as a release first.

**Where a workflow runs.** A shared workflow runs with no vault, one vault, or several, as its `vault` inputs say. A vault workflow runs only against the vault whose store holds it, so installing a release into a vault's store is refused with `packages/vault-input-in-vault-store` for any workflow that declares a `vault` input, at any depth, a shared bundle included. The message names the file and the input.

**Versions.** A release's interface is, for each workflow, its inputs, outputs, and the four call lists, and for a library, its exports; it is read from `workflows.toml` or `library.toml` and each `workflow.toml`, and kept in the store's ledger. Adding or removing a listed workflow is a step like any other change to the interface. The step a version must take from the highest version the ledger records is the largest any workflow requires:

- **Major**: a workflow removed; an input removed, made required, or given another type; a new required input; an output removed or changed; an export removed.
- **Minor**: a workflow added; a new optional input; a new output; an input made optional; an export added; and an entry added to or removed from any of the four call lists; and a required tool added, removed, or given another contract version.
- **Patch**: the interface is unchanged.

### Installing from source

`contextos-web workflows install <vault> <package>` installs the workflow package at `registry/workflows/{package}/` into the vault's package store, and `contextos-web libraries install <vault> <library>` installs the library at `registry/libraries/{library}/`. Identity is by package, so one install makes one release of a package that may hold several workflows. Both take `--format text` (the default) or `--format json`, print one result on standard output, and exit 1 on a refusal, whose result is the same `{"error":{"code","message"}}` document the job authoring commands give. A usage error, such as a `--version` that is not an exact version, exits 2.

An install takes these steps, and writes nothing until every check has passed:

1. It reads the package's source through the primary server in one snapshot: `workflows.toml` (or `library.toml`), each listed workflow's `workflow.toml` and entry script, the modules and templates the package declares, and every library the source names without a requirement, and the libraries those name. The files count against the limits a run's snapshot keeps (500 files, 10 MiB, 32 libraries), and a template folder holding more files than that is refused before any is read. A missing file is `validation/artefact-missing` naming it, and a file that does not parse, a loop of libraries that name each other, or a snapshot over a limit is `validation/artefact-invalid`.
2. It checks the code as validation checks a job's, without a job: the manifest and each file's schema, syntax, imports, declared modules and template paths, `eval`, and the check of the tools a library declares against the workflow's `[mcp_servers]` and `[mcp_tools]` lists. A failure is reported with its validation code (for example `validation/script-syntax`) and nothing is installed. A workflow that declares a `vault` input is refused with `packages/vault-input-in-vault-store`, because a vault's workflow runs only in that vault.
3. It checks that every library requirement the source states is satisfied by a release installed now, using the procedure a run uses, and refuses with `packages/dependency-unsatisfied` otherwise. No library lock is made; a lock is fixed when a job on the release first passes validation.
4. It compares the content with the store. Content equal to a `current` or `future` release of a workflow package, or to any installed release of a library, installs nothing and reports that release. Content equal to a `previous`, retired, or removed release is a new release, because those are closed to new jobs.
5. It writes the release into a staging folder with its `manifest.toml`, records it in the ledger, and makes it visible with one rename, under the vault's store lock. A busy lock turns the install away at once with `packages/store-locked`.
6. For a workflow package it records each workflow's `future` in the vault's `.contextos/jobs.toml` as the host store does (the newest installed release newer than `current`), and reports the release, whether it is current, previous, or future for each workflow, the libraries it requires, and the libraries installed or found installed on the way.

**The version.** Nobody chooses it. The store takes the highest version the ledger records for the package, removed releases included, and steps it by the largest step the interface rules in "The shape of a package release" require: major, minor, or, when the interface is unchanged, patch. The first release is 1.0.0. The ledger keeps each release's interface so the step can be judged. A workflow package and a library share one name space in the vault's store, so source whose name the other kind holds is refused with `packages/duplicate-identity`, naming the source folder.

**What is installed is what was tested.** A `[[libraries]]` entry in source may state no requirement. With a requirement, the install uses the newest installed release of that library that satisfies it, and the requirement is kept as written. Without one, the install reads the library from `registry/libraries/{library}/` and installs it first, as a release, by these same rules and recursively for the libraries it names: source equal to an installed release reuses that release, and source that differs becomes the next version. The package's installed release then requires `^` the version of each such library it was tested on (`^1.5.0`), so a later compatible release is used without installing the package again. If a check of the package fails, no library release is made either.

**A bundle's version.** A bundle installed into either store, with `packages install <vault> <bundle>`, `packages install --shared <bundle>`, or the shared upload route, keeps the version its manifest declares, and nobody works it out. The same rules judge it: the store takes the release it follows, which is the highest version the ledger records for the package below the declared one (removed releases included, using the interface the ledger kept), compares the interface of the bundle with that release's, and refuses the install with `packages/version-step-invalid` (`422` on the route) when the step the version takes is smaller than the changes require. A version that falls between recorded versions is checked against both neighbours: the recorded release below must step to it correctly, and it must step to the recorded release above correctly, removed releases included in both, and the refusal names the pair whose step is too small. The refusal names the two versions, the step taken and the step needed, and each interface change found, such as `sync: input day removed`. A larger step than the changes need is accepted. The first release of a package is accepted at any version, and a release below every recorded version is judged only against the release above it. A pre-release version is refused with `packages/invalid-version`, and a version already recorded with another digest keeps its `packages/duplicate-identity` refusal. The check runs under the store's lock before anything is written, so a refused bundle leaves the store as it was. A shared install's refusal prints `refused <code>: <message>` on standard output, as a vault install's does.

**Installing at a version.** `--version <version>` restores that exact release, and only that. It is allowed when the ledger records the release with a digest equal to the source's and the release is removed, damaged, or incomplete: the replacement is staged, the old folder is renamed aside, the replacement is renamed in, and the aside folder is deleted. A release that is present and verified is reported `unchanged`. A digest that differs is `packages/duplicate-identity`, because a version is never reused with other content, and a version the ledger has never recorded is `packages/not-found`. A library the source reads without a requirement must still match an installed release, or the repair is refused as `packages/duplicate-identity`, since the package could not be rebuilt as it was. A release installed from a bundle has no source in the vault, so it is repaired by installing the same bundle again.

**Locks.** The job state is read first, the vault's store lock is then taken and held until the release is installed, and `future` is recorded last by reading the state again, applying the same rule `packages install` applies in the host store, and writing it with the file's content hash. Another writer changing the state in between makes the read and write repeat, up to three times, before the install reports `workflow/state-changed`; the release is installed by then, and installing again records it.

### Reporting a workflow's releases

`contextos-web workflows releases <vault> <package> <workflow>` reports the releases of one workflow in a vault's package store, and `contextos-web workflows releases --shared <package> <workflow>` reports the same for a package in the host's package store. Both take `--format text` (the default) or `--format json`, and one code reads the two stores above the store port, so the output has the same shape for both. The command only reads: it changes no release, ledger, or state file. A refusal exits 1 with the usual `{"error":{"code","message"}}` document, and a wrong number of words exits 2. A vault report connects to the primary server, as an install does; the host report reads the host's folders and needs no `web.toml`.

Each release the store holds, or the state file records for the workflow, is one entry, newest version first. A release of the package that does not list the workflow is left out unless the state file records it. Whether a release is current, previous, future, or retired is read from the state file's recorded values (`current`, `previous`, `future`, and a retired marker); a release the state file does not mention is future when no release is current or when it is newer than the current one, and retired otherwise. A person reads it as "Current, installed 3 November 2026", and the version appears in the details beneath it:

```text
ops/sync in va
Current, installed 3 November 2026
    version 1.0.0
    locked to crm 1.2.0
    jobs 2 enabled, 1 suspended
Future, installed 3 November 2026
    version 1.0.1
    no lock yet
    no jobs
```

The JSON document holds `store` (`shared`, or the vault's name), `package`, `workflow`, and `releases`, and each release holds these fields, which an agent reads:

| Field | Meaning |
| --- | --- |
| `version` | The exact release version, which a job file's `version` names. |
| `kind` | `current`, `previous`, `future`, or `retired`. |
| `summary` | The words a person reads, such as `Current, installed 3 November 2026`. |
| `installed_at`, `origin` | When the ledger says the release was installed (RFC 3339, UTC), and `source` or `bundle`. Both are `null` for a release the ledger does not record, such as one placed by hand or a store whose ledger is missing or unreadable. |
| `condition` and `problems` | `whole`, `removed`, `incomplete`, `damaged`, or `unverifiable`, told apart by what is present as the read check does, and one line for each finding. A release the state file records but the store no longer holds is `removed`. A listed template or any other listed file that is absent is an `incomplete` finding naming the path. |
| `missing_templates` | For a vault release, the templates it declares that source no longer holds, in the order declared (text: one `missing template: {path}` line each). A vault workflow's templates are read live from `registry/`, so a release can outlive one. Always empty for a host release, whose templates are in the release. |
| `retired` | `null`, or the recorded marker's `at` and `cause` (`superseded`). A release that is retired only because it is older than the current release has no marker. |
| `lock` | `null` until a job on the release has passed validation, then each library release the workflow release runs with: `library`, `version`, and `digest`. |
| `jobs` | How many jobs name the release in their job file, by lifecycle stage; a stage with none is absent. |

Jobs are counted from the job files of the same store that `jobs list` reads, by the `version` each names, so a job file that does not load is not counted.

### Library packages

A package shares code with other packages by being a library: it lists a `library.toml` among its `[[files]]` and no `workflows.toml`. The `library.toml` is the same file a vault library has (`modules`, `exports`, `libraries`, `mcp_tools`), with no `name` key, since the package's name is the manifest's, and its `mcp_tools` declares the tools the library calls, or `{ server = "...", tool = "*" }` for every tool on a server:

```toml
# A library package's manifest.toml
name    = "acme-text"
version = "1.4.2"
digest  = "sha256:..."

[[files]]
path = "library.toml"
hash = "..."
```

```toml
# A library package's library.toml
modules = ["strings.rhai", "lib/internal.rhai"]
exports = ["strings.rhai"]
```

Only a library exports modules: `exports` is a key of `library.toml` alone. A library's `exports` is non-empty and each entry is a declared module, listed once. A library declares no `templates`, because nothing in it would use them, and so cannot ship data files. Installing a library reads its `library.toml` against the schema a vault library's is, so a `library.toml` that does not match is refused at install.

A package that wants a library names it in `libraries` in its `workflows.toml` or `library.toml`, as `[[libraries]]` with a `name` and a semver `requirement` (a bare `1.4.2` means `^1.4.2`), and imports its exports as `package:module`. A vault's source names its libraries the same way, except that an entry may leave `requirement` out, which makes a run of source read that library from its own folder as it stands; "A run of source" below sets out the rule. A library function runs inside the calling job's run, so its calls are checked against the calling workflow's `[mcp_servers]` and `[mcp_tools]` lists and vault scope, and the library's `mcp_tools` grants nothing. Validation checks the library releases a shared job resolves exactly as it checks a vault job's libraries: every tool a reached library declares, and every `*`, must be permitted by the workflow's lists, and every literal `call_mcp` in the library must be declared in its `library.toml`, with a computed tool accepted only under a `*`. The failures are reported as `validation/artefact-invalid` against `{library}:library.toml`. A library release whose `library.toml` cannot be read or no longer matches its schema is an invalid installed release.

**Choosing releases.** The library releases a workflow release runs with are chosen by one fixed procedure with no backtracking, so two hosts with the same releases installed choose the same ones. The choice is made when the first job on the workflow release passes validation, which records it as the release's library lock, and a one-off run of a release with no lock makes it for that run only:

1. The requirements start as the workflow's package's `libraries`.
2. In rounds, every name that has a requirement and no choice yet is taken in name order. For each, the newest installed release that satisfies every requirement collected so far is chosen, by the `semver` crate's matching, so a pre-release is chosen only when a requirement admits it. The chosen release's own `libraries` are then added.
3. A later requirement that excludes a release already chosen fails the resolution, and nothing is retried. A name no installed release satisfies, a name with no installed release, and a package reached again through its own libraries fail it too. Each failure names the package and every requirement on it, with the release that made it.
4. At most 32 library packages are reached and at most 8 levels deep, a library the package names being the first level.

**The snapshot.** The package release, its manifest, every chosen library release and manifest, the entry script, and the text of every module of every one of those packages are read once, in one blocking step, before the script starts. The store is not locked while this happens, so the read ends by reading each resolved release again in full (its folder against its manifest, every listed file against its hash, and the whole against its digest) and fails, naming the release, when one is gone or is no longer the one that was read: an install, removal, or reinstall in the middle of a read is never mixed into one snapshot. A file that cannot be read because its release went in the middle of the read is reported the same way. That is a change on the host caught mid-read and not a defect in the job: nothing about the job changes (no lifecycle transition, no stored validation result), an on-demand validation reports the check as unverifiable and asks for another try, a manual run fails before its script starts and says the release changed while it was read, and a scheduled tick that finds it stays due, consumes nothing, and leaves no run record, so the next pass reads again. Validation and a run are separate reads, tied together by the job's version: a run starts only when the version it resolves is one that was reviewed. A scheduled run does not read the store a second time: the scheduler reads the snapshot before it claims the tick, and the run it starts executes that snapshot, so a due tick costs one read and a release changing after it cannot fail the run. The snapshot's version is still checked against the reviewed one. A manual run, and `jobs run`, read their own. At most 500 module files and 10 MiB of module text are read in total; each file's size is checked from its details before it is read, so a file past either limit is refused unread, and the bytes a read actually returns are charged too, so a file that grows in between cannot take the snapshot past the limit. Each module is also limited to 1 MiB, as a vault workflow's are.

**No pinning.** A job file chooses the release of its own package with `version`, and never a library release: a `[libraries]` table is an unknown key and makes the job file invalid. The libraries a job runs with are its workflow release's library lock, so installing a newer library release changes no job already validated on the release: a library fix reaches a workflow through a new workflow release and successors. Removing a library release a lock names is refused while a rule keeps it (see "Listing, inspecting, and removing").

**Failures.** A library that is not installed or cannot be read, and requirements no installed release satisfies, are `validation/artefact-missing`, as a release of the job's own package that does not resolve is. An installed release, of the job's own package or of a library it resolves, whose `manifest.toml` no longer parses or no longer matches its schema is `validation/artefact-invalid`, naming the package or library: the release is there but not valid. A release is also damaged when a listed file no longer hashes to what its manifest lists, its files no longer hash to its digest, a link stands in it, or its manifest names another release than its folder holds. It counts when it is the release the choice falls on: the release the job file names, or the newest installed release satisfying the requirements on a library. No other release stands in for it. A release with a listed file absent or an unlisted file present, or one the store could not be read for, cannot be verified: nothing about the job changes, as for a release that changed while it was read, and the run or check says so and tries again later. A run of a damaged release ends before its script starts with `workflow/package-unavailable`, as for a package that is gone, but the release is there, so the next pass suspends an `enabled` job with `workflow/validation-failed` and a `validated` or `registered` job returns to `draft`. A set of libraries past a limit (the message names the limit), and a package reached again through its own libraries, are also `validation/artefact-invalid`. Each failure names the library and every requirement on it with the release that made it. The modules of every library a job reaches are checked as the package's own are, and a problem in one is reported against `library:path`, for example `acme-text:strings.rhai`, with its line. A run whose libraries do not resolve ends before its script starts with `workflow/package-unavailable`, naming the library.

**When libraries go away.** A library package that is removed, or replaced so that a job's libraries no longer resolve, is handled as the job's own package being removed. The job has no version while it lasts: a `validated` or `registered` job returns to `draft`, an `enabled` job is suspended with `workflow/package-unavailable` naming the library, and validating the job on demand reports `validation/artefact-missing` against the package and stores it. A due scheduled tick of an `enabled` job that has not yet been suspended is not run and leaves no run record: it fails before the claim, its tick stays due, and the next pass suspends the job. A `skipped` record is only for a tick that finds the claim held.

**Installing and removing.** An install must resolve, and a removal is guarded by what keeps a release (see "Retention" and "Listing, inspecting, and removing" below).

- Install resolves the new release, and every installed release that would now choose it. It is refused, with `packages/dependency-unsatisfied` (`422`), when the new release cannot resolve its libraries or would leave an installed release that resolved before unable to, naming each. An installed release that could not resolve before is not blamed on it. Install reports no saved jobs: a job names a release, and a library lock fixes the library releases it runs with, so installing a release does not change a job's version.
- The inventory is read one release at a time, so a release that does not read whole is listed for itself, with what is wrong with it, and blocks nothing else: the install checks and a run all leave it out, as if it were not installed, so every other release installs, is listed, and resolves as before. A manifest that names a different package or version than the folder it is in is damaged in the same way.

### Listing, inspecting, and removing

Every `packages` command names its package store the same way: the vault, or `--shared` for the host's. A vault's commands connect to the primary server as `workflows install` does; the host's need no `web.toml`. Each takes `--format text` (the default) or `--format json`, prints one result on standard output, and exits 1 on a refusal, whose result is the `{"error":{"code","message"}}` document the other commands give. A wrong number of words exits 2.

| Command | What it does |
| --- | --- |
| `packages list <vault>`, `packages list --shared` | The ledger, each release with where it came from and when, whether it can be read, whether each workflow in it is current, previous, future, or retired, the libraries it requires and the lock it runs with, and every reason a rule keeps it; for a library release, the reasons name the workflow releases whose lock holds it or whose first validation would choose it. Then the releases the ledger records as removed, with the cause, and the staging and removal folders and other entries unfinished operations left. |
| `packages inspect <vault> <package> <version>`, `packages inspect --shared <package> <version>` | The same for one release, with its digest. |
| `packages inspect <vault> --rebuild-ledger`, `packages inspect --shared --rebuild-ledger` | Rebuilds a missing ledger from the releases present, every one of which must read whole, and lists what it recorded. |
| `packages install <vault> <bundle>`, `packages install --shared <bundle>` | Installs a `.tar.gz` bundle, or a folder holding the same files, into that store, as "Installing and removing" above describes. A vault's install connects to the primary server, takes the store's lock without waiting for it, and brings the recorded `future` and retention up to date afterwards; the host's needs no `web.toml`. A refusal of the vault's install is the same `refused {code}: {message}` result the other commands give. A vault's store has no upload route. |
| `packages remove <vault> <package> <version> [--force]`, `packages remove --shared <package> <version> [--force]` | Removes one release under the guard below. |

**The guard.** A removal by request is refused with `packages/in-use` (HTTP `409`) while a rule keeps the release for a reason that means something is using it: it is the `current` or `previous` release of a workflow, a job that is not `retired` names it or was validated on it, a job file of its workflow cannot be read, or, for a library release, a kept workflow release's lock names it or its first validation would choose it (including a workflow release that does not read whole but whose recorded lock names it). The message and the `reasons` of the JSON error name the jobs and releases. The newest `future` release of a workflow and the newest release of a library are kept for what may use them next, are not in use, and are removed without `--force`. What the state and job files say about a release counts whether or not it reads whole: a damaged, incomplete, or unverifiable release that is the `current` or `previous` release of a workflow, or that a job names, is refused like a whole one, and the library releases its recorded lock names are guarded. A release of that kind that no rule holds is removed like any other. The check is made under the same locks retention takes, then the release is renamed aside and what names releases is read again, since a job file can be written by hand without those locks: a release named in the meantime is renamed back and the removal refused, or, with `--force`, removed and recorded as forced. If what names releases cannot be read again, nothing is removed.

**`--force`.** `--force` removes a release a rule keeps. The ledger records the cause `forced` when a rule was overridden and `by request` otherwise, even when `--force` was given. The result names the reasons that were overridden and the jobs the next validation pass will move because their release is gone: an `enabled` job to `suspended`, a `validated` or `registered` job to `draft`. Those jobs are found from the job files and state that name the release, directly or through the workflow release whose lock holds a library release.

**After a removal.** The recorded `future` of a workflow is rewritten when the removal changed it, by the one rule every writer of it uses, and retention then runs, because releases only the removed one kept are no longer kept. The releases retention then removed are named in the result (`Retention then removed ...` in text, `retention_removed` in JSON). The removal stands when either cannot finish; the result then carries a `packages/retention-failed` warning. A removal is complete once the release has been moved out of the store: a `_removing-` folder that then cannot be deleted is logged as a warning and cleared by the next install or removal, and the package's own folder goes with its last release.

**The ledger.** A store that holds releases and has no ledger, or whose ledger cannot be read, refuses every install and removal with `packages/ledger-unreadable` while reads, validations, and runs go on. `packages inspect --rebuild-ledger` rebuilds a missing ledger and marks the rebuild; it is refused with `packages/ledger-present` when the store has a ledger, readable or not, and with `packages/release-incomplete` while any release is incomplete or unverifiable. A ledger that cannot be read is restored from a copy.

**API.** The same operations, for a vault under `/api/v1/{vault}/packages` and for the host under `/api/v1/server/packages`:

- `GET /api/v1/{vault}/packages` answers with what `packages list --format json` prints, and `GET .../packages/{name}/versions/{version}` with what `packages inspect --format json` prints.
- `DELETE .../packages/{name}/versions/{version}` is the guarded removal: `204` when it went through cleanly, `200` with the removal document when something could not be finished afterwards (`warnings`) or retention removed further releases (`retention_removed`), and `409` `packages/in-use` with the `reasons` when a rule keeps the release. A query string is refused with `400` `packages/invalid-query`.
- `PUT .../packages/{name}/versions/{version}/execute/remove` with `{"force": true}` in its body is the removal asked for as a command, and answers `200` with the removal document, which names the cause, the overridden reasons, and the affected jobs. An empty body, or `{"force": false}`, is the guarded removal. A body that is not JSON or names another field is `400` `packages/invalid-request`.
- `POST /api/v1/server/packages` installs an uploaded bundle into the host's store, answering `201`. A vault's store has no upload route.
- Statuses by code: `404` for `packages/not-found` and an unknown vault (`vault/not-found`), `409` for `packages/in-use`, `packages/ledger-unreadable`, `packages/store-locked`, `packages/ledger-present`, `packages/release-incomplete`, and `packages/duplicate-identity`, `422` for `packages/version-step-invalid`, `packages/dependency-unsatisfied`, and `packages/invalid-manifest`, `400` for `packages/invalid-name` and `packages/invalid-version`, `500` `packages/state-unreadable` for a job state file that cannot be read (the message names no path; the log does), and `500` for a store or server that could not be read, which gives no detail beyond the code `packages/internal-error`.

### Retention

Both package stores, the host's and every vault's, remove the releases nothing keeps, automatically. A release is never removed because of its age or because a newer one exists; it is removed because no rule below keeps it, and a release removed this way returns only by installing it again. The policy is one pure function, `contextos_workflows::decide_retention`, which says for each installed release whether it is kept and gives every reason, so a request to remove a release can name what holds it.

**What is kept.** A workflow release (a release of a package that holds workflows) is kept while any of these is true for any workflow it holds:

1. It is the workflow's `current` or `previous` release.
2. A job that is not `retired` names it in its job file (`version`), or the job's state records it as the release its validated version was made from.
3. It is the newest `future` release of the workflow (the newest installed release newer than `current`, or the newest installed release when there is no `current`, leaving out the `previous` release and any release with a retired marker).
4. The workflow has a job file that cannot be read or parsed, because what it names is not known; every release of that workflow is kept until the file is fixed or removed. A job file that parses and names no exact release (no `version`, a range, build metadata, or a pre-release) names nothing and keeps nothing.

A library release is kept while a kept workflow release's library lock names it, while the first validation of a kept workflow release that has no lock yet would choose it now (the same choice, by the same procedure, over the library releases installed in that store), while the newest release of a library requires it (the newest library release stays usable, so the releases its own resolution chooses are kept), or while it is the newest release of its library. A package release is kept while anything in it is kept. Nothing else is kept, and there is no mark to keep a release.

A release that is damaged, incomplete, or unverifiable is neither kept nor removed by retention: it is not known whole, so it is left for the operator. What it holds is not known either, so the state file and the job files say what it is kept as, and the libraries it holds are kept by that: when it is the `current` or `previous` release of a workflow, or a job that is not `retired` names it, the library releases the state's recorded lock for it names are kept, and when it has no recorded lock every library release in the store is kept, since which ones its first validation would choose cannot be worked out. A store that holds releases and has no state file (a vault's `.contextos/jobs.toml`, the host's `shared-jobs.toml`) runs no retention at all, because nothing then says which releases are in use.

**When it runs.** After an install (a bundle or folder into the host store, and an install from source into a vault's), after a job goes from `registered` to `enabled`, after a job is retired or deleted, after a job's `version` is changed in place by `jobs replace` (a replacement that leaves the `version` as it was runs nothing), after a validation that passed, whether an operator's, or an automatic pass's, because the first passing validation of a workflow release fixes its library lock, and after a package release is removed by request. `jobs create` runs no retention: a new job file names a release, and nothing it does leaves a release unkept. A request the transition table refuses runs nothing.

The first validation of a vault's workflow release chooses its library releases and records them as the lock in one state write, so it holds the vault store lock from before the choice to after the write, and retention cannot remove a chosen library release in between. A validation made while another operation holds the lock is reported and not recorded (a conflict, repeated by repeating the request), and a validation pass that finds the lock held validates nothing and looks again at the next pass.

**How it runs, the same in both stores.** It takes the locks in the one order every writer of what is kept uses: the state lock then the host store lock for the host, and the vault store lock for a vault. It reads the state file and every job file of every workflow, decides, and renames each release it will remove into a `_removing-` folder. A job file, and a vault's state file, can be written without those locks, so it then reads them again: a release now named, or of a workflow whose job file became unreadable, is renamed back, and if they cannot be read again nothing is removed. What is left aside is recorded in the ledger with the cause `retention` and deleted. If a crash leaves a `_removing-` folder with no ledger record, the next store operation renames the release back. Finally the recorded `future` of every package is checked against the releases that read whole and rewritten, by the one rule that rewrites it everywhere, when it does not name the newest of them, whether or not this run removed anything.

**When it fails.** A retention that cannot finish (the store's lock is held, the ledger is missing, what names the releases cannot be read) is a warning on the operation that triggered it, and the operation itself has succeeded and stands. The warning has the code `packages/retention-failed` and a message; the failure is logged once at `warn` with the code and the store (`host` or `vault`), never with any vault content. `packages install` and `packages remove` print `Warning: packages/retention-failed: ...`, a removal route carries it in a `warnings` array of its document, the upload route answers `201` with a `warnings` array (absent when there is none), `workflows install` and `libraries install` print a `warning ...` line and carry a `warnings` array in `--format json`, the `jobs` commands print the same line and carry `warnings`, a job page appends it to its status message, `jobs replace` lists it with its other warnings, a job page's delete shows it beside a link back to the jobs list instead of redirecting, and a validation pass reports it in its `warnings`. An install that retention then removes at once (an older release than the one a workflow runs, with no job naming it) says so: `packages install` prints `Installed ... then retention removed it again because nothing keeps it`, `workflows install` and `libraries install` add `removed_by_retention` to their JSON document, and so does the upload route's `201`. Nothing is rolled back and nothing is retried on a timer: the next change to what is kept runs retention again.

### The vault health check

A vault's Vault Health pane lists the findings of the server's `doctor` and, after them under "Workflow releases", the findings this interface makes itself about the vault's package store, because `doctor` knows nothing of it. It reads the store, its state file, and the jobs as `packages list` and `workflows releases` do. Nothing it reports is repaired automatically; each says what to do.

| Finding | Reported when |
| --- | --- |
| An incomplete, damaged, or unverifiable release | A release in the store does not read whole, with each finding. Recovery is the one in the table of damaged, removed, or incomplete releases: restore or remove the entries listed, or reinstall at the release's version. |
| Missing templates | A vault release declares a template that `registry/` no longer holds, listed against its package, workflow, and version. |
| Left-over folders | A `_staging-` or `_removing-` folder from an operation that did not finish, or an entry in the store that is none of these. The next install or removal in the vault deletes or puts back the first two. |
| The ledger | The store holds releases and has no ledger, the ledger cannot be read, a release is in the store and not in the ledger, or a release is gone from the store with no removal recorded. |
| Jobs naming releases the store lacks | A job that is not `retired` names a release the store does not hold, or the vault has no package store at all. |

The store is read once and the jobs are listed once for all of these findings, and a vault's source is asked only for the templates each release declares, once per release and reported against each of its workflows. A failure is a finding about what failed and no more: a store that cannot be read is itself a finding, with the stable code of what failed, and is never shown as healthy; jobs that cannot be listed are a finding that leaves the findings about the releases in place; and templates that could not be looked for are a finding about that workflow release. The store checks have their own time limit (30 seconds), inside the assessment's, so a slow store is one finding and the server's `doctor` findings are still shown. A vault with no package store and no job that names a release has no finding. The assessment is the one every indicator shares, so the header icon and the footer chip count these findings too.

## Files a workflow ships

A workflow can carry files beside its script, such as templates and rules, and its script can read them, never write them. Every change still goes through `call_mcp`, so a script that deploys a template reads it here and writes it to a vault with a tool such as `fs_write_file`.

**Declaring.** A package lists the files and folders its workflows' scripts may read in `templates` in its `workflows.toml`, relative to the package root, and every workflow of the package may read them. A folder admits every file beneath it.

```toml
# workflows.toml
workflows = ["sync"]
templates = ["scaffold", "rules/lint.toml"]
```

A path is `/`-separated and relative, with no empty, `.`, or `..` segment, no backslash, colon, or NUL, no segment ending in a dot or a space, and no segment Windows reserves for a device (`CON`, `PRN`, `AUX`, `NUL`, `COM1` to `COM9`, `LPT1` to `LPT9`, with or without an extension), so it cannot leave the workflow's root and names one file the same way on every platform. A package entry script's path follows the same rule, and an installed release whose `entry` breaks it no longer runs until a corrected release is installed. A package install also refuses a file beneath a declared folder whose name breaks the rule. A package whose manifest declares a path the bundle does not hold, or whose bundle holds a link or any member other than a regular file or a folder, is refused at install. A vault package that declares a path it does not hold, or a path that breaks the rule, does not register, and validation reports it as `validation/artefact-missing` or `validation/artefact-invalid`.

**Reading.** Two functions, each refusing a path that is not declared before anything is read:

| Function | Returns |
| --- | --- |
| `workflow_file(path)` | `#{ content, content_hash }`: the file's complete text and the SHA-256 of its bytes, the shape `fs_attach_file` gives a script, so a shipped file can be compared with a vault file by hash |
| `workflow_files()`, `workflow_files(prefix)` | Every declared file, or every declared file at or beneath `prefix`, as a sorted array of paths. A prefix that overlaps nothing declared is refused |

A workflow reads from the installed release its run resolved, so a release installed during a run, or between two runs, changes only what a later run reads. A link found in an installed release is refused rather than followed. A one-off run of a vault's source reads the package's files through the primary server, like any vault file, so a link in its folder is followed within the vault, as every MCP read follows one, and one that leads out of the vault is refused; a listing leaves out the paths the vault hides. They are read when the script asks for them.

A file is at most 1 MiB and a listing at most 1000 files; neither is ever cut short. A refusal is an error the script can catch, beginning with its code, and an uncaught one fails the run with it: `workflow/file-path-invalid`, `workflow/file-undeclared`, `workflow/file-not-found` (also for a folder), `workflow/file-not-text`, `workflow/file-too-large`, and `workflow/file-unreadable` (also for a link in an installed release, and for a vault path the server refuses). A vault workflow whose primary server cannot be reached gets `workflow/server-unavailable`. A read waits no longer than the run's timeout and stops when the run is cancelled.

**Dry runs.** The functions read the real files in a dry run, since a read changes nothing, so a scenario exercises a scaffold or an update with only its `call_mcp` writes mocked. Reads are not calls, so a scenario does not list them and a run record does not log them.

## Logging and warnings

A script has four logging functions and one flag, each taking a string. A non-string argument fails the script with an error naming the function.

| Function | Effect |
| --- | --- |
| `log_debug(message)`, `log_info(message)` | Written to the host log at that level when the run's minimum level allows it. No effect on the outcome |
| `log_warn(message)`, `log_error(message)` | Written at that level when allowed, and always counted as a reason the run is degraded, whatever the minimum |
| `flag_degraded(reason)` | Counts a reason the run is degraded and writes nothing. Use it for a retry, a delay, or any problem the script recovered from |

Rhai's own `print` and `debug` are log messages too: `print(text)` is written at `info` and `debug(text)` at `debug`, through the same minimum level and the same 2 KiB cut, and neither makes the run degraded. They never reach standard output or standard error directly, so a script cannot corrupt a run summary. A string given to `debug` is written as it is, without the quotes Rhai would put around it. Use `set_output` for anything a caller needs.

Each message is one `tracing` event carrying the workflow, the job and the run identifier when there are any. It is cut at 2 KiB with a ` [cut]` marker. The host adds no input value, credential, or vault content of its own, but the script author decides what a message says, so keep vault content out of messages at info and above.

**Minimum level.** One of `debug`, `info`, `warn`, or `error`. The job's `log_level` wins, then the workflow's, then `[workflows] log_level` in `web.toml`, and with none set the level is `info`. A workflow sets it as `log_level` in `workflow.toml`, and a job as `log_level` in its manifest or shared job file. The messages are visible only if the server's own `[server] log_level` also lets them through. An unknown value fails the load of that one file.

```toml
# web.toml
[workflows]
log_level = "warn"
```

**Succeeded with warnings.** A run that completes without failing, but counted at least one reason, ends as succeeded with warnings. The first 20 reasons are kept, each cut at 512 bytes, with a count of the rest. A run that fails or times out is failed, whatever it flagged earlier. The summary's outcome is `degraded`, its `warnings` hold the reasons (and `Warning:` lines in the text form), and the command exits 0. Reasons are no longer written to standard error. A run record for a job in a vault gets `status: degraded`, which the web interface flags as needing attention without showing it as failed, and retention keeps it like a failed or skipped run. The reasons are not written to the record yet.

A script's own error, including a syntax error, carries `workflow/script-error`, and its message begins with that code.

## A vault job's file

A vault job is `registry/workflows/{package}/{workflow}/jobs/{job}.toml`. It accepts only these keys, and any other fails the job's load:

```toml
name         = "Nightly sync"     # required
slug         = "nightly-sync"     # optional, defaults to the file stem
timeout_secs = 120                # required: whole seconds, greater than zero
version      = "1.2.0"            # required: the exact release of the workflow's package the job runs
predecessor  = "nightly-sync-v1"  # optional: the key of the job of the same workflow this one succeeds, written by `jobs successor`
owner        = "vela"             # required for the job to run: a member of .contextos/team.toml
entity       = "personal"         # optional: a configured entity, or "vault"
log_level    = "info"             # optional minimum log level
ready        = true               # optional: authoring is finished, so validation may run

[inputs]
since = "2026-01-01"
```

**`timeout_secs` is required, and a job never takes its workflow's.** A job file without it does not load: its load fails, naming `timeout_secs`, and validation reports `validation/artefact-invalid`. The same holds for a shared job file, which does not load without it (`jobs create --shared` refuses it with `workflow/job-file-invalid`). A workflow's own `timeout_secs` in `workflow.toml` stays optional and applies only to a one-off run of the workflow (`workflows run`), which defaults to 300 seconds.

**`version` is required, and a job never runs the newest release.** A job file without it does not load: its load fails, naming `version`, and validation reports `validation/artefact-invalid`. It is an exact `major.minor.patch` release of the workflow's package that is installed in the vault's package store (`contextos-web workflows install` reports the version it made), with no range, and it is the same key a shared job file has.

**`owner` is required for a job to run, and is checked against the team.** A job file with no `owner`, or one whose owner is not a member of the vault's team, still loads, so its page, its listing, and a dry run can see it, but validation fails it with `validation/owner-unresolved` and it never runs live (see "The owner gate"). A slug is matched exactly: lowercase letters, digits, and hyphens, and nothing is lowercased or repaired, so `Vela` is not `vela`, and the failure names the text the file declares. A shared job has no owner of its own.

**`entity` is optional.** A job's entity is the job's own `entity`, else its workflow's (`entity` in `workflow.toml`), else `vault`. A declared entity must be `vault` or the slug of an entity in `.contextos/entities.toml`, or validation fails with `validation/entity-unresolved`; entities that cannot be read make the check unverifiable, and an outage is not a defect in the job. The entity is part of the job's version. A run record, and a task raised for the job, carries the resolved entity, so what a record says is what somebody declared and never a name nobody did. A shared job or package workflow has no `entity`.

## The team a vault's jobs need

A vault with saved jobs needs `.contextos/team.toml` with exactly one member who has `chief_of_staff = true`, unique lowercase slugs, and no other broken rule. The chief of staff is who a failure goes to when its job has no owner who is a member. The Agents and Tasks screens read the file leniently as before, and an absent file is an empty team there.

A team that cannot be used for jobs is a configuration error with the stable code `workflow/team-invalid` and a rule that names which was broken (`file-absent`, `file-not-text`, `file-unparsable`, `invalid-slug`, `duplicate-slug`, `team-empty` for a team with no member, checked before the chief count, `no-chief-of-staff`, or `several-chiefs-of-staff`), never a member's name or any of the file's content. While it stands the vault's whole job pass waits: nothing is validated, suspended, raised, or started, and nothing is recorded against any job. It is reported in four places:

- the scheduler logs one `warn` when the problem starts and one `info` when it clears, each with `code = workflow/team-invalid` and the vault (the `warn` also names the rule), and nothing on the passes between;
- `doctor`'s `team_config` check fails, naming the rule, and a vault with at least one saved job and no `team.toml` fails it too (a vault with no job passes with an advisory to add one before the first job);
- the vault's workflows page and each job's page say `scheduled jobs in this vault are paused`, with the code and the rule;
- a live run by hand is refused with `workflow/team-invalid`, and `jobs validate` and `jobs resume` report the owner check as unverifiable and change nothing.

A team that cannot be read at all, because the server cannot be reached or refuses the read for a reason other than the file's being absent, is an outage and not a team error: the pass waits, and the scheduler logs one `warn` when the outage starts and one `info` when it clears, each with `code = workflow/server-unavailable` and the vault and nothing else, and nothing is recorded against any job.

When the team file is absent, the pass asks whether the vault has any saved job. A listing that fails to answer says nothing about the team: the pass waits, and what was noted of the team (a problem that is standing, or none) stays exactly as it was, so a failed listing never clears a standing problem and logs no `info` that the team is valid again. Once the team is corrected, the next pass goes ahead and runs only the latest tick each job missed (see "Missed ticks").

## Job lifecycle

A job has one stored state, which decides how it may run. The state is held in `.contextos/jobs.toml` with the rest of what an operator decides about a job, never in a manifest that authoring writes. The file holds nothing a run writes: which run is active, the next run number, and the last tick that fired are kept on the host beside the job's claim (see "Claims and active runs").

| State | Runs on its schedule | Run by hand |
| --- | --- | --- |
| `draft` | no | no |
| `validated` | no | no |
| `registered` | no | yes |
| `enabled` | yes | yes |
| `suspended` | no | yes |
| `retired` | no | no |

A job nothing is stored for is a `draft`. A run by hand of a job that is not `registered`, `enabled`, or `suspended` is refused with `workflow/job-not-runnable`, naming the state, before anything is claimed or recorded. A dry run is allowed in every state but `retired`, because it reads and never writes.

**Schedule.** A job's schedule is stored beside its state and set by the operator, and the scheduler starts a job only when it is `enabled`. A job file has no `schedule` key, so a job with none stored runs by hand only.

**Transitions.** Each transition has the actors that own it, and a change by any other actor is refused with `workflow/transition-refused`, writing nothing. The validator moves a `draft` to `validated`. A completed dry run moves `validated` to `registered`. Only the operator sets a schedule on a `registered` job, which makes it `enabled` for the version the job has now, and a job whose files changed since it was validated and dry-run is refused until it is validated again; replaces the schedule of an `enabled` or `suspended` job, which does not change its state; clears the schedule of an `enabled` job, which makes it `registered`; resumes a `suspended` job, which keeps its schedule and needs validation to have passed for the version the job has now; retires an `enabled` or `suspended` job, which clears its schedule; re-activates a `retired` job as a `draft`; and deletes a `retired` job. The operator or the system suspends an `enabled` job, and a suspension always carries a reason. A changed version returns a `draft`, `validated`, or `registered` job to `draft`, and puts an `enabled` job on probation (below). A `draft` job, and a `validated` or `registered` job whose version just changed, may name only the current or a future release: validation fails with `validation/artefact-invalid` on `version` for a release that is now previous or retired, except for a return successor naming the previous release. A `validated` or `registered` job whose release becomes previous keeps its stage but cannot be enabled until `jobs replace` moves its `version` to the current or a future release, which returns it to `draft`.

Setting a schedule, and resuming, count the schedule from that moment, so a tick that fell due while the job was not running is not run late. A state change never cancels a run in progress: a job suspended or retired while running finishes. Every change is made through `job_lifecycle::change_lifecycle` for a vault job and `shared_lifecycle::change_shared_lifecycle` for a shared one, which reads the state with its hash and writes it back with it, so a change another process made in between is rejected with `workflow/state-changed` and never overwritten, and a state file that cannot be read or written is `workflow/state-unavailable`.

**State keys.** A vault's `.contextos/jobs.toml` and the host's `state/shared-jobs.toml` have one layout, with no fixed words at the top: a table for each workflow of each package, `[{package}.{workflow}]`, which holds the workflow's `current`, `previous`, and `future` versions (`current` and `previous` each with the time it took effect, as `current_at` and `previous_at`); a table for each release under its quoted version, `[{package}.{workflow}."{version}"]`, which holds that release's library lock and its retired marker; and a table for each job, `[{package}.{workflow}.{job}]`, which holds the job's state, schedule, and recorded facts. A version is quoted and always holds dots, and a job key never does, so the two cannot meet. `current`, `previous`, and `future` are not job keys: a job file whose key is one of them does not load, and `jobs create` and `jobs replace` refuse it with `workflow/job-key-invalid`. A release is current or previous by what these tables record. It is future when it is newer than `current` (or there is no `current`), unless it is the `previous` release or carries a retired marker, so there can be several future releases; the recorded `future` is the newest of them, the newest installed release of the workflow that is newer than `current` (or the newest installed release when there is no `current`), leaving out the `previous` release and any release with a retired marker, and absent when there is none. It is rewritten in the same state write as the change that moves it: an install of a release (`contextos-web packages install`, or the upload route), a removal of one (by request, forced, or by retention), and an enabling that moves `current` and `previous`. Whether a release is future does not depend on the recorded value, which only names the newest future release (the default target of `jobs successor --to future`). The host's install and removal take the state lock before the store's lock and change the state while holding both, so a refused install or removal writes no state, and a state file that cannot be read refuses the install or removal before the store is changed. Both stores use the one rule, `JobsState::record_future`. An old form of the file is read as unknown keys, like any other. A job's table also holds `ever_enabled = true` once the job has been `enabled`, which suspension and retirement keep. A job read from a file without it counts as having been enabled when it is `enabled`, is `suspended`, or records a retirement by its successor, and as never enabled otherwise. A successor's table holds its `[{package}.{workflow}.{job}.link]`, written at its first passing validation: the predecessor's `identity`, whether it was already `retired`, and the `predecessor` key the job file named then. An enabled, suspended, or retired job's file is held to that key, so a file that names another key has been edited, even when the job it named was deleted. A link recorded without a key is held only to the file naming a predecessor at all.

**When current and previous move.** Only an enabling moves them, in the same state write. Enabling a job on a future release makes the former `current` the `previous` and marks every release not newer than the new `current` that is now neither as retired with the cause `superseded`. Enabling a return successor on the previous release swaps `current` and `previous`; a swap that moves `current` up marks every unmarked release strictly between the old and the new `current` as `superseded` at the time of the enabling, so a release passed over stays retired, and a swap that moves `current` down marks none. A marker already recorded keeps its first time.

The state file is an ordinary vault file, written through the same MCP tools as any other. The lifecycle keeps authored job and workflow files from scheduling or enabling anything, and it does not defend the state file against a writer that already has write access to the vault.

**The owner gate.** A vault job whose owner is missing or is not a member of the team never runs live. An `enabled` job in that position is suspended by the system on the next pass, with code `workflow/owner-unresolved`, before anything else about it is looked at, and a task goes to the chief of staff (see "Tasks raised for job failures"). A live run by hand, from the command line or a page, is refused with `workflow/owner-unresolved` before the job's claim is taken and before a run number is: nothing is written, the job keeps its lifecycle, and a job on trial stays on trial. In a run summary the refusal is `refused`, exit 3. A scheduled tick of such a job is not started and is not recorded as fired. A dry run stays allowed, because it reads and never writes. A job that is only `validated` or `registered` is left as it is and raises no task, because it never runs unattended. Resuming validates, and validation fails an unresolved owner with `validation/owner-unresolved`, so a suspended job whose owner has not been put back cannot be resumed. The owner coming back does not resume a job: it stays suspended until the operator resumes it, and the schedule counts from the resume.

**The suspension record.** A job's suspension is stored in `.contextos/jobs.toml` as a structured table beside its state, and no longer as free text:

```toml
[ops.sync.nightly.suspension]
code = "workflow/probation-failed"
by = "system"
at = "2026-10-21T03:00:05+11:00"
reason = "the first live run of this version failed: ..."
run = 4
failure = "workflow/script-error"
```

`code` is one of `workflow/operator-suspended` (the operator, with the operator's reason), `workflow/validation-failed` (a new version failed validation, or the rules tightened and an enabled job no longer passes), `workflow/probation-failed` (the first live run of a version failed: `run` is its number and `failure` the code it failed with), `workflow/owner-unresolved`, and `workflow/package-unavailable` (a shared job's package is gone). `by` and `at` are who suspended the job and when, and each is absent when it was never recorded; `run` is absent when the failed first run had no record to number it by. A suspension an earlier release stored as free text (`suspended_reason`) reads as `workflow/suspended`, with the actor and time the job's state already held. What that state never held stays unknown: no actor is assumed to be the system and no time is invented, none is written back, pages and the command line leave the field out, and such a suspension raises no task, since it may have been the operator's. The record is written back in the new form and never with `suspended_reason`. A refusal that names a suspended job, for a vault job or a shared job, carries the whole record in its text (code, who, when, run, failure, and reason) and, in a run command's JSON summary, as a structured `suspension` object beside `message` (`code`, `by`, `at`, `run`, `reason`, and `failure`, each left out when it was never recorded). Every text read from the state, the reason, the failure code, and the code included, is cut to the length a recorded error may have, marker included, when it is read, so a hand-edited state never yields an unbounded refusal or page. Replacing the schedule of a suspended job changes none of this, and neither the time nor the actor of the last change of state. The job pages, `jobs list`, and the tasks name the code, and a failed first run's number links its record.

**Missed ticks.** When several ticks have fallen due since the last that fired (a stopped process, a long sleep, a vault whose jobs were paused for its team), only the latest runs, and that tick is the one recorded as fired, so a backlog never fires one tick on every later pass.

## Validating and registering a job

**Version.** A vault job's version is a digest of its workflow manifest, its entry script, and its job file. The formatting, comments, and key order of a manifest, and the `ready` key a job file carries, are not part of it, so tidying a file or setting `ready` is not a new version. A validation result and a completed dry run are each stored against the version they were made for, and stop applying when it changes: a `draft`, `validated`, or `registered` job whose version changed goes back to `draft`.

**Validation.** Static validation compiles the entry script and never runs it, so it has no effect on a vault or a server. It reports every failure it finds, each with a stable code and the file it concerns, and stores the result: a job that passes and is a `draft` becomes `validated` for its version. A failure the job owns for the version it holds, such as a script that does not compile or an owner no longer on the team, returns a `validated` or `registered` job to `draft` and suspends an `enabled` one with `workflow/validation-failed`; a check that could not be made, such as a server that could not be reached, is stored and changes no state. When the workflow manifest, the job file, or the entry script cannot be read, the job has no version, and the failure is stored without one. Such a failure is recorded once, not again on every pass, and a `validated` or `registered` job whose files can no longer be read goes back to `draft` like any other changed version.

| Code | Meaning |
| --- | --- |
| `validation/artefact-missing` | A file the job needs is not there, including the entry script |
| `validation/artefact-invalid` | A manifest does not parse or does not match its schema, including a job file with no `timeout_secs` |
| `validation/script-syntax` | The entry script or a module does not compile, uses `eval`, or imports what it cannot; the failure names the file, and the message gives the line and column |
| `validation/input-invalid` | The job's values do not satisfy the workflow's declared inputs |
| `validation/server-not-configured` | The workflow may call a server that is not configured |
| `validation/owner-unresolved` | The job declares no owner, or an owner who is not a member of the vault's team |
| `validation/tool-not-found` | A tool a shared workflow may call is not offered by its server |
| `validation/entity-unresolved` | The `entity` a job or its workflow declares is neither `vault` nor a configured entity |

A shared job is checked for the same things, against its package: the job file loads, the package and release resolve and have the workflow, the entry script compiles, the job's values satisfy the workflow's declared inputs, each `vault` input names a vault the primary server reports, every server a workflow's `permitted_effects` name is configured in `web.toml`, and each permitted tool is offered by its server, found by listing the server's tools and nothing else. A server that is configured but cannot be asked, and a primary server that cannot report its vaults, leave a check unverifiable: the job does not pass, its state is unchanged, and the outage is not recorded as a defect in the job. A shared job has no owner of its own, so there is no owner check.

The workflow manifest, the job file, and the entry script are checked independently, and each reports the first problem found in it. A job's inputs and owner are checked against its workflow, so when the workflow manifest is invalid they are not reported until it is fixed.

**When validation runs.** A `draft` job is validated automatically when its job file says `ready = true`, once for each version it has, and an operator can validate any job on demand whether or not it says so. `ready` is authored content that says authoring is finished: it sets no state and no schedule. A job whose job file does not say `ready` is not validated automatically. A failed result stands until the version changes, so a failure caused by something outside the job's files, such as a server missing from `web.toml` or a member missing from the team, is retried by validating on demand once it is fixed.

**Who asked, and the rules revision.** A stored validation says who asked for it (`by`: the validation pass, or the operator by hand) and under which revision of the validation rules it was made (`rules`). A result stored by an earlier release has neither and reads as the pass's, at revision zero. Tightening the rules would otherwise never reach a job whose files nobody edits, so a job whose stored result predates the current revision is validated once more, whether or not its version changed, on the first pass after an upgrade:

| Job | The first pass after the upgrade |
| --- | --- |
| `enabled`, no `owner` or an owner who is not a member | Suspended with `workflow/owner-unresolved`, and a task goes to the chief of staff |
| `enabled`, no `timeout_secs` | Suspended with `workflow/validation-failed` (failure `validation/artefact-invalid`), and a task goes to the owner, or the chief of staff |
| `enabled`, and the new rules pass | Unchanged, with no new trial: its version did not change |
| `registered` or `validated`, failing the new rules | Returns to `draft`, with the failure stored and shown |
| Any job whose script uses `eval` or imports anything but a declared or built-in module (revision 2) | Fails with `validation/script-syntax`: an `enabled` one is suspended with `workflow/validation-failed`, and a `registered` or `validated` one returns to `draft` |
| `draft` with `ready = true`, failing | The failure is stored, and one `validation-failed` task is raised |
| `suspended` | Untouched: resuming validates under the new rules |
| A shared job with no `timeout_secs` | Revalidated by the shared pass: an enabled one is suspended with `workflow/validation-failed`, and no task is raised |

A check that could not be made leaves the job as it is and is looked at again on a later pass. An upgrade can therefore suspend several enabled jobs at once, each with a task, and the job pages show the cause.

**Registering.** A dry run that completes for the job's current version registers a job that was validated for that version. A dry run completes when the script succeeds, or succeeds with warnings, and has used every call its scenario declares. A dry run of a job that is still a draft, that did not complete, or whose version changed since it was validated runs as usual and registers nothing.

## Operating a job

An operator moves a job through its lifecycle with `contextos-web jobs <command> <vault> <workflow-slug> <job-slug>`, or `jobs <command> --shared <package> <workflow-slug> <job-slug>` for a shared job. Every command calls `job_operation::perform`, the one operation that holds what an operator's request means, whichever kind of job it is about. A refusal exits unsuccessfully with an error that begins with its `workflow/` code, names the job's state, and says what would allow the change, and it writes nothing.

| Command | What it does |
| --- | --- |
| `jobs validate` | Validates the job now, whether or not its job file says `ready`, and stores the result in the job's state. The script is compiled and no tool is called. A `draft` that passes becomes `validated`. A failure lists every failure with its code and file and exits unsuccessfully. A job in any other state keeps the result and keeps its state |
| `jobs dry-run [--scenario <name>]` | The same as `jobs run --dry-run`. It connects to the configured servers to check the script's calls against their tool schemas, and no call reaches a server for real. A dry run that completes registers a `validated` job |
| `jobs schedule <cron> [--format text\|json]` | Sets the schedule. A `registered` job becomes `enabled` for the version it has now. The schedule of an `enabled` or `suspended` job is replaced, and its state does not change. Enabling a successor says which predecessor it retired and how the current and previous releases moved. See "Successor jobs" |
| `jobs schedule --clear` | Clears the schedule of an `enabled` job, which makes it `registered` |
| `jobs suspend --reason <text>` | Stops an `enabled` job on its schedule. The reason is required, stored, and shown. A run in progress finishes |
| `jobs resume` | Validates the job for the version it has now, then resumes it. See below |
| `jobs retire` | Takes an `enabled` or `suspended` job out of use and clears its schedule |
| `jobs list --shared [--format json]` | Lists the shared jobs. A suspended job shows its code, who suspended it and when, the run that failed it, the failure's code, and the reason (`suspension` with `code`, `reason`, `by`, `at`, `run`, and `failure` in the JSON, each left out when it is not recorded) |
| `jobs reactivate` | Brings a `retired` job back as a `draft`, unless it has a standing successor, or is a successor whose predecessor now has another |
| `jobs successor` | Makes a successor of a job, or with `--all` of every job of a workflow, so the job moves to another release. See "Successor jobs" |
| `jobs delete` | Deletes a `retired` job's stored state. A vault job's files are left as they are. A shared job's file is removed with its state, so it does not return as a `draft`: the file goes first, then the state, both under the state lock, so a failure part way leaves a `retired` job with no file, which a repeat of the command removes. A shared job whose key two files share, or whose key a usable file is tracked under while an unusable file (too large, not UTF-8, or unreadable) is named for it, is refused with `workflow/job-key-taken`, naming both files by name only, and nothing is removed. A file that cannot be removed, or a file under the job's own name that is too large, not UTF-8, or unreadable, is refused with `workflow/job-store-unavailable` and no state is written. The package folder, the workflow folder, and the file are checked again to be plain (no link or reparse point) immediately before the removal, and a swap in between is refused with `workflow/job-store-unavailable` with no state written. The folder is flushed after the removal and before the state is written |

A cron expression has five fields and is read as wall-clock time in the operator's zone (the contextos server's `[server] timezone`, else its host's), so `0 7 * * *` runs at 07:00 where the operator is all year, daylight saving included. Across a transition the Open Cron Pattern Specification applies: a fixed time the clock skips (spring forward) runs at the first time after the skip that day, a fixed time the clock repeats (fall back) runs once, at its first occurrence, and an interval or wildcard schedule skips the missing hour and runs in both passes of a repeated one. One that does not parse is refused before anything is read or written.

**Enabling is for the version that was proved.** A schedule is accepted only for the version the job was validated and dry-run for. When the files changed since, the request is refused and says to validate the job again, so an edit made after the dry run can never reach the scheduler unreviewed.

**Resuming validates first.** `jobs resume` validates the version the job has now and records the result. It resumes the job only when validation passes, which means no failure and no check that could not be made, and it resumes it for the version that validation was made for. A job resumed for a version other than the one it was enabled with has that version's first live run put on trial, as a new version is under probation. A resume while the vault's team cannot be used reports the owner check as unverifiable, stores no defect, and leaves the job suspended. A resume that is refused names every failure, or each check that could not be made, exits unsuccessfully with `workflow/validation-failed`, and leaves the job `suspended` with the failing result stored, so the job page shows it.

**What each command needs.** A vault job's state and files are reached through the primary MCP server, so every vault command connects to the servers in `web.toml`. A shared job's state is the host state file, so `schedule`, `suspend`, `retire`, `reactivate`, and `delete` need no `web.toml` and no server, and work on a host whose servers are down. Only `validate` and `resume` read a server, to list its tools and report its vaults.

**A job with no files.** `validate`, `resume`, and a schedule that enables need the job's files, and a job with none is `workflow/job-not-found`, naming where it was looked for. A state that cannot take the request is refused for that reason first, so a retired job whose files are gone is told what its state allows. A job found by the slug its file names is the job, whatever the file is called. A `retired` job whose files were deleted can still be reactivated or deleted, because those need only its state.

## Job pages

The same lifecycle is on the pages as on the command line, because both call `job_operation::perform`. A vault's workflows and the host's shared workflows are one hierarchy, workflows then jobs then runs, and the URLs follow it and the rules of `http-routing-response-contract.md`. A job is identified as `{package}/{workflow}/{job}` in both, in its address, in the commands, in claims, and in run records. A vault's pages take the vault's name first. A shared workflow belongs to a package and not to a vault, so its pages take `/server` where a vault's take `/{vault}`.

| Method | Vault path, beneath `/{vault}` | Shared path, beneath `/server` | Does |
| --- | --- | --- | --- |
| GET | `/workflows` | `/workflows` | Lists every workflow with its jobs, and `?state=` filters them. The strip above the list counts every job by state, whichever filter is in use |
| GET | `/workflows/{package}/{workflow}` | `/workflows/{package}/{workflow}` | One workflow, its releases, and its jobs |
| GET | `/packages` | `/packages` | The library releases the store holds, what each checks as, the workflow releases locked to each, and the jobs on them |
| PUT | `/packages/{name}/versions/{version}/execute/{command}` | `/packages/{name}/versions/{version}/execute/{command}` | `validate-dependents` validates every job on the workflow releases that lock the library release; `resume-dependents` resumes the picked ones. Both answer with the release's jobs block |
| GET | `/workflows/{package}/{workflow}/jobs/{job}` | `/workflows/{package}/{workflow}/jobs/{job}` | A job's status, actions, and run log |
| PATCH | `.../jobs/{job}` | `.../jobs/{job}` | Sets the schedule (`{"schedule": "0 3 * * *"}`, or an empty one to clear it), or a vault job's favourite (`{"favourite": true}`) |
| DELETE | `.../jobs/{job}` | `.../jobs/{job}` | Deletes a retired job's stored state |
| GET | `.../jobs/{job}/confirm-delete` | `.../jobs/{job}/confirm-delete` | The confirmation whose button is the `DELETE` |
| PUT | `.../jobs/{job}/execute/{command}` | `.../jobs/{job}/execute/{command}` | `validate`, `suspend` (with `{"reason": "..."}`), `resume`, `retire`, or `reactivate` |
| GET | `.../jobs/{job}/runs` | `.../jobs/{job}/runs` | The runs region alone, which it asks for again while a run is active. A fragment, so a browser is sent to the job's page |
| POST | `.../jobs/{job}/runs` | `.../jobs/{job}/runs` | Starts a run by hand: a live run for an empty body or `{"dry_run": false}`, a dry run for `{"dry_run": true}` |
| PUT | `.../jobs/{job}/runs/{run}/execute/cancel` | `.../jobs/{job}/runs/{run}/execute/cancel` | Asks the active run named `R-{n}` to stop |

A PATCH names exactly one thing to change. A body that names both, neither, or anything else is shown as an alert on the job's status and changes nothing.

**What a job page shows.** Three regions. The status region has the state and whether it needs attention, which a draft and a suspended job do. The schedule, in local time, naming the zone. Whether the next live run is the first of its version and so on trial. For a suspended job, its code, why, by whom, when, and the run that failed it, linked to its record. The last validation, whether it passed, with every failure's code, file, and message and every check that could not be made. When the job was last dry run. The actions are the ones the state allows and no others, and the page asks the transition table which they are, so it cannot offer what the operation would refuse: a draft or validated job offers only validation, a registered one offers validation and a schedule, an enabled one offers validation, a schedule, clearing it, suspending, and retiring, a suspended one offers validation, a schedule, resuming, and retiring, and a retired one offers only re-activating and deleting. The release region and the runs region are described below.

**How an action answers.** Every action is an HTMX request that replaces the job's status region, or for a run the runs region, and the pages add no client-side script. An action on the status answers with the refreshed region and, inside it, what happened, and beside it the runs region, because what a state allows decides whether a run may start. An action on a run answers with the runs region alone. A refusal is an alert in a `200`, because HTMX does not swap a `4xx`, and it keeps its `workflow/` code, names the state, and says what would allow the change. A failed validation, or a resume whose validation failed, lists the failures in the region. A request to delete asks first, through `confirm-delete`, and a deletion sends the browser to the workflow's page with `HX-Redirect`. An unknown command or job is `404`, and so is a workflow or job name that is not a URL-safe slug, because no job is tracked under one. A store that cannot be read is `502`, and a vault's state file that does not parse is that, and not a page of drafts. Validation is offered for a job in any state but `retired`, and a request to validate a retired job is refused. A run by hand is offered only where a run by hand is allowed, and a dry run in every state but `retired`.

**The runs region.** Beneath the status, with the run log. It shows the run that holds the job's claim, with when it started and a Cancel button, or says a run is starting when the claim is held and the run has not yet said which run it is. It offers Run now, which is shown disabled with "an active run in progress" while any run of the job holds the claim, and a request that gets past the button anyway is an alert with `workflow/job-busy` and starts nothing. It offers Dry run, which is never held back by a live run and never holds one back. It shows a dry run started from a page that has not ended, and how the last run started from a page of this process ended, which is where a run that failed before it had a record is told. While a run is active, or a dry run started here has not ended, the region asks for itself again every three seconds, so it follows the run to its end and then stops asking. A form post that is not HTMX, such as a card's Run now, starts the run and sends the browser to the job's page; one that is refused is a `409`. Run records of a dry run carry `dry_run: true` and the log marks them.

**The release region.** Between the status and the runs, from the same listing and report the workflow page is made from. It says which release the job names and whether that is current, previous, future, or retired, with its install date (`Previous, installed 3 November 2026`), and the release's version and library lock in its details: the library releases it is locked to, or `No lock yet` before a job on the release has passed validation. A release that is incomplete or damaged, or that the store no longer holds, is flagged with what is wrong. It links the job this one succeeds and the job that succeeded it, `No job` where there is none. Three flags raise an alert: `Needs a successor`, for a job in `enabled` or `suspended` on a previous or retired release that no job has succeeded; `Link mismatch`, for a successor whose `predecessor` key is now held by a job of another identity than the one its link recorded, which is the case that stops its enabling; and `Job file cannot be used`, which gives the reason a job file that does not load cannot be used, with its code. The region reads the vault's package store through the primary server, or the host's, and when it cannot be read it says so with the code and the rest of the page still draws.

**The whole chain's runs.** The run log shows the runs of the job and of every job it succeeded or that succeeded it, together and newest first, up to twenty in all. Runs are numbered from 1 under each job's own key, so each run is marked with its job and the version of the release it read, as `nightly-v2 · 2.0.0`, and a record that does not say which release it read is marked with its job alone. The chain is found from any of its jobs, retired ones included: each job's `predecessor` is followed back (or, when its file is gone, the job whose retirement record names it), and forward through the replacement each retirement record names, else the job that stands as its successor. A chain of three viewed from the oldest, the middle, or the newest job therefore shows all of the runs, and a retired job's page says `Succeeded by` the job that replaced it. A predecessor whose file has been deleted still shows its runs. `Needs a successor` still looks only at a standing successor. A chain whose jobs cannot be listed shows the job's own runs with a notice that the jobs it succeeded or was succeeded by could not be read, never those runs as if they were the whole chain's; run records that cannot be read show the log as unavailable. A job's card shows the job's own latest run, whatever newer runs its successors have.

**The workflow page.** `GET /workflows/{package}/{workflow}` lists the workflow's releases between its facts and its jobs, newest first, each as current, previous, future, or retired with its install date (`Current, installed 3 November 2026`). The version is in the release's details, with where it was installed from, when it was retired and why, and its library lock. Under each release the jobs on it are listed by lifecycle stage, each linked to its page. A current release with no job in `enabled` or `suspended` reads `Current, nothing running on it`. A job in `enabled` or `suspended` on a previous or retired release that no job has succeeded is flagged `Needs a successor`, naming it. A release that is incomplete, damaged, or cannot be verified is flagged with each finding, and a release whose declared templates are gone from source lists them, so a person sees at once that scheduled runs on it are refused. The page offers no way to install a release or make a successor: those are `workflows install`, `libraries install`, and `jobs successor`, and the API.

**Moving between the lists.** A row of tabs on the workflows lists and the packages pages moves between them. On a vault's screens the tabs are its workflows, the shared workflows, and its library releases. The shared screens have no vault in their address, so they have a tab for each configured vault's workflows, and the sidebar of the active vault: the one this browser was last in, kept in the `cc_active_vault` cookie, or the first configured vault when there is none or it is no longer configured. A band at the top of the sidebar and the vault switcher both say the page is the server view, and the band names the vault the menu below it belongs to. `/server/settings/` works the same way.

**The Server section.** Every page's sidebar ends with a Server section, below the vault's Knowledge group, whether or not any vault is configured. Its Workflows group links to `/server/workflows`, `/server/jobs`, and `/server/packages`. `GET /server/jobs` lists every shared job with the workflow it is under, with the same state strip and `?state=` filter as the workflows list.

**The packages page.** `GET /{vault}/packages` (a tab on the vault's workflows list, "Library Releases") and `GET /server/packages` (the same tab on `/server/workflows`) list every library release the store holds, including one no workflow release is locked to, which says so. Each release shows four checks, made again over the installed release each time the page is read: `Manifest` (the release reads whole: nothing listed is missing or changed and nothing else is there), `Syntax` (every module compiles), `Imports` (the libraries it requires are installed and read whole), and `Declared tools` (every literal `call_mcp` in its code is to a tool its `library.toml` declares, the same check installing it makes). A check shows `Passed`, `Failed` with each finding, or `Not checked` with the reason when an earlier check leaves it nothing to read, and a check that could not be made is never shown as a pass: when the vault's package store cannot be opened to make the checks, every library release is listed with all four as `Not checked` and the reason. A release that does not read whole is still listed when the store records it as a library (its ledger entry lists no workflow, or a workflow release's lock names it): its `Manifest` check fails with the condition and each problem, the other three show `Not checked` with the reason, and the jobs locked to it and the actions on them work as for any other. A release that does not read whole and that the store has no record of as a library cannot be told from a workflow release and is left to the Vault Health pane and `packages list`.

Under the checks, the workflow releases whose lock names the library release are listed, each as `{package} {workflow}, {kind}` and linked to the workflow's page. A library release older than the newest release of its library that a workflow release is still locked to is marked, because a library fix reaches a workflow only through a new workflow release and successors.

Beneath those, the jobs on the workflow releases that lock the library release are listed, each linked to its page with its state. Two actions follow, each a `PUT .../packages/{name}/versions/{version}/execute/{command}` that HTMX swaps into the release's jobs block (the library release is the entity and the dependents are the command, as in the routing contract):

- `validate-dependents` validates every listed job through the operator operation a job's own page uses, so a validation is recorded; a job whose state allows no validation (a retired job) is skipped, shown as skipped and not as a failure, and is not asked.
- `resume-dependents` resumes the jobs the person ticked (only a job the transition table lets resume has a checkbox), each through the per-job `resume`: the job is validated for the version it has now and resumed only when that passes, and the suspension record is unchanged. The body is the checkbox form as JSON, `{"package/workflow/job": "on"}`; nothing picked, a body that is not that form, and a pick that is not a job on a workflow release locking the library release do nothing and are told in the block.

Each job gets its own result line, with the failures validation found when it did not pass. One job failing or being refused never stops the rest, and the jobs are read again afterwards, since a validation can move a job off the library release. Installing a release and making a successor stay with the command line and the API. The command line needs no matching command: `jobs validate` and `jobs resume` already do the same per job, for a vault or `--shared`. The host's page also lists every installed package with its exports and the installed packages that depend on it, and the releases that cannot be read.

**The list.** `?state=` is a state name or `attention`, a chip for each is offered, and a name that is neither is `400`. Every job file is listed, whether or not the job registers, so a draft that fails validation is visible and can be validated from its page. A job that does not register is shown by its slug and says so, and a workflow whose manifest does not register is flagged, and so is a shared workflow that no installed package provides now. A shared workflow takes its name from its package and comes from the package's newest installed release, and one whose package or workflow name is not a URL-safe slug has no page and is left out with a warning in the log.

**Shared jobs.** A shared job's page reads the host's package store, job files, state file, and run records, and acts on the host state under its lock. It has no favourites. Its run log is the host's run records, newest first, up to twenty. A shared page is drawn in the shell, which takes its navigation from the primary MCP server, so a full page needs that server connected, while the fragments an action swaps in read the host alone. `shared` is a top-level path like `settings`, so a vault with that name would not be reachable at its own root.

## Claims and active runs

A job's claim is an exclusive operating-system lock on a file on the host, held for the whole of a live run, for a vault job as for a shared one. A vault job's is `claims/vaults/{vault}/{package}/{workflow}/{job}.lock`, with the vault named without regard to ASCII case, as the server matches it. It never touches the vault, so `.contextos/jobs.toml` is written only when an operator, validation, or a probation outcome changes a job, and a run writes nothing to it. The claim directory must be on a local disk, because advisory locks are not dependable on network filesystems, and two hosts that run jobs of one vault do not exclude each other, so a vault's jobs are run from one host: a second host would also mark the first host's live run `interrupted`.

- A run of one job never overlaps another run of it, in any process: a manual run finds the claim held and is refused with `workflow/job-busy`, and a scheduled tick that finds it held leaves a finished `skipped` record. Different jobs, even in one vault, run at the same time. A dry run takes no claim.
- Beside the lock, `{job}.run` says which run holds it and when it started, written once the run has its number, and `{job}.cancel` is a request to stop that run. Both are removed when the run ends. The lock decides: a file a dead process left is never trusted, because a run is reported as active only while its lock is held, and a request that names a run that does not hold the claim is refused.
- Looking at a claim takes a shared lock for an instant, so a run that tries for the claim in that moment tries again a few times before it believes the job busy.
- A vault job's run number is kept on the host, `{job}.next-run-id`, taken under `{job}.bookkeeping.lock`, and is never below the highest of the job's latest 100 run records, so a lost counter never gives a number a record already has. Numbers are per job. The last cron tick that fired is `{job}.last-fire`, recorded forward only before the run starts, so one tick fires once across a restart and between two processes. The highest run already examined for a `run-failed` task is `{job}.run-failed-examined`, moved forward only, under the same lock and written atomically (see "Which failures are new"). The vault's `last_scheduled_fire` is only where an operator's last schedule change counted the schedule from.
- A run that takes the claim marks every record of the job still `running` that is not a dry run as `interrupted`, with the time and the reason a shared run's record gets, because holding the claim proves the process that wrote it is gone. A dry run's record is written when it ends, and carries `dry_run: true`, so one is never mistaken for an abandoned live run.
- A job page keeps a table, in memory, of the runs it started from a page of this process. It is how a dry run, which takes no claim, is seen while it runs, and how a run that failed before it had a record is told. It is a convenience of the page and never the record of a run.

## Shared workflows and jobs

Shared workflows come from the host package store. All host data lives under the platform app-data directory, beside `packages/`:

```text
<app-data>/
├── packages/{package-name}/{version}/   # with packages/ledger.toml and packages/.lock beside them
├── workflows/{package-name}/{workflow-slug}/jobs/{job-slug}.toml
├── claims/shared/{package-name}/{workflow-slug}/{job-slug}.lock
├── claims/vaults/{vault-name}/{package-name}/{workflow-slug}/{job-slug}.lock
├── claims/.../{job-slug}.{run,cancel,next-run-id,last-fire,run-failed-examined,bookkeeping.lock}   # beside a vault job's claim
├── runs/{package-name}/{workflow-slug}/{job-slug}/{YYYY}/{MM}/R-{n}.md
└── state/shared-jobs.toml        # each job's lifecycle state, schedule, and recorded facts, with a .lock beside it
```

A package is installed into the host's store with `contextos-web packages install --shared <path>`, or into a vault's with `contextos-web packages install <vault> <path>`, where the path is a `.tar.gz` bundle or a folder holding the same files; a folder is only read, and both are verified against the manifest's templates, modules, listed files, and digest before anything reaches `packages/`. The manifest lists every file of the release but itself under `[[files]]`, each with its `path` and the SHA-256 of its bytes (`hash`, 64 hexadecimal characters); a file the manifest does not list, a listed file the bundle lacks, and a file that does not hash to what is listed are each refused, naming the file. The `digest` is unchanged: one SHA-256 over every listed file in path order, each as its `/`-separated path, a zero byte, its bytes, and a zero byte. Installing a newer version leaves older ones installed until retention removes those nothing keeps (an install of an older release than the one a workflow runs, with no job naming it, is removed again at once and says so), installing the same bundle again changes nothing, and other content under a version already installed is refused as a duplicate identity. A version is exactly `major.minor.patch`; a pre-release version is refused.

The store is kept by the package engine that also keeps a vault's store. `packages/ledger.toml` records every release ever installed and every removal; a store that holds releases and has no ledger, or whose ledger cannot be read, refuses every install and removal while reads and runs go on, until the ledger is restored from a copy or rebuilt from releases that all read whole. An install is staged in a `_staging-` folder and made visible with one rename, and a removal renames the release into a `_removing-` folder before the ledger records it and the folder is deleted, so a reader never sees a partly written or partly removed release. The advisory lock on `packages/.lock` serialises installs and removals across processes, and a caller that finds it held fails at once.

A vault keeps its own store of the same shape in `.contextos/packages/` inside the vault, and `contextos-web` never reads the MCP server's `config.toml` to find it. It asks the vault's primary server: one `vault_info` call per session reports each vault's name and root path, and the server runs on the same host, so that path is usable here whether the server is reached over stdio or HTTP. The answer is kept for the rest of the session. If `vault_info` fails, does not list the vault, lists it without a path, or names a path that is not a folder on this host, the store cannot be located: validation reports the job unverifiable, a run is refused with `workflow/package-unavailable`, and the message names the cause. Reads of the store go through the primary server like any vault file. Writes go through the write pipeline and take no vault write lock, because the server never writes below `.contextos/packages/`; a release is staged in a `_staging-` folder and renamed into place, so a reader never sees a partial one. The store's lock is `.contextos/packages/.lock`, with its ticket queue (`.lock.queue/` and `.lock.queue.lock`) beside it. It serialises every `contextos-web` process, the service and the command line alike, that changes that vault's store, and a caller that finds it held fails at once. The package engine treats those three entries as part of the store, so listing, read checks, ledger rebuilds, and retention neither report them as strays nor touch them.

A package's name, and every name in its `libraries`, is lower-case ASCII letters, digits, and hyphens, starting with a letter or digit, at most 64 characters. A name Windows reserves for a device (`con`, `nul`, `com1`, and so on) and `contextos`, which names the built-in script library, are refused. The name is the package's folder in the store, so a manifest, command, or URL naming any other form is refused before the store is touched: the HTTP API answers `400` with `packages/invalid-name`. A release installed before names were checked, under a folder that is not a package name, is skipped with a warning naming the folder, and is removed by hand.

A shared job is a file at that path, read with no vault and no MCP server configured, and written with the commands under "Creating and changing a shared job" (a file placed by hand is still read). It accepts only these keys, and any other fails that job's load:

```toml
name         = "Copy meeting notes"   # required
slug         = "copy-meeting-notes"   # optional, defaults to the file stem; when given it must equal the job key
timeout_secs = 120                    # required: whole seconds, greater than zero
version      = "1.2.0"                # required: the exact release the job runs
predecessor  = "copy-meeting-notes-v1" # optional: the key of the job of the same workflow this one succeeds, written by `jobs successor`
log_level    = "info"                 # optional minimum log level: debug, info, warn, or error

ready        = true                   # optional: authoring is finished, so validation may run, and nothing else

[inputs]
source_vault      = "client-a"
destination_vault = "client-b"

```

A shared job has no `schedule` and no `owner` of its own. When `web.toml` has an `[owner]` table with a `name`, that name owns every shared job; with no table, a shared job has no owner.

**Release selection.** A run of a saved job uses exactly the release its job file names, and a one-off run the release `--release` names (`current` by default), and never the newest. A release the store has recorded but no longer holds whole (removed, damaged, or incomplete) ends the run with `workflow/package-unavailable`, and no other version is used. A one-off run of an exact version the store has never recorded is refused with `workflow/job-not-runnable`. The choice is fixed at the start, so an install during a run cannot change the script it executes. The resolved version and content digest are logged at INFO with the names, never the values, of the inputs, and so is each library release the run resolved (`library`, `version`, `digest`). The run's record carries `libraries`, a list of `{name, version, digest}` in name order, when the run resolved any.

**State.** A shared job has the same lifecycle as a vault job, held in `state/shared-jobs.toml` under the app-data directory, keyed by package, workflow, and job: its state, the schedule the operator set, the last tick that fired, the first-run flag, and what validation and a dry run found, each against the version it was made for. The Web scheduler and a command-line process can both change the file, so every change is made under an exclusive advisory lock on `shared-jobs.lock` beside it, taken only for the read, change, and write, never across a run, and applied to the file as it is then. Like the claims directory it must be on a local disk. A file that does not parse is refused and left as it was, and a run is never started from state that cannot be read.

**Version.** A shared job's version is a digest of its parts, each length-prefixed: a `shared` tag, its package's content digest, the release its job file names, its job file without the `ready` key, and its package's `manifest.toml` as installed, parsed and written back without `digest`, so a release reinstalled with another manifest is a new version although its files are the same. When the job resolves libraries, a `libraries` tag follows and then each library in name order: its name, release, content digest, and manifest without `digest`; a job that resolves none has no library parts. Every library the package depends on is in the version, imported from or not, because its requirement can stop a run. Installing a newer release of the job's package changes nothing about the job, which names the release it runs. A job whose package or named release is not installed, or whose libraries do not resolve, has no version. Validation, the version, and a run are each made from one snapshot, and a scheduled run of a version nobody reviewed does not start.

**Start of a run.** In order: load the job, check that its state allows the run, select the release, validate the inputs, check each `vault` input against the primary server, take the claim, start the run's record, run, finish the record, delete the job's records past the retention window, release the claim, and settle what the run decides about the job (probation, below). A failure before the claim leaves no claim behind and no record. A run by hand needs the job `registered`, `enabled`, or `suspended`, and a scheduled run needs it `enabled`; anything else is refused with `workflow/job-not-runnable` before anything is claimed.

**Scheduling.** The Web scheduler runs an `enabled` shared job when its stored schedule falls due, the same way a run by hand runs it, with `trigger: scheduled` in its record. The tick is recorded in the host state under the lock before it is run, so one tick fires once across a restart and between two processes. Each due job runs on a task of its own, so two jobs never wait for each other, and none needs a vault, so shared jobs run with none configured. A tick that finds the job's claim held is not run and not run late: it leaves a finished record with `status: skipped`, as a vault job's overlapping tick does. A due job whose current version has not been reviewed is not started that pass, and its tick stays due until a pass has reviewed it. The run checks this itself, against the release it resolved, so a release installed between the scheduler's check and the run is refused with `workflow/job-not-runnable` rather than run unreviewed. A run by hand is the operator's own act and is not held back, and what it settles is about the release it ran. The scheduler starts shared jobs alongside its pass over vaults and not after it, so a slow vault server never delays one.

**Claim.** A run of a saved shared job holds an exclusive operating-system lock on its claim file, `claims/shared/{package}/{workflow}/{job}.lock`, for its whole length, and a second run of the same job is refused with `workflow/job-busy`. Different jobs never block each other, including when they act on the same vault. The claim reads and writes no vault file. It is released on success, failure, timeout, cancellation, or a panic. The file is never deleted and its existence means nothing: a process that dies loses its lock, so the next run takes the claim and nobody clears anything. The claims directory must be on a local disk, because advisory locks are not dependable on network filesystems. A one-off run takes no claim.

Two job files that would share one job key, because one names the other's file stem as its `slug` or both name the same one, are both set aside with a warning until one is renamed, since they would otherwise take turns putting each other's version on probation.

A shared job file is limited to 64 KiB and an entry script to 1 MiB; a larger file ends the run before it starts. When a saved shared job fails and `web.toml` has an `[owner]` table, the run summary's `owner` names that owner.

### Creating and changing a shared job

A shared job file is authored with `contextos-web jobs create|show|replace|list --shared`, never by writing the host's data folder with an editor or a session's own file tools. The commands need no running server and work while servers are down; a missing `web.toml` is fine, but one that cannot be used is refused (see below). Every command takes `--format text` (the default, lines for a person) or `--format json`, one JSON document on standard output. The flag is the one run summaries use.

| Command | What it does |
| --- | --- |
| `jobs create --shared <package> <workflow> <job> --file <path or ->` | Stores the TOML, read from the file or from standard input for `-`, at `workflows/{package}/{workflow}/jobs/{job}.toml` beneath the app-data directory. The job is a `draft` with nothing stored for it |
| `jobs show --shared <package> <workflow> <job>` | Prints the job file and its content hash, finding the job by the key it is tracked under |
| `jobs replace --shared <package> <workflow> <job> --file <path or -> --expected-hash <sha256>` | Replaces the file when its current hash is the one given, which is the hash `show` printed |
| `jobs list --shared` | Lists every job file and every stored state entry once, with its package, workflow, job, file (when its stem is not the job key), name, state, schedule, `ready`, version, content hash, and problems |

**Same checks as a placed file.** The TOML is read with the 64 KiB limit enforced before it is held, then loaded exactly as a file placed by hand is, so what one accepts the other accepts. The bytes given are stored exactly: no change to line endings, comments, or key order. A file that does not load is refused with `workflow/job-file-invalid`, which names the violation and carries the validation code a placed file would get (`validation/artefact-invalid`) in its message and in the JSON `cause` field, and nothing is written. The key the job would be tracked under, which is its `slug` or else the file's stem, must equal `<job>`, or the request is refused with `workflow/job-key-mismatch`, so the command's key is always the job's key. `replace` checks the new bytes against the name of the file that is actually tracked under `<job>`, so replacing a file stored under another name (`other.toml` with `slug = "nightly"`) with bytes that drop the `slug` is refused and the file is left unchanged. A schema or syntax error names the line and column it points at and never repeats the source line; a value in the job's `[inputs]` table is never repeated in an error, though a type error for a typed key (`timeout_secs = "soon"`) names the offending value. Each of the package, workflow, and job must be a slug and must not be a Windows reserved device name (`con`, `prn`, `aux`, `nul`, `com1` to `com9`, `lpt1` to `lpt9`), or the request is refused with `workflow/job-key-invalid`, so a job authored on one platform is portable to another.

**Create never overwrites.** Identical bytes at the path succeed and report `unchanged`; different bytes are refused with `workflow/job-exists`. Another file already tracked under the same key (its `slug` names `<job>`) is refused with `workflow/job-key-taken`, and so are `show` and `replace` of a key that a usable file is tracked under while an unusable file is named for it, with both files named and nothing written. Stored state for the key with no file, such as a retired job whose file has gone, is refused with `workflow/job-state-remains`: delete that job's state first, retiring the job if it is not already retired. The file is written to a temporary file in the workflow folder, flushed, and moved into place with a rename that fails when anything is already there, so a file that appeared in the meantime is never replaced, on Linux, macOS, and Windows alike. Missing package and workflow folders are made. A created file takes the permissions a hand-placed file would on Unix (the process umask applies), and a replaced file keeps the permissions it had. A package or workflow that is not installed is accepted and reported as a `workflow/package-unavailable` warning ("is not installed"); the job stays a `draft` and cannot validate until the release is installed. When the package store itself cannot be read, the warning has the same code and says the package and workflow "could not be checked", because it is not known whether they are installed.

**Replace is checked by hash.** A current hash that is not the one given is refused with `workflow/job-changed`, which gives the current hash, so a hand edit made after `show` is never lost. Identical bytes succeed and report `unchanged`. Replace changes only the file and writes no state: a replacement is a new version of the job, and the lifecycle's version rules apply on the next pass or on `jobs validate`. A `draft`, `validated`, or `registered` job returns to `draft`, an `enabled` job's new version is validated and put on probation, and a `suspended` job stays suspended. Replace is allowed in every state, including `retired`, as a hand edit is.

**A jobs folder that cannot be listed.** Only a `path/not-found` refusal of a workflow's `jobs` folder means it has no jobs. Any other refusal, and an answer that is not a listing, leave its jobs unknown: that is an outage, the pass or command reading it stops, and no lifecycle state changes.

**Job files that cannot be used.** A regular `*.toml` file under the jobs folder that is over 64 KiB, not UTF-8, or unreadable is never silently dropped. `list` shows it with a `validation/artefact-invalid` problem whose reason is `too large`, `not UTF-8`, or `unreadable: <kind>` (never its content or path), and a file whose stem is not a slug is listed with `workflow/job-key-invalid`. `show` and `replace` of a job whose file is unusable are refused with `workflow/job-file-invalid` (cause `validation/artefact-invalid`), not `workflow/job-not-found`, and the file is untouched. `create` at that path is `workflow/job-exists`, and `delete` refuses as above. The scheduler's discovery still skips these files.

**One lock.** Every check and every write of an authoring command is made under the host state file's lock, the lock every lifecycle change and the scheduler already take, so a create never races a state change or another create. Create, show, and replace never write the state file. `list` takes no lock.

**Links.** The package folder, the workflow folder, and the job file are checked and refused with `workflow/job-store-unavailable` when any is a symbolic link, a Windows junction, or any other reparse point (the `FILE_ATTRIBUTE_REPARSE_POINT` attribute), and the checks are repeated at the point of the write. Errors name a job by its key and never print a host path.

**Output and exit status.** A result is the one thing a command writes to standard output, and a refusal is part of that result: `{"error":{"code":"workflow/...","message":"...","cause":"..."}}` for `--format json` (`cause` only for `workflow/job-file-invalid`), and a line beginning `refused` and the code for `--format text`. A refusal exits with status 1 and prints nothing to standard error. A usage error, such as an unknown `--format` or an `--expected-hash` that is not 64 hexadecimal characters, exits with status 2 and is reported before anything is read. Diagnostics go to the configured log like every other `jobs` command: when `web.toml` loads, its `[server]` `log_file` and `log_level` apply, and when it is absent the default logging writes to standard error. A `web.toml` that is there and cannot be used (unreadable, malformed, an unknown or invalid setting) or a `log_file` that cannot be opened is never ignored: the command does nothing and refuses with `workflow/config-invalid`, naming only the class of failure and no path, as its result (the JSON error document, or the `refused` line), with status 1 and nothing else on standard output. The server-less operator commands (`schedule`, `suspend`, `retire`, `reactivate`, and `delete` of a shared job) refuse the same way through their usual error line on standard error. A refusal is logged at DEBUG only, so at the default level a refusal writes nothing to standard error, and a create or replace that wrote bytes, or a delete, of a shared job file is logged at INFO with the package, workflow, and job and nothing else (no content, no path). A create or replace that found the file already holding the bytes (`unchanged`) wrote nothing and is logged at DEBUG at most. The result is the only output on standard output. `create` and `replace` print the outcome (`created`, `replaced`, or `unchanged`), the package, workflow, and job, the new content hash, and any warnings, each with a `code` and a `message`.

A `list` problem is one of: a file that does not load (its `validation/` code), a file that is too large, not UTF-8, or unreadable (`validation/artefact-invalid`), a file whose name is not a slug (`workflow/job-key-invalid`), a key two files share (`workflow/job-key-taken`, on each of them), or stored state with no file (`workflow/job-not-found`).

The Web pages stay lifecycle-only: they show, validate, schedule, and delete a shared job, and never author one. Packages do not ship default jobs.

## Successor jobs

A job names one release of its workflow, and a job that is `enabled`, `suspended`, or `retired` never changes it. A job moves to another release through a successor: a new job of the same workflow whose file is its predecessor's with `version` replaced and `predecessor` added, so the predecessor's retirement is part of enabling the successor. A job that was never enabled (`draft`, `validated`, or `registered`) has no use for one: its `version` is changed in place with `jobs replace`.

```text
contextos-web jobs successor <vault> <package> <workflow> <job> <new-job> [--to future|previous|<version>] [--format text|json]
contextos-web jobs successor --shared <package> <workflow> <job> <new-job> [--to ...] [--format ...]
contextos-web jobs successor <vault> <package> <workflow> --all [--to ...] [--format ...]
contextos-web jobs successor --shared <package> <workflow> --all [--to ...] [--format ...]
```

**The release.** `--to` defaults to `future`, the newest future release recorded for the workflow. `previous` names the recorded previous release, which makes a return successor for a job on the current release. An exact version names that release. A release that is not installed in the store is refused with `workflow/package-unavailable`, and so is `future` when none is recorded; `previous` when none is recorded is `workflow/transition-refused`.

**What is written.** The successor's file is the predecessor's with `version` set to the release, `predecessor` set to the predecessor's key, and a `slug` line, if it has one, set to the new key; everything else, comments and `ready` included, is kept as written. It is a new `draft`: it is validated, dry-run, and enabled like any new job, its parameters are edited with `jobs replace` while it is a `draft`, and enabling it retires its predecessor. The file is checked for syntax, schema, key, and release fields, and its `[inputs]` are not checked against any workflow: a successor that names a release whose inputs differ is written as a `draft` carrying the old inputs, which are taken up with `jobs replace` and judged against the release when it is validated. The same check applies in a vault and in the shared store. A file is never written over another, and the command changes no state.

**Refusals.** Each is `workflow/transition-refused` naming the reason, except as noted. Nothing is written for any of them.

- The predecessor is `draft`, `validated`, or `registered`: change its `version` in place instead.
- The predecessor already has a standing successor: one that has not been retired, or one that was enabled, which the predecessor's retirement records. A job retired by its successor's enabling has that successor standing for good, even after the successor is retired, so neither another successor nor a re-activation is allowed. The same rule decides `jobs create` for a key a standing successor names, in a vault and in the shared store, reading `predecessor` from each job file's text alone.
- The release is a retired release, or is older than the predecessor's release and is not the previous release, or is the previous release while the predecessor is not on the current release.
- The predecessor is `retired` and was never `enabled`, which ends its chain: it was retired from `draft`, `validated`, or `registered` and never re-activated and enabled since. Make the successor of the job before it instead. The validation of a successor that names such a job fails with `validation/artefact-invalid` on `predecessor`.
- The new key is already a file (`workflow/job-exists`, whose message gives the way on: delete that job if it is retired, or give the successor another key), is tracked under another file or is the `predecessor` of a job that has not been retired (`workflow/job-key-taken`), has stored state with no file (`workflow/job-state-remains`), or is not a usable job key (`workflow/job-key-invalid`).

A predecessor that is already `retired` after having been `enabled` may have a successor, and a successor may name the release its predecessor is on.

**The link is checked at every validation.** The predecessor must exist, be of the same workflow, be `enabled`, `suspended`, or `retired` after having been `enabled`, and have no other standing successor. Once the link is recorded, a `predecessor` key that no job holds passes only when no job in the workflow holds the identity the link recorded, which means the predecessor was deleted; while that job still exists under another key, the file fails with `validation/artefact-invalid` on `predecessor`, and an enabling over such a file is refused with `workflow/transition-refused`, naming the job that holds the recorded identity, and the advice to validate again. A key held by a job of another identity fails in the same way. A link also records the key the job file named, and a file that names another key afterwards fails on `predecessor`, so a predecessor deleted and then replaced by a different name in the file is caught as an edit.

**One job of a chain is in use.** A predecessor and its successors form a chain, and at most one job of it is `enabled` or `suspended`. Besides the refusals above, three rules keep it so, in a vault and in the shared store:

- A live run by hand (`jobs run` and `workflows run` of a saved job without `--dry-run`, and the page's Run now) is refused with `workflow/job-not-runnable`, naming the other job, for a job with a standing successor and for a successor whose predecessor is `enabled` or `suspended`. A dry run is never refused for this, so a successor is tried by dry runs before it is enabled. The refusal comes before the claim, so it takes no run number and writes no record.
- Enabling a successor (`jobs schedule`, the page, or the API) is refused with `workflow/job-busy` while a run holds its predecessor's claim, because enabling retires the predecessor. Nothing is written, and the command is repeated once the run has ended. A predecessor that is already retired is not retired again, so its claim does not matter. The claim is read when the enabling is prepared and not held until the state is written, so a run that takes it in the instant between the two is not seen.
- The scheduler re-reads the job's state after it has taken the job's claim for a due tick, and runs nothing, releasing the claim, when the job is no longer `enabled`. A scheduled run of a shared job makes the same check once it holds the claim. So a job retired by an enabling between a tick falling due and the claim being taken does not run.

**What enabling reports.** `jobs schedule` on a job that goes from `registered` to `enabled` prints `... is now enabled.` followed by its schedule, the predecessor it retired when it retired one, and how the current and previous releases moved, as in `Current moved from 1.0.0 to 2.0.0; previous moved from none to 1.0.0.` (a pointer with no release is `none`). `--format json` prints one document, `{"message": "...", "enabling": {"schedule", "retired", "current": {"from", "to"}, "previous": {"from", "to"}}}`; `enabling` is `null` for any change that is not an enabling, and `retired`, `current`, and `previous` are `null` when the enabling did not retire a job or move a release. The page and the `PATCH` that sets a schedule answer with the same sentences in the job's status region.

**`--all`.** Makes one successor for every job the workflow's state records as `enabled` or `suspended` on the current release, whatever its file holds. A selected job whose file is unusable, is missing, has several claimants on its key, or names no exact version is refused and reported, so it is never skipped quietly. Each new key is the predecessor's key with any trailing `-v{major}-{minor}-{patch}` removed and `-v{major}-{minor}-{patch}` of the release added, so `nightly-v1-0-0` and `nightly` both become `nightly-v2-0-0`. When any key is taken or any job would be refused, nothing is written and every refusal is reported. `--all --to previous` makes return successors for the whole workflow before any is enabled; enabling the first swaps the current and previous releases. The result says, for each successor and for a step of any size and in either direction, whether it carries over without parameter changes, which it does only when the two releases declare the same inputs and outputs, or else lists the changes to the workflow's inputs and outputs the job must take up while the successor is a `draft`, read from the interfaces the store's ledger holds for the two releases. A successor naming its predecessor's own release carries over. When the ledger does not hold the interfaces, the result says that the changes are not known.

**Locks.** A shared successor is checked and written under the host state lock. A vault successor is checked and written under the vault store lock (`.contextos/packages/.lock`), through the vault's primary server, so no release is installed, removed, or kept differently between the checks and the writes. A busy store lock turns the command away at once with `packages/store-locked`. When a write fails part way through `--all`, the successors already written are reported with the failure, and a repeat finds their keys taken.

**Output.** `--format json` prints one document: `store`, `package`, `workflow`, `release` (`version` and `kind`), and `successors`, each with `predecessor`, `job`, `from`, `to`, `step` (`same`, `patch`, `minor`, or `major`), `hash`, `carries_over`, and `interface_changes` (each with `side`, `name`, `change`, `required` for an added field, and `text`; `null` when the changes are not known). A refusal carries `store`, `package`, `workflow`, `release` once it is known, `successors` (the ones already written, after a write that failed part way through `--all`), and `refusals`, as `{"error":{"code","message"},"store","package","workflow","release","successors":[...],"refusals":[{"job","code","message"}]}`, where `error.code` is the code every refusal shares, or `workflow/transition-refused` when they differ, and exits with status 1. A usage error, such as a word count that fits no form above, exits with status 2.

**API.** The same operations are available as `PUT /api/v1/{vault}/workflows/{package}/{workflow}/jobs/{job}/execute/successor` with a body of `{"new_job": "...", "to": "..."}` (`to` is optional), and `POST /api/v1/{vault}/workflows/{package}/{workflow}/jobs/execute/successor` with `{"to": "..."}` (the body is optional). The host's workflows use `shared` in place of the vault: `/api/v1/server/workflows/{package}/{workflow}/jobs/...`. Both answer 200 with the JSON the command prints. A refusal answers with the error document and a status by code: 404 for `workflow/job-not-found` and `workflow/package-unavailable`, 409 for `workflow/transition-refused`, `workflow/job-exists`, `workflow/job-key-taken`, `workflow/job-state-remains`, and `packages/store-locked`, 422 for `workflow/job-file-invalid`, `workflow/job-key-invalid`, and `workflow/job-key-mismatch`, and 500 for `workflow/job-store-unavailable` and `workflow/state-unavailable`, including a package store that could not be read to check whether the release is installed, and for `workflow/server-unavailable`. A body that is not JSON, names an unknown field, lacks `new_job`, or names a `to` that is not `future`, `previous`, or an exact version is 400 `workflow/request-invalid`, an unknown vault is 404 `vault/not-found`, and a vault whose server is not configured or cannot be reached is 500 `workflow/server-unavailable`.

## Run records, cancellation, and retention

A run of a saved shared job writes one Markdown note at `runs/{package-name}/{workflow-slug}/{job-slug}/{YYYY}/{MM}/R-{n}.md`, in the frontmatter shape of a vault run record (`type: job-run`) plus the package name, version, and content digest, and `dry_run: true` or `false`. A one-off run takes no claim and writes no record. Nothing is written to any vault. Run numbers are per job, kept in `next-run-id` beside the records, taken under a short lock of their own (`next-run-id.lock`) so that a run holding the claim, a skipped tick, and a dry run, which cannot or need not take it, never get the same number, and never reused after a record is deleted. A scheduled tick that was not run leaves a record with `status: skipped`, no calls, and the reason in `error`; it is deleted by retention like any other record that is not running. If the counter is missing the next number follows the highest record found, and a counter holding anything but a number stops the run being recorded, with the run going ahead, until the file is deleted.

The note is written `status: running` before the script starts and rewritten after every call and at the end, so a run that dies leaves the calls it completed. Its body is a table of the calls the run made, in order: server, tool, outcome, and duration. Outcomes are `succeeded`, `failed: <code>`, `refused: <code>` for a call the scope refused before it was issued, and `abandoned in flight, outcome unknown`. Inputs are named in the frontmatter and their values are never written. So are the inputs overridden for the run (`overrides`) and the outputs the script set (`outputs`); a record written before these fields existed reads back with both empty. A call's arguments and result are never written. Past 1000 calls the log stops listing and says how many further calls were not logged. A failed run's `error` is the failure's own message cut at 1 KiB, which can include text a server or the script put in it. A record that cannot be written is logged and the run goes ahead without one.

| `status` | Meaning |
| --- | --- |
| `running` | The run has not ended |
| `succeeded` | The run completed |
| `degraded` | The run completed and its script counted at least one reason |
| `failed` | The run failed. `error` begins with the `workflow/` code when there is one, which includes `workflow/timed-out` |
| `cancelled` | The operator cancelled the run |
| `interrupted` | A later run found the record still `running` and took the claim its writer held, so the process is gone. The last call may or may not have taken effect |

**A vault job's run record.** A vault run's record is `memory/jobs/{package}/{workflow}/{job}/{YYYY}/{MM}/R-{n}.md`, `type: job-run`, with the run's `trigger`, `status`, `dry_run`, `started`, `completed`, `error`, `code`, and `script_started`, and `package`, `version`, and `digest` of the release the run read and `libraries` (`name`, `version`, and `digest` for each locked library release) when it read one. `entity` is the job's resolved entity (see "A vault job's file"), never a fixed name. `code` is the stable `workflow/` code a failed run ended with, and `null` when the run did not fail or the failure had no stable kind: a task for a failure reads it and never parses the `error` text. `script_started` is `true` once the run's script has started and `false` when the run ended before it did (an unreadable entry script or module, an unavailable server, an invalid input, a busy or unrunnable job), and it is `false` while the run is still going. A record from before the field existed is read as started unless its `code` is one of `workflow/entry-unreadable`, `workflow/server-unavailable`, `workflow/input-invalid`, `workflow/job-busy`, `workflow/job-not-runnable`, and `workflow/state-unavailable`.

**Timeout.** A run's timeout is checked between script steps and also while a call is waiting for its answer. A call that has not answered when the timeout passes is abandoned, logged `abandoned in flight, outcome unknown`, and the run fails with `workflow/timed-out`; the server may or may not have acted on the call.

**Cancellation.** A page, or another process, can ask an active run to stop by writing a request beside its claim, `{job}.cancel`, naming the run. The run polls for it every second and stops at its next script step, as for Ctrl-C, so a run is cancelled by whichever process asks, and a request names the run it is for, so one that is left behind never stops a later run. A request is refused unless that run holds the claim now. Ctrl-C, or a termination signal, stops a command-line run at its next script step, which a script cannot catch, or abandons the call in flight, then releases the claim and records `cancelled`. Vault runs are interrupted exactly as shared runs are, for `jobs run` and `jobs dry-run` and for `workflows run`: SIGINT and SIGTERM on Unix, and Ctrl-C and console close on Windows. A vault live run's record is completed `cancelled`, a vault dry run's record is written `cancelled`, the claim is released, and the summary says `cancelled` and exits 130. A second signal ends the process at once with status 130 and no summary, leaving the record `running` for the next run of the job to mark `interrupted`. Calls the run had already made stay made: nothing is retried, resumed, compensated, or rolled back, and a call that was in flight may or may not have been acted on by its server.

**Retention.** When a shared run ends, that job's records older than the retention window are deleted, whatever their status except `running`, and a month or year folder that empties is removed. There is no archive. Other jobs' records are not touched. The window is 30 days, set in `web.toml`:

```toml
[workflows]
run_retention_days = 30
```

Zero is rejected.

## Tasks raised for job failures

The Web scheduler raises a task in the vault's task system when a vault job needs somebody's attention. One reconcile step runs on each pass over each vault, after validation and before anything is started, and reads what is durably there: the job state, the finished runs of an enabled job above its examined mark, and the team. Because it reads state and not events, a failure the command line recorded, or one that happened while no scheduler was running, is raised by the next pass of a scheduler that is, and a raise that failed is tried again on the next pass. The command line and the pages never raise.

**Events.** A task is raised for each of these, once:

| Event | Origin episode | Raised when |
| --- | --- | --- |
| `validation-failed` | `v-` and the first 12 hexadecimal characters of the version, or `unread-` and the UTC time when the job's files could not all be read | A `ready` draft whose automatic validation for its current version found failures (a check that could not be made is not a failure) |
| `suspended` | the UTC time of the suspension | The system suspended the job, for any code: a new version that failed validation, a failed first run, an owner who is not a member, a rules revision the job no longer passes, or a suspension an earlier release stored |
| `run-failed` | `R-` and the run's number | A finished live run of an `enabled` job above the host's examined mark is a scheduled run that failed or timed out after its script started (`script_started`), and it is new: see "Which failures are new" below |

Nothing is raised for a degraded, skipped, interrupted, or cancelled run, for a dry run, for a failure of a run started by hand (a manual first run that suspends the job is raised as the suspension), for a suspension the operator made, for a validation the operator asked for, or for a shared job. A scheduled run whose script never started (`script_started` is `false`) is no defect of the job and raises nothing, whatever its record says. A later failure while a `run-failed` task is open is folded into it: nothing is raised for it and the open task's run history is where it is found. Once that task is closed, the next failed scheduled run raises a new one, and a failure that was folded never comes back as a task of its own. Which states are closed comes from the vault's `.contextos/tasks.toml` (the shipped `done` and `dropped` without one), and a task in a state the configuration does not declare counts as open.

**Which failures are new.** This is decided from the task list, the run's number, and the host's examined mark, never from what a scheduler process remembers and never from any file's modification time, so a dry run, a skipped tick, or a restart cannot change it. The mark is `{job}.run-failed-examined` beside the job's claim on the host, next to `next-run-id` and `last-fire`, and holds the highest run number already examined for a `run-failed` task. Every finished live run above the mark is examined, lowest first, within the latest 50 run records one history read returns, and the record's own `script_started` is read in that same read. The first failed, started, scheduled run is new when no `run-failed` task for the job is open, its number is greater than the mark, and its number is greater than every `R-n` named in the origins of the job's existing `run-failed` tasks. The failures after it are folded into it, so a failure followed by a success between two passes still raises its task, and one raise covers every failure examined with it. The mark moves forward, never back, to the highest run examined once the runs have been decided: when a run is folded (a task is open, or its number is at or below the mark or a run a task names), when a run is not a failure that can raise a task (it succeeded, was manual, or never reached its script), and when a raise has succeeded or the task was found to exist already. A job on its first-run trial has none of its runs examined, and a failed first run that suspends the job moves the mark to that run only after the settlement that recorded the suspension (the run settles first, and the host moves the mark under the bookkeeping lock, off the async executor, only when the settlement returned a suspension; a settlement that was not recorded leaves the run unexamined, so its failure is looked at once the trial is over). The gap between a recorded suspension and the mark moving, or a host that stops inside it, is closed from durable state and not from the order of steps: a pass that finds a failed run the job's current suspension record names (`run = n`) raises no `run-failed` task for it, because the suspension's task covers it, and moves the mark past it. so a failure a suspension handles is never raised a second time as a `run-failed` task, after a resume or otherwise. A create that fails leaves the mark where it was, so the run is tried again on the next pass; so does a task list that cannot be read. A failure that ended while a task was open was folded into it, the mark records that, and it stays folded after the task is closed. A mark ahead of the run records (a record was lost) does not hide a later failure numbered above it. A mark that is not a number, or that cannot be read, is logged as one warning per vault and job (naming the file by its job folder and name) until a read finds it usable; it decides nothing, is left for the operator to repair or delete, and the job's run counter is forgotten on every pass while it is unusable, so the runs are looked at again as soon as it is repaired or removed. A record whose `script_started` is present but is neither `true` nor `false` is reported in the same way, once per vault and job, naming the record by its vault path: that run and every run after it are not examined, the mark stays below them, and no code-based fallback is used for it; the job's other runs are still listed everywhere else. Only a record that lacks the field altogether is judged by its code. The mark is host bookkeeping, not vault state: a second host running the same vault keeps its own mark, so a failure folded on one host can surface once as a task on the other.

**The task.** It is created with `base_record_create` on `memory/tasks.base` from the `A` series with `unique_fields: ["origin"]`, so the server, inside the vault's write lock, refuses a second task for the same origin, closed or open: the raise is idempotent across processes and restarts, and a refused duplicate counts as raised. Its frontmatter is `type: task`, `task_type: action`, `state: next`, `title`, `entity` (the job's resolved entity), `assignee`, and `origin`, and nothing else beyond what record creation stamps.

- `origin` is `contextos-web:<purpose>:<workflow>/<job>:<episode>`, with the purposes above.
- `assignee` is the job's owner when the owner is a member of the team, otherwise the chief of staff, and always the chief of staff for a suspension with `workflow/owner-unresolved`.
- The title is `Job <workflow>/<job> failed validation`, `Job <workflow>/<job> was suspended: <code>`, or `Scheduled run R-<n> of <workflow>/<job> failed: <code>`.
- The body has a `Context` (what happened, when it happened, the job's state, and each stable code). The time is the episode's own: the run's start for a failed run, the suspension's `at`, or the time of the validation that failed, and never when the scheduler noticed, an `Evidence` list (vault paths to the workflow, entry script, job file, `.contextos/jobs.toml`, the run record where there is one, and the run history folder), and `What to do` for the event. It holds codes and paths only: never a failure message, an error text from a record, a call's arguments or result, an input value, script text, or a host path. The messages stay in the record and the state file the evidence links to.
- A vault with no tasks base, or whose sidecar declares no `A` series, has no task system: the scheduler logs one warning per vault, raises nothing, goes on to start due jobs, and raises the task once the system exists. Any other refusal is logged once for its origin and code and tried again on the next pass.

The scheduler keeps in memory which origins it knows are raised and which run counter it last looked at for each job, so a quiet pass makes no call; none of it is stored, so a restart costs at most one refused call for each episode still present.

## Events recorded for job outcomes

The same reconcile step that raises tasks also records events for job outcomes, so a job that crashes before it can record anything is still shown red or amber on the Signals screen (see [`web-signals.md`](web-signals.md)). A job records the events appropriate to it, such as a data load it finished, and there is no event per run. The scheduler records these four, and nothing else:

| Kind | Severity | Recorded when | Fields |
| --- | --- | --- | --- |
| `job.failed` | error | A finished scheduled run of a vault job ended `failed`, whether or not its script started | `package`, `workflow`, `job`, `run` (`R-n`), `code`, and a `subject` linking the run record |
| `job.suspended` | warning | A vault job is suspended by the system and the suspension records when | `package`, `workflow`, `job`, `code`, `suspended_at`, and a `subject` linking the job file |
| `job.retired` | info | A vault job was retired by its successor's enabling, which the job's state records with the time | `package`, `workflow`, `job`, `replaced_by` (the successor's key), `retired_at`, and a `subject` linking the job file |
| `job.recovered` | info | The first scheduled run to `succeed` after a scheduled run that failed with no scheduled success between them (runs that ended cancelled, interrupted, skipped, or degraded in between neither recover the job nor forget the failure), or the first scheduled run to `succeed` after a recorded `job.suspended` that no `job.recovered` has followed and the job was resumed | `package`, `workflow`, `job`, `run`, and a `subject` linking the run record |

Each is recorded with `base_record_create` on `memory/events.base`, with the job's resolved entity, and with `unique_fields` of `kind`, `package`, `workflow`, `job`, and `run` (or `suspended_at` for a suspension, and `retired_at` for a retirement), since a job may record kinds of its own about the same run, so the server refuses a second event for the same episode, whichever process asks, and a refusal with `io/duplicate-record` counts as recorded. Any other failure is logged once for the job and the kind, leaves the event unrecorded, and the job's runs are read again on the next pass. A failure to read whether a suspension is still open is treated the same way: the decision is made again on the next pass and never settled as no recovery. The job's state, schedule, and tasks are never changed by recording.

Nothing is recorded for a shared job, a run started by hand, a dry run, or a run that was cancelled, interrupted, degraded, or skipped. Only runs that started within the 7 days before the vault was first reconciled for events are recorded, so a vault that has never had events does not fill with the history of every run its jobs ever made. The host keeps when it first reconciled each vault (a file named for the vault in the `.job-events-reconciled` directory of the host's job-claims directory, a place no workflow's name can take), so every later reconcile records each run since, however long ago, and none before. If that file cannot be read, or does not hold a time, the host logs it once, considers only the last 7 days of runs on each pass, and leaves the file alone for the operator to repair or remove, so the floor is never moved forward by overwriting it. A failed scheduled run of a job since retired or taken out of use is recorded like any other; only a job that has never left draft is not read. A failed run whose record names no code is recorded with `workflow/failed`.

A vault that has no `memory/events.base`, whose sidecar `memory/events.md` names no kind registry (`event_kinds`), or that does not declare one of these kinds in `registry/events/`, records nothing. The scheduler logs one warning naming the vault, looks at the vault again after five minutes, and records the missing events once the vault can take them.

## Dry runs

`--dry-run [--scenario <name>]` runs a workflow against a scenario and never reaches a real server for a call. A vault-registered workflow's scenarios are under its `tests/` directory. A package may ship `{workflow}/tests/{name}.toml` in the same format, covered by its content digest like every other file, and a shared workflow's dry run reads them from the release it resolved, whether it is a saved job (`jobs run --shared --dry-run`) or a one-off (`workflows run --shared --dry-run`). With no `--scenario`, the only scenario the package ships for the workflow is used. A workflow with no scenario is refused, naming it and the package, so its jobs cannot become `registered`; several without a name are ambiguous; and a name cannot leave the workflow's `tests/` folder.

### One-off runs

`workflows run` runs one workflow once: no job, no claim, no run record, and never on a schedule. The code it runs is a release, chosen with `--release`, or, for a vault workflow only, the source in `registry/`, chosen with `--source`. The two cannot be given together, and `--source` has no meaning for a shared workflow, which has no source on the host.

- `--release current` (the default), `future`, and `previous` take the version the state records under that name for the workflow, in the vault's `.contextos/jobs.toml` or the host's `shared-jobs.toml`. A name the state records nothing under is refused with `workflow/job-not-runnable`, as is a workflow with no job yet enabled when `current` is asked for.
- `--release <version>` takes an exact version, which must be the workflow's `current` or `previous` release or a `future` release (any release newer than `current`, or any release of a workflow with no `current`). A retired release, and a version the store's ledger has never recorded, are refused with `workflow/job-not-runnable`; a version the ledger records but the store no longer holds whole is `workflow/package-unavailable`. A one-off run of `previous` is allowed, so a return can be checked before it is made.
- A release is read and verified before its script starts, like a saved job's, and a release or a locked library release that is removed, damaged, or incomplete ends the run with `workflow/package-unavailable`. A release with a recorded library lock runs with it. A release without one resolves its libraries over the installed library releases for this run only and records no lock. Either way the summary lists the library releases in `libraries`, and the release's version and digest in `scope`.
- A dry run (`--dry-run`) of a one-off run replays a scenario: a vault workflow's from `tests/` in source, read live, and a shared workflow's from the release.
- `--source` switches the run to source, through the primary server, and installs nothing. The workflow's code is read from `registry/workflows/{package}/`. Each library the source names without a requirement is read from its own source in `registry/libraries/{library}/`, and each named with a requirement comes from the newest installed library release that satisfies it, the same rule applying to the libraries a library names. No lock is recorded. The summary lists each library in `libraries` as its release or as `source`, and `scope` carries no version or digest because the run read no release. A requirement no installed library release satisfies ends the run before its script starts with `workflow/package-unavailable`, naming the library.

A saved job's dry run uses its release and library lock as its live run does. Templates of a vault workflow are read live from source while the release fixes their declared paths, so a template renamed or removed after install surfaces only when a script reads it: the read fails naming the template. A dry run of a `draft` job fails, a dry run or a run by hand of a `validated` or `registered` job fails and leaves the stage unchanged, a scheduled run of an `enabled` job fails and raises the run-failed task, a run on trial suspends the job, and a `suspended` or `retired` job is not run. Finding the workflow's jobs does not read its templates, so a missing template never hides a job. `workflows releases` reports the missing templates of every kept release.

A dry run of a saved shared job takes no claim, so it never holds back a live run and is never held back by one, and it leaves a run record with `dry_run: true`, so the history shows it was synthetic. The record takes its number from the job's own counter and is written once, when the run ends, so a dry run that dies leaves nothing for a later run to mistake for a dead live run. A dry run neither marks another record interrupted nor sweeps old ones, which stays with the live runs. A one-off dry run takes no claim and changes no state. A dry run that completes registers a `validated` shared job for the version it ran, and never settles a trial. Its summary says so, for vault and shared workflows alike, so it is never mistaken for a live run: the headline begins `Dry run`, the `Scenario:` line says the call results were mocked, and `dry_run` is `true`. The summary's `registration` says whether a dry run of a saved job registered it, or why not.

## Probation

Everything below holds for vault jobs and shared jobs alike. For a shared job the review is made by the Web scheduler's pass over the host state, a new version is a changed job file (other than a changed `version` or `predecessor`, which suspends the job) or a changed release of the same name, a job whose package was removed is suspended, and a run settles against the version it resolved before it started.

A suspension made here is stored with a code: a new version that fails validation is `workflow/validation-failed`, and a first live run that fails is `workflow/probation-failed`, naming the run's number (when its record could be started) and the failure's code. Both raise a task (see "Tasks raised for job failures"), and a shared job's raise none.

A job that runs unattended should stop when an upgrade breaks it, without waiting on a person for every harmless change. An `enabled` job whose version changes keeps its schedule, and its first live run of the new version is on trial.

**A new version.** The validation pass looks at each `enabled` job's version on every poll, and validates the new version at once. When it passes, the job stays `enabled` and its first-run flag is set. The dry run is not repeated. When it fails, or when the job's files can no longer be read, the job is suspended with every failure, each with its code and the file it concerns, as the reason, and the result is stored as for any validation. A check that could not be made, such as a server that cannot be reached, leaves the job as it is, and the version is looked at again on a later pass, because an outage is not a defect in the job. A file that cannot be read once is not a new version: the version that validation reads is compared with the one on record, and only a version that differs, or files that cannot be read again, change anything. A `suspended` job whose files change stays suspended and is not validated, and a job that is not `enabled` is never put on probation.

**A file edited away from the job's record.** An `enabled` job changes release or predecessor only through `jobs successor`, so a job file whose `version` or `predecessor` differs from what the job's state records is not a new version of the same job. It is validated at once and suspended with `workflow/validation-failed`, naming `validation/artefact-invalid` on `version` or `predecessor` among the failures, and it is never put on probation with the new release or lock. Any other edit to an `enabled` job, such as its parameters, is still a new version on trial. A `suspended` job is held to its record in the same way: validating it, which a resume does first, fails on `version` or `predecessor`, the job stays `suspended`, and resuming it is refused until the file is restored, whatever release the file names. A `retired` job's file is checked when it is re-activated: one whose `version` or `predecessor` differs from the record refuses the re-activation with `workflow/transition-refused`, naming the key, the recorded value, and the value the file names, and the job stays `retired` until the file is restored. An `enabled` job whose file names a release that is not installed is suspended with `workflow/validation-failed`, because the edit is the fault; `workflow/package-unavailable` is for a job whose unedited release has since gone.

**A due job is not started on a version nobody has reviewed.** When a job is due and its current version is not the one on record, because a pass was dropped or has not yet run, the scheduler does not start it that pass. Its tick stays due, and it runs once a pass has reviewed the version, so a failed run can always suspend it. A run started by hand is the operator's own act and is not held back, so a failure of an unreviewed version leaves the job as it is until the next pass puts the version on probation.

**The first-run flag.** Enabling a job sets the flag, because its first live run is the first of its version. The flag is stored as `first_run` beside the job's state, only while it is set, and only an `enabled` or `suspended` job has it. A job that leaves those states, by having its schedule cleared, being retired, or going back to `draft`, loses it, and enabling it again sets it again.

**How the first live run ends.** Every live run, whether the scheduler started it, someone ran it by hand from the job page, or `jobs run` ran it, is settled against the version it ran.

| The run | What happens |
| --- | --- |
| `succeeded` or `degraded` | The flag is cleared, and the job carries on |
| `failed`, including `workflow/timed-out`, once its script started | The job is suspended with the failure as the reason, the flag stays set, and the run is not retried |
| `refused`, the script never started | Nothing changes |
| `cancelled` | Nothing changes |
| `interrupted` | Nothing changes, because a run that never reported an outcome is only marked after a restart |

An outcome is applied only to the version that is on trial. A run of any other version, such as one that began before a newer version passed validation, changes nothing, and neither does a run of a job whose flag is not set. A manual run of a `suspended` job whose flag is set that succeeds clears the flag and leaves the job suspended, because resuming is the operator's decision. One that fails changes nothing, so the original reason stands. A manual run of a `registered` job has no flag to settle and changes nothing, so enabling that job still puts its first scheduled run on trial. A dry run never settles anything, whatever it finds, so a synthetic result is never taken as acceptance.

A resumed job keeps its flag, so a job suspended by a failed first run is still on trial when it is resumed. If the state file keeps changing under the settlement, or cannot be written, the outcome is not applied and a warning is logged naming the job, because a failed first run that does not suspend its job is a safety gap. The files of a job are read before the run starts, so a file that changes between that read and the script being read may leave the outcome attributed to the earlier version.

## Failure codes

| Code | Meaning |
| --- | --- |
| `workflow/effect-denied` | The call is outside the workflow's declared servers or permitted effects |
| `workflow/vault-out-of-scope` | The call names a configured vault the run is not scoped to |
| `workflow/vault-unavailable` | A vault the run names is not, or is no longer, configured |
| `workflow/server-unavailable` | The server is not configured or cannot be reached, or the vault list could not be read to scope the run, or the job's files could not be read to check whether a scheduled run's code was reviewed |
| `workflow/tool-error` | The tool ran and returned an error |
| `workflow/result-unreadable` | The tool's result could not be read as JSON |
| `workflow/package-unavailable` | The package, workflow, or the release a job names is not installed |
| `workflow/input-invalid` | An input is undeclared, missing, or invalid |
| `workflow/job-busy` | Another run of the same job, shared or in a vault, holds its claim. Also given to enabling a successor while a run holds its predecessor's claim, since enabling retires the predecessor |
| `workflow/timed-out` | The run exceeded its timeout and was stopped |
| `workflow/job-not-runnable` | The job's lifecycle state does not allow this run, and the refusal names the state; or a live run by hand of a job in a chain whose other job holds it, and the refusal names that job; or the code of a scheduled run of an `enabled` job, including a library it reaches, changed since it was reviewed |
| `workflow/owner-unresolved` | A vault job's owner is missing or is not a member of the team. As a refusal, a live run was refused before its claim. As a suspension code, the system suspended the enabled job |
| `workflow/team-invalid` | The vault's `.contextos/team.toml` is absent, is not text, does not parse, or breaks a rule a job depends on (the rule is named). A live run by hand is refused, and the vault's scheduled jobs are paused |
| `workflow/operator-suspended` | In a job's suspension record only: the operator suspended the job |
| `workflow/probation-failed` | In a job's suspension record only: the first live run of a version failed, naming the run and the code it failed with |
| `workflow/suspended` | In a job's suspension record only: a suspension an earlier release stored as free text, whose cause was not recorded |
| `workflow/transition-refused` | A lifecycle change by an actor that does not own it, or from a state it does not apply to |
| `workflow/state-changed` | The job state changed while a change or a run claim was being made, so nothing was written; repeat the request |
| `workflow/state-unavailable` | The job state file could not be read or written, or is malformed (for a scheduled run, also when it cannot be read, including over a dropped connection, or parsed to check whether the job's code was reviewed); the host's app-data folder, a claim, or a run-record location could not be resolved or used for a run command; or, as a host warning in a run summary, a dry run's registration or a first run's settlement could not be saved |
| `workflow/validation-failed` | A validation an operator asked for, or the one a resume makes, did not pass: a failure was found, a check could not be made, or the files could not all be read. The message lists each |
| `workflow/job-not-found` | A command that needs a job's files found none by that name |
| `workflow/job-file-invalid` | The TOML given to `jobs create` or `jobs replace` does not load as a job file, or the file `jobs show` or `jobs replace` found is too large, not UTF-8, or unreadable. The message and the JSON `cause` carry the validation code, `validation/artefact-invalid` |
| `workflow/job-key-invalid` | The package, workflow, or job given to an authoring command is not a slug, or is a Windows reserved device name |
| `workflow/job-key-mismatch` | The key the TOML would be tracked under (its `slug`, or the file's stem) is not the `<job>` the command named |
| `workflow/job-exists` | `jobs create` found a file at the job's path with different content, and left it untouched |
| `workflow/config-invalid` | The `web.toml` a command was started with is there and cannot be used, or its `log_file` cannot be opened, so the command did nothing. A run command (`jobs run`, `jobs dry-run`, `workflows run`) also refuses with it when `web.toml` is missing, because it needs the servers the file names, or when no path for it can be determined. A command that needs no server treats a missing `web.toml` as no configuration and carries on with the default logging. The message names the class of failure and never a path |
| `workflow/job-key-taken` | Another job file is tracked under the key, so a create is refused, or two files share the key (or a usable file is tracked under it and an unusable one is named for it), so a show, replace, or delete cannot say which is meant. The message names the files |
| `workflow/job-state-remains` | `jobs create` or `jobs successor` found stored state for the key and no file. Delete that state first |
| `vault/not-found` | In the API only: the vault named in the path is not one the primary server has |
| `workflow/request-invalid` | In the API only: the body of a request for a successor is not JSON, names an unknown field, lacks a field it needs, or names a release that is not `future`, `previous`, or an exact version |
| `workflow/job-changed` | `jobs replace` found a file whose hash is not the one given, and gives the current hash. Read it again with `jobs show` and reapply the change |
| `workflow/job-store-unavailable` | The jobs folder could not be read or written, a link or reparse point stands where a folder or the job file belongs, or a delete could not remove the job's file (or found one that cannot be used) and wrote no state |
| `workflow/job-unreadable` | A schedule that would enable a job needs a version of it, and its files could not all be read |
| `workflow/file-path-invalid` | A path given to `workflow_file` or `workflow_files` is not a plain relative path inside the workflow |
| `workflow/file-undeclared` | The path is not, and is not beneath, anything the workflow declares in `templates` |
| `workflow/file-not-found` | A declared path holds no file, or names a folder where a file was asked for |
| `workflow/file-not-text` | A declared file is not UTF-8 text |
| `workflow/file-too-large` | A file is over 1 MiB, or a listing is over 1000 files, or in a vault over 32 folders deep |
| `workflow/file-unreadable` | A declared file could not be read, is a link in an installed release, or has a name no workflow path can address |
| `workflow/script-error` | The script failed on its own: it threw an error, or hit a runtime or syntax error. The message begins with the code and carries Rhai's own text |
| `workflow/entry-unreadable` | The workflow's entry script, one of its declared modules, or a module or manifest of a vault library it reaches could not be read or is not text, so the run ended before its script started; the message names the file |
| `workflow/output-invalid` | A script set an undeclared output, one of the wrong kind or over 64 KiB, or finished without a required output |
| `workflow/internal-error` | A task of the host that was running the workflow panicked or was lost, which is no fault of the workflow |
| `workflow/cancelled` | In a run summary only, not a failure kind: the run was cancelled |
| `workflow/workflow-not-found` | The workflow is not registered in the vault the command named |
| `workflow/scenario-unavailable` | A dry run has no scenario, the named one is missing, or the scenarios cannot be listed or read |
| `workflow/scenario-ambiguous` | A dry run found several scenarios and none was named |
| `workflow/scenario-invalid` | The scenario does not parse |
| `workflow/record-unavailable` | A vault live run's record could not be created, so the run was refused. In `host_warnings` it means a record could not be written or finished after the run went ahead |
