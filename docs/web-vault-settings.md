# Web vault settings

`/{vault}/settings/` in `contextos-web` has two groups in its sidebar.

- **Configuration**: the panes that edit this vault's `.contextos/*.toml` display and behaviour settings (Tasks, Inbox & Outbox, Journal, Notes, Wiki, Insights, Entity folders, Profile). The Journal pane's `.contextos/journal.toml` is described in [`web-journal.md`](web-journal.md). They all render in one page and switch without a request; a link to `/{vault}/settings/#{pane}` opens that pane.
- **Vault management**: Decisions (decisions and questions in one register), Registry, Standards, Templates, and the Activity log. Each is its own resource at `/{vault}/settings/{category}/`, with every file or record beneath it addressable, so a page can be bookmarked and reached with back and forward.

Every write goes through the vault's MCP tools and is conflict-checked: an edit carries the `expected_hash` of the version it was made from and is refused (`io/conflict`) if the file changed since, and a new file is written without a hash, which the server refuses (`io/expected-hash-required`) when anything already exists at that path. A refused write keeps what was typed on screen and says why.

## Which categories appear

| Category | Shown when | Empty view |
| --- | --- | --- |
| Decisions | Always: its location is configurable from its own page | "No decisions registered yet" |
| Registry | Any one of its six type folders exists | Each type's tab shows "Nothing defined yet" |
| Standards | `standards/` exists | "No standards yet" |
| Templates | `templates/` exists | "No templates yet" |
| Activity log | `memory/log/` or `memory/sessions/` exists | "No operations logged yet" or "No session summaries yet" |

A category that is not shown answers `404`. The sidebar count beside each category leaves out folder `index.md` files.

## `.contextos/vault-management.toml`

Where the Decisions register and the registry types live. Every key is optional; an absent key keeps the scaffold value shown below. A file that cannot be parsed, names an unknown key, or carries an unsafe path is not used: the pages say why and show the defaults. Each page's Edit action updates its own table, keeping every comment and unrelated key in the file, refuses a folder, base, or template that does not exist in the vault, and is refused if the file changed since the page was opened.

```toml
[decisions]
base = "memory/decisions.base"                       # must be a .base file
dir = "memory/decisions"                             # decisions and questions, one tree

[decisions.decision]
prefix = "VD"                                        # the id series, and what tells an untyped record's type
template = "templates/registry-vault-decision.md"    # a Markdown file, or "" for none

[decisions.question]
prefix = "VQ"                                        # must differ from the decision prefix
template = "templates/registry-vault-question.md"

[registry.agents]    # likewise hats, prompts, roles, scripts, skills
dir = "registry/agents"
template = "templates/registry-agent.md"             # "" for none
```

Paths are vault-relative, `/`-separated, with no `..`, `.`, or empty segment, no backslash or colon, and no hidden (dot-prefixed) folder. A managed folder may not be `memory/log` or `memory/sessions`, or lie inside them: those are the append-only records the Activity log shows read only.

## The Decisions register

Decisions and questions are one Markdown note per record in one folder. A record's type is its `type` frontmatter (`decision` or `question`); a record without one is told by its id prefix (`VD-` or `VQ-` by default), and a record matching neither is shown as unclassified, with only the fields every record shares. The list shows every record newest id first with its type, status, last update, and onward link (the decision that superseded it, or the decision a question led to); `?type=` and `?status=` narrow it. A record reads its fields beside its rendered body; Edit saves fields and body together.

| Type | Fields | Status values |
| --- | --- | --- |
| Decision | `title`, `status`, `date`, `superseded_by` (a decision id), `applies_to` (one per line), `description` | `adopted`, `superseded` |
| Question | `title`, `status`, `entity`, `resolved`, `decision` (a decision id), `blocks`, `resolution_path`, `description` | `open`, `resolved` |

`id`, `type`, `created`, and `updated` are shown but never edited: the id and type are fixed at creation and the server stamps the timestamps. A record already holding a value outside its vocabulary, or a full timestamp in a date field, keeps it. A save is one `note_update` call carrying only the fields that changed and, when it changed, the body, checked against the version the page showed: it is applied whole or refused whole, `created` is kept, `updated` is stamped, and a save that changes nothing writes nothing. The server holds it to the note rules (no empty title, no description repeating the title, well-formed Obsidian links and callouts) and stores it, like all text, with `\n` line endings.

New offers a decision or a question. The record is created with `base_record_create` against the configured `.base`, so the sidecar assigns the next id from that type's prefix, and the record is written with its `type`. The sidecar must declare the prefix (in `key_prefixes`); if it does not, the form says so rather than minting an id from another series. The body starts from the type's template with its frontmatter and heading removed, and the template's trailing authoring comment is shown beside the form instead.

## Libraries: Standards, Templates, and the Registry

A library is a folder tree beside a reader. Markdown files read rendered with their frontmatter as a property list; any other text file reads as highlighted source (Python, shell, PowerShell, JavaScript or TypeScript, Rust, TOML, YAML and `.base`, JSON and `.canvas`). Templates show Templater commands and `{{placeholders}}` as marked code rather than interpreting them.

Edit replaces the whole file, frontmatter included, and saves it back byte for byte, except that the vault stores all text with `\n` line endings: a file with `\r\n` or lone `\r` ones says so in the editor and is saved with `\n`. A file over 1 MB opens read only with the reason shown. New creates a file in the folder being browsed:

| Library or type | A new item is |
| --- | --- |
| Standards, Templates, Agents, Hats, Prompts, Roles | `{name}.md` |
| Scripts | `{name}` including its extension |
| Skills | a `{name}/` folder holding `SKILL.md` |

A registry type with a configured template starts its new definitions from that template's text. `registry/apps/` and `registry/workflows/` are managed by the Apps and Workflows screens and do not appear here.

## Activity log

Read only: the operation log (`memory/log/YYYY/MM/YYYY-MM-DD.md`) and session summaries (`memory/sessions/`) are append-only records, and no edit, new, or save action is offered. A day's log shows each `HH:MM[:SS] | origin | operation | description | files: ...` line as a table row, with `\|` read as a literal bar; a line in any other shape is shown as written. Navigation runs newest first by year and month, with the previous and next logged day either side.

## Tasks configuration: `.contextos/tasks.toml`

The Tasks pane edits four parts: the task types, the shared stages and each type's own, the hierarchy depth labels, and the priorities. Add, Remove, and the Move buttons change the form without writing, as do the buttons that give a type its own stages (starting from a copy of the shared ones) or return it to the shared ones; Save validates everything and writes the file, keeping its comments and any other keys.

```toml
[[stage]]                 # the shared stages, in lifecycle order
state = "waiting"
label = "Waiting"
icon = "clock"            # any icon the stylesheet renders
bucket = "needs_call"     # now, needs_call, backlog, done, or omitted
colour = "amber"          # grey, faint, blue, green, amber, red, purple, teal, or omitted
closes = "never"          # never, completed, cancelled, or omitted

[hierarchy]
depth_labels = ["Initiative", "Action"]

[[task_type]]
value = "decision"
label = "Decision"
prefix = "D"              # the id series, when the tasks sidecar declares it
initial = "proposed"      # one of the type's own open states; its first open state when omitted

[[task_type.stage]]       # the type's own stages; a type with none uses the shared stages
state = "proposed"
bucket = "needs_call"

[[task_type.stage]]
state = "adopted"
bucket = "done"
closes = "completed"

[[priority]]              # most urgent first
value = "highest"
label = "Highest"
```

Each task type moves through its own stages, or the shared stages when it declares none of its own. A task's State choices, its colour and label, which Focus section it appears in, and whether it counts as open all come from its own type's stages, so two types can name the same state (each type's `dropped`) and still be counted, filtered, and coloured apart. The stage strip is there to prompt action, so it shows one group per type with a tile for each of the type's open stages only (as shipped: an action's backlog, next, doing, waiting, and review; a decision's proposed; and a question's open), never a closing stage. A tile links to its stage as `type:state`; a link naming a bare state matches that state in every type. The strip ends with one tile for momentum rather than action, Done under Today: the tasks of every type closed as completed today (in a stage whose `closes` is `completed`, with a `completed` date of the operator's today, in the zone the contextos server reports). It links to All tasks with `?completed=today`, which lists only those tasks in every view, done ones included, and never changes the strip's counts; `today` is the only value accepted. A task dropped or reopened since it was completed is not counted.

A stage's `closes` says whether entering it closes a task and which date that stamps: `completed`, `cancelled`, or `never`. A stage without one closes as its state's shipped answer: `done`, `adopted`, and `answered` complete a task, `dropped` cancels it, and any other state leaves it open. A task is open when its type declares its state and that state does not close it; a task in a state its type does not declare is shown with that state as written and is not counted as open. Every type needs at least one open state. A task whose type is not declared, or which has none, follows the shared stages: it is counted, coloured, and placed in Focus by them, and a link names its stages as `:state`. The stage strip has no tiles for such tasks.

A stage's `colour` is the colour its state chip is shown in on the task table, the Focus cards, and the stage editor, chosen from a fixed palette that follows the light and dark themes. A stage without one takes the shipped colour for its state: backlog, done, adopted, and answered grey, next blue, doing green, waiting and open amber, proposed teal, review purple, and dropped faint grey; any other state is grey.

An unknown key anywhere in the file makes it unreadable (the pane says which), so a save can never drop it. Absent `[[stage]]`, `[[task_type]]`, or `[[priority]]` lists use the shipped vocabulary. The shared stages are the action lifecycle: backlog, next, doing, waiting, review, done, and dropped. The task types are action (`A`, on the shared stages, starting in `next`), decision (`D`: proposed, adopted, and dropped, starting `proposed`), and question (`Q`: open, answered, and dropped, starting `open`). The priorities are highest, high, medium, normal, and low. The task detail, create, and table screens offer these values, and sorting by priority follows the declared order, with an undeclared value after every declared one.

A new task whose state is left to its type starts in the type's `initial` state, or its first open state. A state chosen on the create form or the detail page must be one the task's type declares, and a save that changes a task's type or state to a pair the type does not declare is refused, naming the type's states; a task already holding such a state can still have its other fields edited. The detail page offers one close button per closing state of the task's type, and none once it is closed. Closing a task stamps its close date unless the save gives one, including from the edit form, whose date fields arrive blank when unset, and a task created straight into a closing state is stamped too. A task already closed under a state its type no longer declares (a decision left `done`) keeps its close date when moved to one of its type's closing states, and is not recorded as closed again. While `tasks.toml` exists but cannot be read, a change of a task's type or state, and creating a task, are refused with the reason, since its states cannot be checked; other edits still save. Nothing schedules a task's review on its own: a review date is the `scheduled` field, set by whoever raises or edits the task.

A task note is filed in a folder beneath `memory/tasks/`. The create form's Folder field chooses it, suggesting every folder that already holds a task and its ancestors, and an empty folder files the task at the top level. The detail page shows the folder relative to `memory/tasks/`, as the Folder field takes it (Top level for a task directly in it), and its edit view moves the note to another folder with the Move button, keeping its file name and content. A folder is refused, and nothing is written, when it is absolute, uses `\` or `:`, contains an empty, `.`, or `..` folder name, contains a folder name starting with `.` (which Obsidian hides), or contains a control character or any of `#`, `|`, `^`, `[`, `]`, `<`, `>`, `"`, `?`, and `*`, which would break a link naming the task by its path or which Windows refuses. A move onto a note already filed under the same name is refused rather than replacing it, and when the tasks base's filters select only the notes directly in its folder (`file.folder ==`), a sub-folder is refused on create and on move, since the task would leave the base. Any other refusal names the server's reason. A task in a folder this form cannot name exactly (one with surrounding spaces, written outside ContextOS) shows its folder, still relative to `memory/tasks/`, without the move form. On the create form, `folder` always chooses where the note is filed and is never saved as a frontmatter field; a vault-specific `folder` field can still be set from the edit view. A task is referenced by its key, never its path: link fields store `[[a-001|A-001]]`, and a task listed in the Journal ([`web-journal.md`](web-journal.md)) is linked the same way (by its file name, labelled with its key, when the file is not named for its key), so moving a note leaves them resolving. Moving a note does not rewrite a link someone wrote by its full path.
