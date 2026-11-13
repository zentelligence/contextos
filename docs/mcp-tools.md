# MCP tools

Every tool operates on a vault-relative path (`name://relative-path`, or a bare relative path when only one vault is configured) resolved through the server's path-safety layer. An unmanaged vault is filesystem-only: the tools still read and write its files, but the managed `index.md`, operation log, Git recovery, and search services are off for it, so a tool that depends on one of them reports it as disabled.

## Line endings

The vault stores text with `\n` line endings, as it gives every path with `/`. Every text write converts any `\r\n`, or lone `\r` (the classic Mac line ending), it is given or finds in a file it rewrites to `\n`, and returns a `write/line-endings-converted` warning when it converted anything; the returned `content_hash` is that of the converted text. `fs_edit_file` matches `old_text` against the converted text, so text quoted with `\n` finds its place in a file that still has `\r\n` or `\r` endings, and its diff (including a dry run's) is against the converted text. Binary writes (`content_base64`) are stored exactly as given, and reads return files as they are on disk.

## Filesystem

| Tool | Purpose |
| --- | --- |
| `fs_read_text_file` | Read a UTF-8 file with an optional head, tail, or inclusive line range, or a byte part given by `offset` and `length`. See [Reading a file in parts](#reading-a-file-in-parts) |
| `fs_read_multiple_files` | Read several UTF-8 files with isolated per-file failures |
| `fs_write_file` | Atomically create or replace a UTF-8 file with conflict protection |
| `fs_edit_file` | Apply exact-match edits transactionally, with an optional dry-run unified diff |
| `fs_attach_file` | Embed a text or base64 binary file as a size-capped MCP resource, followed by a text block giving its `content_hash`. With `expand_links` (Markdown notes and canvases only), the note, or the canvas with its text nodes' links and file nodes, with every link rewritten in place to a vault-rooted path, and a text block reporting `links_expanded` with no `content_hash` |
| `fs_create_directory` | Create a directory tree idempotently |
| `fs_list_directory` | List direct children with file and directory markers |
| `fs_directory_tree` | Return a bounded recursive JSON directory tree with exclusions |
| `fs_move_file` | Move or rename a file or directory without replacing the destination |
| `fs_delete_file` | Move a file or empty directory to trash, or hard-delete when configured |
| `fs_search_files` | Find paths by case-insensitive glob with exclusions |
| `fs_get_file_info` | Return path metadata, permissions, timestamps, and a bounded content hash |
| `fs_list_allowed_directories` | List configured, resolved vault roots and their managed flags |

### Reading a file in parts

`fs_read_text_file` and `fs_read_multiple_files` return a bounded preview, not the whole file, when the content reaches the inline threshold (`resource_link_threshold_kb`, 5 kB by default): `truncated` is `true` and a resource link to the rest is attached. `content_hash` always describes the whole file. `fs_attach_file` returns the whole file, up to 10 MiB, in one call.

To read a file the preview cannot cover, or one a whole read refuses as too large (above `max_read_mb`), `fs_read_text_file` reads a byte part:

- `offset` (default 0) and `length` (default and maximum: the inline threshold, minimum 4; the vault's `max_read_mb` also caps it) select the part. A part is read by seeking, so its cost is the part and never the file, and a single line of any length can be read. It ends on the last whole UTF-8 character within `length`, so it can be shorter. `offset` and `length` cannot be combined with `head`, `tail`, or `range` (`io/invalid-argument`).
- A part reports `content`, `size`, `version`, `offset`, `length`, `next_offset`, and `truncated`. `next_offset` is where the next part starts and is `-1` when the part reaches the end of the file. `truncated` is `false` only for a part that is the whole file. A part carries no `content_hash` and no `line_count`, since either would read the whole file, so take the hash for a later write from a whole read or `fs_attach_file`. A part never has a resource link.
- A part validates only its own bytes: a NUL byte or invalid UTF-8 inside it is `io/binary`, while binary bytes elsewhere in the file are not seen.
- A part that ends inside a character before the end of the file ends before that character (it drops at most three bytes). A part that holds a NUL byte or invalid UTF-8, a file that ends inside a character, or a part that could not advance is `io/binary`, so a read in parts always advances or fails and never loops.
- An `offset` beyond the end of the file, or inside a character, and a `length` below 4 are `io/invalid-range`. Start at 0 or at a returned `next_offset`.

Every read returns `size` (the file's size in bytes) and an opaque `version`: `fs_read_text_file` for every selection, each `fs_read_multiple_files` item that was read, and the text block of `fs_attach_file` (not with `expand_links`). Pass the `version` back as `expected_version` on every later read of the file. It is checked before anything about the file or the part's offset and length, and a file that has changed is refused with `io/conflict`, carrying `current_version` beside `code`, so the next read can start again from the beginning. A malformed `expected_version` is `io/invalid-version`. The `offset` or `length` beside `head`, `tail`, or `range` refusal is an argument error, checked before anything about the file.

The `version` is a cheap guard, not proof of identical content. On Unix it covers the file's size, modification time, device, inode, and change time (which the kernel advances on every change and a caller cannot set), so a same-size rewrite that restores the modification time is still noticed, unless the change time also cannot change. On Windows it covers size, modification time, and creation time only, so a same-size change that keeps the modification time can go unnoticed. A whole read carries `content_hash` for exact comparison. A part's own bytes and version come from one open file, which is checked again after the part is read.

To read a file in parts: read part 0, keep its `version`, pass it as `expected_version` with each `next_offset` as the next `offset`, and stop when `next_offset` is `-1`. On `io/conflict`, start again.

## Obsidian notes, Bases, and Canvas

| Tool | Purpose |
| --- | --- |
| `note_create` | Create a validated Obsidian Markdown note with standard frontmatter defaults (`entity` from its top-level folder only), its body given as `content` or copied verbatim from a vault file with `content_from` |
| `note_update` | Update an existing note's frontmatter and body in one atomic, conflict-checked write, keeping `created` and stamping `updated` |
| `frontmatter_read` | Read ordered YAML frontmatter from a note; with `expand_links`, every link value is resolved to its vault-rooted path (nearest first, as `fs_attach_file` expands links) and no `content_hash` is returned, so the values cannot be written back |
| `frontmatter_update` | Apply an atomic JSON merge patch to note frontmatter while preserving the body |
| `frontmatter_link` | Add or remove wikilinks in the link fields a base declares, validating each target and writing the reciprocal on the linked note |
| `links_read` | Read the links of a note or file. `direction` is `out` (the default), `in`, or `both`. `out` and `both` return `outgoing`, the note's wikilinks, embeds, and Markdown links, and `unresolved`; `in` and `both` return `incoming`, one entry (`source`, `line`, `form`) for each link to the file, sorted by `source` then `line`, read from the link graph after it is brought up to date with outside edits (a link from a `[vault.links] exclude` folder still counts, as it does for `backlinks`). `in` and `both` fail with `index/disabled` on a vault whose graph is off. Each outgoing link carries its `form` (`wikilink`, `wikilink-embed`, `markdown`, or `markdown-embed`) and, when it resolves, `resolved`, the vault file it reaches. Links resolve by the one Obsidian rule, against every file in the vault, so a link to an attachment, canvas, base, or note in an excluded folder resolves. `unresolved` holds objects (`target`, `form`, `kind` of `not-found` or `ambiguous`, and `candidates` for an ambiguous link), one for each distinct target written in each form; a template placeholder is in neither list. |
| `base_create` | Create a validated Obsidian Bases YAML definition and its paired key sidecar, declaring one or more id series |
| `base_read` | Read an ordered Bases definition with schema diagnostics, and `records_folder`: the folder its top-level filters confine its records to, when they name one, with `records_include_subfolders` (`true` for `file.inFolder`, `false` for `file.folder ==`, whose records sit directly in the folder) |
| `base_apply` | Apply ordered Base operations atomically after validating the complete result |
| `base_record_create` | Create a keyed record note within a Base's scope, assigning its id from one of the sidecar's declared id series, stamping a `base` link to the base and the fixed fields of the series' declared template |
| `base_query` | Execute a Base view's filter tree against the vault and return matching rows, or look records up by id within a base's scope |
| `canvas_create` | Create a validated JSON Canvas 1.0 document and generate omitted identifiers |
| `canvas_read` | Read Canvas nodes and edges with schema diagnostics |
| `canvas_apply` | Apply ordered Canvas operations atomically after full validation |

### Copying a file into a note

`note_create` takes the new note's body either as `content` or as `content_from`, a vault file whose body the server copies in verbatim. The content never passes through the caller, so a source filed into `memory/raw/` costs no tokens and cannot be altered on the way.

- `content_from` is `{ path, expected_hash? }`. Give it or `content`, not both; both or neither is refused with `io/invalid-argument`.
- A Markdown source (`.md`, in any case) has its frontmatter left behind and its body copied. The note's properties come only from `frontmatter` and the defaults, so a caller that wants to keep a source property reads it with `frontmatter_read` and passes it on. Malformed source frontmatter is refused with `format/frontmatter`, never skipped.
- Any other text file is copied whole, even when its first line is `---`: frontmatter is a Markdown notion.
- The source is read whole, within its vault's `max_read_mb`. A larger source is refused with `io/too-large` rather than cut short, a binary one with `io/binary`, a missing one with `path/not-found`, and one outside every vault root (including through a symlink) with a `path/*` code.
- `expected_hash` is the source's `content_hash` as last read. A source that has changed since is refused with `io/conflict`, whose `path` names the source, and nothing is written. A malformed hash is `io/invalid-hash`.
- The source is read under the new note's write lock, so within one vault no other write can interleave with the copy. A source in another vault is protected by `expected_hash` instead.
- `references` still apply, appended after the copied body. The body is validated as Obsidian Markdown (`format/markdown`) and stored with `\n` line endings, like every text write.
- The result is `path`, `content_hash` (of the note as written), and `warnings`.

### Updating a note

`note_update` changes an existing `.md` note's frontmatter, its body, or both, in one write. Use it rather than `frontmatter_update` followed by `fs_write_file` whenever both change: two writes can leave the fields saved and the body refused.

- `expected_hash` is required: the `content_hash` from `frontmatter_read`, `fs_attach_file`, or the previous write. A note changed since is refused with `io/conflict` and nothing is written.
- `patch` is an RFC 7386 merge patch for the frontmatter: a value sets a field, `null` removes it, and other fields keep their order. `body` replaces everything after the frontmatter's closing `---` line. Give either or both; neither is refused with `io/invalid-argument`.
- `created` is kept and `updated` is stamped on every update, so a patch naming either is refused with `format/note-managed-field`.
- A patch that sets `title` to an empty value, or leaves `description`, `summary`, or `excerpt` equal to the title, is refused with `format/note`, as `note_create` would refuse it. An existing note that already repeats its title is not refused for it unless the patch touches those fields.
- The body is validated as Obsidian Markdown (`format/markdown`). Like every text write it is stored with `\n` line endings (see [Line endings](#line-endings)).
- The frontmatter block is written back from its parsed form even when only the body changes: YAML comments are lost and quoting and flow style are normalised, as with `frontmatter_update`. For a hand-annotated note, change the body with `fs_edit_file` instead.
- A note that starts with a UTF-8 byte order mark keeps it, and its frontmatter is recognised after it (by every frontmatter tool).
- A stale `expected_hash` is reported as `io/conflict` before the patch or body is judged.
- The result is `path`, `content_hash` (of the note as written), and `warnings`.

### Keyed bases and id series

A `.base` has a paired `<name>.md` sidecar, created by `base_create`, that assigns record ids. It can declare several independent id series, each with its own counter: for example `A` for actions and `D` for decisions in one tasks base.

```yaml
key_prefix: A                       # optional: the default series
key_format: '{key_prefix}-{key_id}'
key_prefixes:                       # every series and its last assigned number
  A: 159
  D: 3
  Q: 0
last_create: 2026-09-28T09:00:00Z
last_update: 2026-09-28T09:00:00Z
```

- `base_create` takes `key_prefix` (the default series) and `key_prefixes` (further series). Supply neither for a base with no key scheme, whose sidecar is `key_format: FILEPATH`. An invalid set is refused with `format/base-sidecar` before anything is written.
- `base_record_create` stamps the record with `base`, a wikilink to the base file (for example `base: "[[memory/tasks.base]]"`), so the note names its governing base. A `base` you supply is kept when it links to that same base, and is refused with `io/invalid-argument` when it names any other base or is not a string. `base` is not a valid `link_fields` name.
- `base_record_create` takes an optional `prefix` naming the series to assign from. Omit it to use the default series. It is refused with `io/invalid-key-prefix` when the sidecar has not declared the prefix (matching is case-sensitive), when `prefix` is omitted and the sidecar declares no default series, and when a `prefix` is given for a `FILEPATH`-keyed base. Nothing is written when a call is refused. The assigned key is returned as `key`; a call that assigns none (a `FILEPATH`-keyed base) leaves `key` out of the result rather than returning `null`.
- Ids are unique within a base's identity scope, wherever this tool checks or counts them (an id already held, the highest id of a series, `unique_fields`), and `doctor` checks duplicates in the same scope: the folder the base's top-level `filters` name (`file.inFolder("...")`, or `file.folder == "..."` for only the notes directly in it), leaving out each folder's `index` note, whatever else the filters say, so a condition such as `status != "closed"` hides a record from views without freeing its id. Filters that name no folder define the base by its notes' properties, so the notes they select are its records; such filters that need the link graph (`file.backlinks`) leave the whole vault. Give every base `&& file.basename != "index"` so its views agree. With `folder` omitted or relative, the record is placed in the folder the filters name; a base whose filters name its folder with `file.folder ==` refuses a relative `folder` (`io/invalid-argument`), since a record beneath that folder would not be one of its records, and filters naming no folder need an absolute `/`-prefixed `folder`.
- Ids are the prefix, a hyphen, and the next number zero-padded to three digits, for example `D-002`. Counters are independent, so `A-012` and `D-002` coexist. A declared prefix with no counter starts at zero. An id is never reused. When the next id is already held by a note written outside the tool, the counter is caught up to the highest number of that series in the base's scope, the next free id is assigned, and a warning is returned. A caller-supplied id that a note in the scope already holds is refused (`io/invalid-argument`). The note is rendered and validated first, so content or a title it cannot take, or an existing file at its path, is refused before any id is reserved. The id is then reserved in the sidecar before the note is written, and a change to the sidecar by another process is retried up to three times before the call fails with `io/conflict` and nothing written. A note that cannot be written after its id was reserved fails with `io/key-reserved-note-not-written` and leaves a gap, so ids are unique but not gap-free. Declare an existing hand-authored series with its current last number to avoid the catch-up warning.
- `base_record_create` takes an optional `unique_fields`, 1 to 8 frontmatter field names whose values together identify the record, for example `["origin"]` for a task a workflow raises from some evidence. When a record in the base's scope already holds the same value for every named field, the call is refused with `io/duplicate-record`, whose `path` and message name that record and its id, and nothing is written or reserved. Each named field must be set in `frontmatter` to a string, number, or boolean, and a malformed list is refused with `io/invalid-argument`. The check runs inside the vault's write lock (see below), so of two raisers creating the same record at once, in one process or two, exactly one succeeds. A raiser that may run again uses this to find the record it already raised rather than search first and create if absent, which two runs can both get past.
- A prefix is ASCII letters and digits, starting with a letter, at most 32 characters, and prefixes that differ only by case are rejected: a prefix becomes part of the record's filename. This applies to every prefix a sidecar declares, including the single `key_prefix` of an older sidecar, so an older sidecar whose prefix contains a hyphen, an underscore, or a leading digit is now refused; rename the prefix in the sidecar (and the ids of its existing notes, if you want them to match) to use it.
- A stored sidecar that is malformed (a missing field, an unsafe or duplicated prefix, a counter kept both in `last_id` and in `key_prefixes`, an empty map, a default that is not declared, or `key_prefixes` on a `FILEPATH` sidecar) is refused with `format/base-sidecar` by `base_record_create`. `base_apply` still applies its schema edit and reports the sidecar problem as a warning.
- To add a series to an existing sidecar, edit it by hand: set `key_format` to `'{key_prefix}-{key_id}'` if it is `FILEPATH`, add the prefix to `key_prefixes`, and set each counter to the highest number already in use in that series. For example, a hand-numbered requirements base would declare `FR`, `NFR`, `IS`, and `OS` with their current highest numbers.

#### Series templates

A sidecar can declare the template each key series follows, by hand. The server reads these keys and never writes or alters them, and a rewrite of the sidecar leaves them as they are.

```yaml
template: '[[templates/task|Task]]'          # one link: every series; a one-item list is the same
series_templates:                            # a series of its own overrides template for that series
  D: '[[templates/task-decision|Decision task]]'
  Q: '[[templates/task-question|Question task]]'
fixed_fields: [task_type]                    # fields fixed beyond type, which always is
```

- A link is resolved as every link is, nearest first from the sidecar's folder, and only a name found nowhere nearer is looked up across the vault, where more than one match is ambiguous. A `template` listing several links names no template for a series without its own `series_templates` entry; that series behaves as it did before, and `doctor` reports the sidecar. `series_templates` keys must be declared prefixes, matched case-sensitively, and a `FILEPATH` sidecar cannot declare it. A `template` that is neither a link nor a list of links, a `series_templates` value that is not one string, and a `fixed_fields` list that is empty, longer than 16, repeats a name, names a key the server or sidecar already uses (`type` aside), or names a link field make the sidecar malformed (`format/base-sidecar`).
- `type` is always a fixed field. Another field is fixed only when `fixed_fields` names it and the template declares it. A fixed value must be literal: a number, a boolean, or a non-empty string free of `{{`, `}}`, `<%`, `%>`, and `|`. A template's other values, such as `state: next` or `priority: normal`, are defaults for people using the template in Obsidian and are never stamped or checked.
- `base_record_create` writes the series template's fixed fields on the record, with the template's YAML types, in place of `type: note`. Each field the caller omits is stamped (`type` where the standard default sits, any other after the caller's fields); a supplied value equal to the template's is kept. Every other standard default, `status` included, is unchanged. A base that declares no template, or a series with none, behaves as before.
- A supplied value that differs (including `null`, another type, or another case) is refused with `io/fixed-field-conflict`. Beside `code`, `message`, and `remediation` the error carries `fields`, `series`, and `template` as structured members, and never echoes a value. A template that cannot be used (not found, ambiguous, not Markdown, a base's sidecar (a note beside a `.base` file of the same name, which is never a template, so a link that resolves to the base's own sidecar is refused), larger than the read limit, malformed frontmatter, or without a literal value for `type` or a fixed field) is refused with `format/series-template`, naming the series and the link. Both are refused before any key is reserved, so nothing is written and no number is spent. The template is read on every attempt, so a record is stamped from the template as it stands then.
- Declaring a single `template` binds every series of the base, so check that a sidecar whose one `template` serves series of different kinds also declares `series_templates` for them before upgrading.

A sidecar written before series existed (`key_prefix` plus `last_id`) still works. The first time the server writes it, whether creating a record or refreshing `last_update` after a `base_apply`, it is converted to the `key_prefixes` form and `last_id` is removed. Every other frontmatter key on the sidecar, such as a `template` link, and its body are preserved. The conversion is one way: a server from before this change cannot read a converted sidecar, so deploy the new server everywhere a vault is used before any sidecar is converted or edited into the map form.

### Links between records

`frontmatter_link` edits the cross-reference fields of a note (for example `depends_on`, `blocks`, `related`, and `parent`) instead of leaving each caller to read, edit, and rewrite the whole list. A base declares which fields are links, and which field on the linked note reciprocates, under `link_fields` in its sidecar. The declaration is written by hand; the server never writes or alters it, and a malformed one makes the sidecar malformed, so `base_record_create` and `base_apply` refuse it too (`format/base-sidecar`).

```yaml
link_fields:
  depends_on: {inverse: blocks}
  blocks: {inverse: depends_on}
  blocked_by: {}
  related: {inverse: related}
  on_completion: {}
  parent: {single: true}
```

A field name is an identifier of ASCII letters, digits, and underscores, starting with a letter, at most 64 characters, and not (compared without regard to case) one of the keys a note or sidecar already uses: `id`, `type`, `title`, `entity`, `status`, `state`, `created`, `updated`, `tags`, `aliases`, `description`, `summary`, `excerpt`, `template`, `series_templates`, `fixed_fields`, `link_fields`, `key_prefix`, `key_prefixes`, `key_format`, `last_id`, `last_create`, and `last_update`. Each `inverse` must be declared and must name the first field back.

- **Request.** `operations` holds 1 to 100 `{op, field, target}` entries. `op` is `add` or `remove`, `field` is a declared link field, and `target` is the id of a record in the note's own base (for example `D-002`) or a vault path (a leading `/` is the vault root). `path` must be a Markdown note. All operations apply in order in one atomic write, and every one is validated before anything is written. `expected_hash` is checked even when the note would not change, so a stale hash is refused (`io/conflict`) and nothing, reciprocals included, is written.
- **Which base governs.** A note whose `base` property links to a base that contains it and declares `link_fields` is governed by that base, which settles two bases of equal scope; a `base` naming anything else is ignored. Otherwise a note's link fields come from the narrowest base whose folder scope contains it and whose sidecar declares `link_fields`. A base declaring none does not shadow a wider one, a base whose filters name no folder covers the whole vault, and two of equal scope are refused. The scope is the folder the base's top-level filters confine its records to, read the way `base_record_create` and `base_read`'s `records_folder` read it: a `file.inFolder("...")` call or a `file.folder == "..."` equality that every record must satisfy. A `.base` file that cannot be parsed is skipped, so it cannot govern a note. A sidecar that is malformed or unreadable, on a base whose scope contains the note, might have been the governing one, so the call is refused with that sidecar's error, which names the base, unless the note's own `base` property names a base that contains it and declares `link_fields`. The same sidecar then does not block the call, so a stray vault-wide `.base` beside a note that is not a sidecar cannot stop links on notes that name their base. A base folder or an id containing a quote, a backslash, or a control character is refused (`io/invalid-argument`). `changes[].target` and `reciprocal[].path` are the linked note's vault-relative path including `.md`.
- **Targets.** An add needs the target to exist. A remove also accepts the vault path of a note that no longer exists, so a dangling link can be cleared, and writes no reciprocal for it. Links match by note, whatever their heading, display text, or case, and whether written by full path or by bare name. An id containing a quote, a backslash, or a control character is refused; pass the vault path instead. A target reached through a symlink into another vault root is not found.
- **Idempotent.** Add does nothing when the link is there, and remove does nothing when it is not. A list keeps its other entries and an emptied list stays `[]`; a single-valued field is deleted on remove, and adding a different link to a set single-valued field is refused (`link/single-valued`). Existing entries are kept exactly as written.
- **Link form.** A quoted wikilink with display text, for example `"[[d-002|D-002]]"`. The display text is the target's `id`, else its `title`, else its file name, taking the first that can be written in a wikilink. The target is written relative to the governing base when the note is in its scope: the bare key (`d-002`) for a keyed base whose file is named for its key, else the path within the base's folder without `.md` (`entity/offer/d-002`). A note outside the base is named by its full vault path without `.md`, which is the form Obsidian resolves most reliably. Matching on repeat calls recognises every form, and a unique bare file name as well. `contextos-web` shortens these for display and resolves each to its screen, see [`web-links.md`](web-links.md).
- **Reciprocals.** For a field with an `inverse`, the same operation is applied to that field on the linked note, so `depends_on` on one note and `blocks` on the other stay in step. This happens only when the linked note's governing base declares the pair, and it is skipped, with no warning, when it does not. The note's own write comes first. A reciprocal whose write fails is reported in `warnings`, and repeating the same call repairs it, because a reciprocal is attempted even when the note's own entry was already there. A reciprocal skipped because of a problem on the linked note (a malformed field, a single-valued field already set) is also a warning, and repeating the call helps only once that is fixed.
- **Result.** `{path, content_hash, changes: [{field, target, link, outcome}], reciprocal: [{path, field, outcome, written}], warnings}`. `outcome` is `added`, `already-present`, `removed`, `absent`, or, for a reciprocal only, `skipped`. For a remove, `link` is the stored entry that was removed.
- **Errors.** `link/undeclared-field` (naming the declared fields), `link/no-link-fields`, `link/ambiguous-base`, `link/target-not-found`, `link/target-ambiguous`, `link/self-reference`, `link/single-valued`, `link/malformed-field` (a field holding neither a wikilink nor a list of them), and `link/unlinkable-text` (a name containing `[`, `]`, `|`, `#`, `^`, or a line break). A note or sidecar that cannot be parsed reports `format/frontmatter`, and a malformed declaration reports `format/base-sidecar`.
- **Cost.** Resolving ids, finding the governing base, and judging whether a name is unique read the vault, once per call for the name index and the bases, and once per distinct id. The write lock is held throughout, so very large vaults will feel it.
- The web tasks screen writes its own link chips through `frontmatter_update` and does not use this tool yet.

### Querying a base and looking records up by id

`base_query` runs a view of an existing `.base` (`path`, optional `view`) or an inline `definition`, and returns the matching notes as a Markdown table (default), JSON, or CSV.

- **Filter grammar.** `==` and `!=` against a quoted string, number, `true`, `false`, or `null`; `property.contains("value")`; `property.containsAny("a", "b", ...)` and `property.containsAll("a", "b", ...)`, which on text test for each value as a substring and on a list test for each value as an item, as Obsidian does; `file.hasTag("tag")`; `file.inFolder("folder")`; combined with `&&`, `||`, `!`, and parentheses in a string, or `and`, `or`, and `not` nodes in YAML. A filter naming a formula, or using any other construct, is refused with `query/base-query-unsupported-filter`, naming the construct and where it sits in the filter tree. The whole filter is checked before any note is read, so the refusal comes even when no note would have reached the construct.
- **Lookup by id.** `ids` (with `path`) returns the records of the base whose frontmatter `id` matches, from the notes the base's own top-level `filters` select, never a view's. One call replaces a query and a read per record, which suits following a task's `depends_on` or `parent`. Each record in `records` carries `id`, its vault-relative `path`, its `content_hash` (the hash of the note as read, usable as a later write's `expected_hash`), and its `frontmatter`, plus its body as `content` when `include_content` is `true`. Records come back in the order their ids were asked for; an id no record in scope holds is listed in `not_found`; an id held by more than one record returns each of them with a `query/base-query-duplicate-id` diagnostic. `ids` takes 1 to 100 distinct, non-empty ids, requires `path`, and is refused with `io/invalid-argument` alongside `definition`, `view`, `limit`, or `format`, as is `include_content` without `ids`.
- **Claiming a record.** Look the record up, check it is unclaimed or its claim has lapsed, then `frontmatter_update` it with the looked-up `content_hash` as `expected_hash`. The update is applied only if nothing has changed the note since it was read, so of two sessions claiming the same version, in one process or two, exactly one succeeds and the other gets `io/conflict`.

### Writes from several processes

One operator usually runs several `contextos` processes against the same vault at once: one per agent session, scheduled or interactive, and the one ContextOS Web holds. Every mutating tool call holds the vault's write lock for its whole duration: an in-process lock, and an operating-system advisory lock on `vault-write.lock` in the vault's state directory, which every process run by the same user on the same host resolves to the same file. A conflict-checked write (`expected_hash`) therefore cannot be interleaved with another process's write to the same vault, and checks that span several reads and writes in one call, such as `unique_fields`, hold across processes too. The operating system releases the lock when its holder exits, so a crashed process never leaves a vault locked. Reads do not take the lock. The advisory lock and its queue live in `contextos-fs` (`lock_vault_writes`), so any crate that mutates a vault, not only `contextos-mcp`, takes the same lock without depending on the MCP adapter; `contextos-mcp` adds the in-process lock in front of it.

- Waiting calls are served in the order they asked, through a queue of ticket files beside the lock (`vault-write.lock.queue/`), so a process that writes continuously cannot starve another: releasing the lock and asking again puts it behind everyone already waiting. A ticket is held under its own advisory lock, so one left by a process that died while waiting is removed by whoever finds it. An older build that polls the lock without queueing still contends for it, so the order holds among processes of this build.
- A call waits up to 30 seconds for another process to finish writing the same vault, queueing included, then fails with `io/vault-busy`, naming the lock file. A wait that long means another process is stuck mid-write.
- A lock file that cannot be created, opened, or locked fails the call with `io/write-lock`. The state directory must be on a local disk: advisory locks are not dependable on network or synchronised folders, which is why the lock is not kept in the vault.
- Two processes that resolve different state directories for the same vault (a different user, host, or `state_directory` setting) do not share the lock.
- The vault's derived stores (the text index, the link graph store, and the vector store) are shared the same way: each serialises writers across processes with a bounded wait (5 seconds) and fails a write still blocked as `index/busy`, which reaches the vault write as a warning. Their health, and what happens to a damaged one, is described in [`search.md`](search.md#index-health-and-recovery).

### Events

The server writes a note under `memory/events/` for things worth seeing, so screens and agents can follow what happened without reading every record. Events are not a second operations log: `memory/log/` already records every operation. An event is one note at `memory/events/{YYYY}/{MM}/{YYYY-MM-DD-HHmmss}-{slug}.md` (UTC, with a `-{n}` suffix when the path is taken) whose frontmatter holds `type: event`, `kind`, `severity` (`info`, `warning`, or `error`), `at`, `entity`, then `subject`, `subject_id`, `subject_path`, `subject_base`, and `actor` where they apply, then the kind's own fields. It carries none of the common note fields (`title`, `created`, `updated`, `tags`, `aliases`, `description`) and never copies a note's content.

**Subjects.** A record of a keyed base, such as a task, is referenced by its key, never its path: `subject` is `[[a-001|A-001]]` (by its file name, labelled with its key, when the file is not named for its key), `subject_id` is the key, `subject_base` is the base it is a record of, and `subject_path` is where it was when the event happened. Any other note is linked by its path and carries only `subject_path`. An event is about the same subject as another when both name the same base and key (keys compare without regard to case), or, for a note that is no record, the same path. So a task stays the same subject wherever it is filed, and a key two bases share (a `D-` decision task and a `D-` register decision) names two subjects.

**What the server fires.** No client needs to record these, and none can:

- `task.created`, `task.state-changed`, `task.reassigned`, `task.claim-lapsed`, and `task.unknown-state`, when a completed tool call creates a task, turns an existing note into one (for example `note_update` or `frontmatter_update` setting `type: task` on a note in a base's folder), or changes its `state`, `assignee`, or claim. A task is a note with `type: task` that `memory/tasks.base` selects. The server compares the task with a table of its last-written values kept in the vault's state directory, so each change is recorded once across every process. A task changed outside ContextOS (in Obsidian, or by sync) fires nothing until the next tool call writes it, which records the combined change. A task the table has not seen, written for the first time by a tool call, is announced as created, including one first made outside ContextOS; a write to a task the table already holds never fires `task.created` again. A missing or unreadable table is rebuilt from the task notes with no events, and the write that finds it missing cannot tell a new task from an old one, so it fires no `task.created`. Which states a task type declares comes from `.contextos/tasks.toml`, or the shipped lifecycles without one.
- `task.moved`, severity `info`, when a completed move takes a tracked task to another path within the task base's folder, whether the task note itself was moved or a folder holding it was, one event per task. It carries `from` and `to`, both vault paths, and its subject is the task where it now is. A move clears no attention: a warning about the task stays open until something else happens to the task. A task moved out of the task base's folder, or deleted, fires nothing and is no longer tracked.
- `member.state-changed`, when `agent_state_change` succeeds. The note is `type: event`, `kind: member.state-changed`, severity `warning` when the new state is `blocked` and `info` otherwise, `entity: vault`, a `subject` linking `registry/agents/{slug}.md` when that note exists, and the fields `member`, `from` (absent on a first transition), `to`, and `reason`. The tool's name, parameters, and result are unchanged.
- `vault.degraded` and `vault.recovered`, when a completed write returns a warning from a secondary service (Git, the managed index, search, projection) that was not already current, and when a later write completes without it. Every warning code is classified once as a failure or a notice; a notice, such as `projection/hand-edited-replaced` or `write/line-endings-converted`, fires nothing. The event carries the `code` and `service`, never the warning's message.

These events are written as internal operations: Git stages them with the write that raised them, and neither the managed index nor the operation log records them. A write the server makes for itself never fires an event, so an event note cannot raise another. When an event note cannot be written the tool call still succeeds and returns a warning with code `events/write-failed`.

**Recording a declared kind.** A vault declares the kinds clients may record in `registry/events/`, and the sidecar of `memory/events.base` (`memory/events.md`) names that folder with `event_kinds: registry/events` (the registry that recording, the retention sweep, and `doctor` all read) and maps each key series to the kind whose key it assigns with `series_kinds`, as a task sidecar maps a series to its template with `series_templates` (for example `series_kinds: {S: session.run-started}`). Record an event with `base_record_create` against `memory/events.base`:

- Give `kind` and, optionally, `severity`, `subject`, `entity`, and the kind's own fields in `frontmatter`, and the event's text in `content`. The server stamps `type`, `at`, and the path, and writes the note from the shape every event shares; it does not name the file from the key series.
- `subject` is one whole wikilink on one line; a bare path, an embed, or a link inside other text is refused. It is resolved as every link is (nearest the events folder first, then by name), before the vault's write lock is taken, and the note it found is checked again under the lock. The server reads that note and stamps `subject_path`, and `subject_id` and `subject_base` when the note is a record of a keyed base (its `id`, and `memory/tasks.base` for a task that base selects, else its `base` property). A record must be linked by its key, `[[a-001|A-001]]`, or, when its file is not named for its key, by its file name labelled with its key, `[[ship-it|A-001]]`, the form the server writes; linking it by its path is refused. A link that names no note is kept as given, with nothing stamped, so check that it names the note you mean. `subject_id`, `subject_path`, and `subject_base` are never supplied.
- When the kind declares a key field (`id_field`), pass `prefix` naming a series the sidecar maps to the kind, and the server assigns the next key of that series under the vault's write lock, writes it to that field, and returns it as `key`. Read the key from the result and use it; never supply it or make one yourself. Keys are zero-padded as every key is, so the first is `S-001`. A kind whose definition names a key field is usable only while exactly one series in `series_kinds` maps to it.
- `unique_fields` works as for any record, so a recorder that may run again finds the event it already recorded (`io/duplicate-record`).
- `prefix` is required for a kind with a key field and refused for any other kind, whose result has no `key`. `filename` and `folder` are not accepted for an events base.

Nothing is written and no key is reserved when a call is refused with one of these codes: `events/unknown-kind` (the kind is missing, undeclared, or its definition cannot be used), `events/server-fired-kind` (the server fires this kind), `events/invalid-severity`, `events/invalid-fields` (a required field is missing, a field is neither declared nor common, a common note field was supplied, or a value is not a single-line string, number, boolean, or list of those), `events/invalid-subject` (a subject for a kind with none, none for a kind that requires one, a subject that is not one line, a subject that is not one whole wikilink, a record linked by its path rather than its key, or a link that names more than one note), `events/invalid-series` (`prefix` is missing for a kind with a key field, given for a kind without one, or names a series the sidecar's `series_kinds` does not map to the kind), and `events/server-stamped` (`at`, a path, `filename`, `folder`, `subject_id`, `subject_path`, `subject_base`, or the kind's key field was supplied).

**Only the server writes in `memory/events/`.** Every other tool that would write, edit, move, or delete there, `force` notwithstanding, is refused with `events/server-owned` before anything changes. So is moving or deleting a folder that contains `memory/events/` (such as `memory`) or moving a folder onto one, since either could change events out of the guard's sight, and `git_restore` always leaves `memory/events/` alone whatever `restore_exclude` lists. `memory/events/index.md` is ordinary content. Events are removed only by the retention sweep.

**Only contextos-web writes in `.contextos/packages/`.** The vault's installed workflow packages and libraries live there, and a tool edit would change what a job runs. Every tool that would write, edit, move, or delete below it, `force` notwithstanding, is refused with `packages/server-owned` before anything changes, as is moving or deleting a folder that contains it (such as `.contextos`) or moving a folder onto it, and `git_restore` always leaves it alone whatever `restore_exclude` lists. Nothing below it is open. Because the server never writes there, `contextos-web` changes the store without taking the vault's write lock; the store has its own lock, `.contextos/packages/.lock`. The rest of `.contextos/` (such as `jobs.toml`) and `registry/` stay writable. Each server-owned folder has its own code and remediation, configured in `server_owned_folders`.

**Retention.** `[vault.events]` sets `retention_days` (default 90, zero refused with `config/invalid-retention`) and `keep_attention` (default `true`). The server sweeps each vault when it starts serving and once a day after that, under the vault's write lock, and records the day in the state directory so that of several processes only the first to see a new UTC day sweeps. It deletes `info` events older than their window (their kind's `retention_days` when the definition declares one), keeps a `warning` or `error` event while `keep_attention` is `true` and it is still open (and sweeps it by the same window once something has cleared it, never before the event that cleared it), keeps an event whose `at` cannot be read, and removes month folders it empties. It deletes only notes whose `type` is `event` (a note of any other type under the folder is kept and reported by `doctor` under `event_unreadable`) and never follows a symbolic link or other reparse point out of the folder. It judges age by the server's clock, the same instant every event's `at` is stamped from. Git keeps every deleted event.

**Never decide work from events.** What to work on is read from the task records, never from events. Events say what happened, they can lag a change made outside ContextOS, and a vault's event store may be swept.

`doctor` reports `event_kind_invalid` (a definition that cannot be used: unreadable, a name that differs from the file's, a server-fired kind, a key field that no series in `series_kinds` maps to, or a kind that more than one series maps to), `event_kind_duplicate` (a kind defined in more than one file), and `event_unreadable` (an event note with no readable `at`, or a symbolic link in the events folder, which the sweep never follows), `event_kind_truncated` (a registry folder with more definition files, or nesting, than the server reads, so some kinds cannot be recorded), and `event_kind_misplaced` (an `event_kinds` entry in the sidecar of any base other than `memory/events.base`, where it has no effect: only the vault's events base records events), each naming the files and none auto-fixable. A `memory/events.base` that cannot be addressed (for example a link out of the vault) is reported under `event_kind_invalid` and does not stop the rest of the report. A vault with no kind registry and no events gets none of them.

## Query

| Tool | Purpose |
| --- | --- |
| `query_text` | Ranked full-text search across vault markdown with path, tag, frontmatter, and note type filters; paged, sortable, with a minimum relevance |
| `query_semantic` | Vector similarity search over chunked note content; paged, sortable, with a minimum relevance; requires `[vault.search] semantic = true` |
| `query_blended` | Full-text and semantic search fused by reciprocal rank fusion into one file-level ranking; degrades to whichever index is available |
| `query_graph` | Traverse the link graph (wikilinks and Markdown links): neighbours, backlinks, shortest path, or orphans (notes with no links in or out), plus two integrity operations, `unresolved` (every link that finds no file or several, and every note whose links cannot be read) and `unlinked` (every note no other note links to), which return `links` or `notes` with `total` and `offset` and take `path` (an existing folder of the queried vault, validated like any other `path`), `offset`, and `limit` (see [`search.md`](search.md#link-integrity)). Each edge's `kind` is the form the link was written in: `wikilink`, `wikilink-embed`, `markdown`, or `markdown-embed`. Each node's `kind` is `note`, `file` (an existing file that is not an indexed note), `not-found`, or `ambiguous`; `phantom` is true for the last two; an `ambiguous` node lists the files it answers to in `candidates`, and a note whose links could not be read carries the reason in `unreadable`. The graph is brought up to date with files changed outside the server before it answers |
| `query_index_status` | Report per-index document counts, staleness estimate, last build time, and health, naming why any index cannot be read and where a damaged one was moved aside |
| `query_index_rebuild` | Rebuild the text index, link graph, and/or semantic index from a full vault scan, first moving a damaged store aside; lists any store it recovered in `recovered` |

The paging rule, relevance definitions, blended search, and hit metadata are described in [`search.md`](search.md).

## Git recovery

| Tool | Purpose |
| --- | --- |
| `git_init` | Initialise recoverable local Git history for a vault |
| `git_commit` | Commit all pending MCP-owned staged paths immediately |
| `git_restore` | Restore historical content as new forward mutations, without rewriting history |
| `git_status` | Report branch, staged, unstaged, untracked, and pending MCP paths |
| `git_log` | Read local commit history with an optional path filter |
| `git_diff` | Return a size-capped unified diff between refs or the working tree |

### How mutations are committed

Every change an MCP tool makes to a vault with Git initialised is staged as it is made, and committed once the vault has been quiet for `commit_debounce_s` seconds. This holds for every mutating tool, and for the writes they cause (index maintenance, operation-log lines) as well as the write itself. The tools that write on their own account, `vault_index_rebuild`, `vault_log_append`, and `doctor_resolve`, are held to the same rule: each hands every write it makes to the same Git and search services, so none can be left for the operator to notice and commit. With every change arriving through MCP, nothing stays uncommitted once the vault has been quiet for the window.

- **A rolling window, with a ceiling.** Each change restarts the quiet period, so a vault under continuous activity defers its commit. To stop that going on indefinitely, a batch that has been open for ten windows is committed by the next change, whatever has arrived since.
- **One commit per operation.** A whole-vault `vault_index_rebuild` is a single commit, not one per index. Changes made within the same window share a commit.
- **The commit names its work.** The subject is `mcp: <tool> (<n> files)`, naming the tool or tools that made the change (`mcp: vault_index_rebuild (748 files)`), and the body lists what was done, up to 50 lines and then a count. Work an internal service does on a tool's behalf carries that tool's name.
- **The operation log is part of the operation.** Log lines written for an operation are committed with it. A manual `vault_log_append` is its own operation and commits with its own entry.
- **A failed commit is reported and retried.** The write has already succeeded, so it is never rolled back. The paths stay pending, the commit is retried at the debounce interval (at least one second) up to four attempts in all, and `git_status` reports `commit_failure` (its `code`, `message`, and `attempts`) until a commit succeeds. Any later mutation starts a fresh window.
- **`doctor` flags what should not exist.** MCP-made changes still uncommitted after the vault has been quiet for the window fail the `Uncommitted MCP changes` check, and `doctor_resolve` (or `contextos doctor --resolve`) commits exactly those recorded paths. A batch that is still receiving changes is inside its rolling window and is not flagged. Edits made outside MCP, in Obsidian for example, are only counted in the check's message: they are never a failure and never committed by the server.
- **A one-shot `contextos doctor --resolve` commits before it exits**, since no debounce timer outlives the process.

What this does not cover, so that it is not mistaken for a guarantee:

- A process that dies in the instant between a write completing and that write being staged leaves a file no record mentions. It cannot be told apart from an operator edit, so the doctor counts it as one. A rebuild that dies part-way can leave up to 49 written indexes in the same state (routing is done in batches of 50). Closing this needs the intent to be recorded before the write, which is a change to the write pipeline and is not part of this behaviour.
- A pending record left behind after a successful commit, followed by an operator edit to the same path, can cause that edit to be committed with the next recovery.

## Diagrams

| Tool | Purpose |
| --- | --- |
| `mermaid_validate` | Validate a Mermaid diagram from a note's fenced or inline source, without rendering |
| `mermaid_render` | Parse, lay out, and render a Mermaid diagram to SVG |

## Diagnostics and maintenance

| Tool | Purpose |
| --- | --- |
| `doctor` | Report read-only health checks for the effective configuration; identical content to `contextos doctor`. Each check carries a stable `kind` and, unless global, its `vault_name`; an auto-fixable check names its stable repair code in `remediation`, and `remediation_tool` only when a callable MCP tool performs it. Each vault's `link_integrity` check ("Link integrity") passes with the number of links checked, or fails, never auto-fixably, with counts and the first ten broken links (see [`search.md`](search.md#link-integrity)). Each vault's `record_conformance` check ("Record conformance", which replaced `base_property`) reports, and never repairs, records whose fixed fields differ from their series template, `base` properties that do not name a base selecting the note (naming the one base that does), and sidecars whose template cannot be used, listing at most 10 of each without showing any value. When the configuration projects definitions to agent hosts, it also reports projection findings (see below). `path` limits the report to one vault plus the global check |
| `doctor_resolve` | Resolve every currently auto-fixable doctor finding; each outcome carries its `kind` and, when a configured vault applies, its `vault_name` and the vault's `vault_path` |
| `vault_info` | Report server, transport, effective vault configuration, and substrate health, without exposing secrets. `vaults` is in configuration order, which decides which vault's agent and skill definitions win, for host projection and for the plugin's lookups. Also reports `time_zone`: the IANA zone every time written into a vault is shown in, its source (`configured`, `system`, or `undetected`), its offset now, and, for a host zone with no IANA name, the POSIX `rules` it was built from |
| `vault_index_rebuild` | Rebuild managed `index.md` files for every non-excluded folder in a subtree |
| `vault_log_append` | Append one explicit manual entry to the shared daily operation log |
| `agent_state_change` | Record a team member's working/blocked/idle/awaiting-input state transition in `.contextos/team.toml`, with one `member.state-changed` event under `memory/events/` (see [Events](#events)) |

### Doctor remediations

Every auto-fixable finding names its stable repair code in `remediation`. When a callable MCP tool performs the repair the code is that tool's name (`vault_index_rebuild`, `git_init`, `git_commit`, `query_index_rebuild`) and `remediation_tool` repeats it. A repair the server performs itself has a code of its own and no `remediation_tool`, because a client reading that field must only ever be handed a tool it can call.

### Agent host projection findings

`doctor`'s `team_config` check also reads `.contextos/team.toml` as the vault's jobs do: it fails, never auto-fixably, naming the rule and never a member's content, when the file does not parse, is not text, has a slug that is not lowercase letters, digits, and hyphens, shares a slug between two members, or does not have exactly one member with `chief_of_staff = true`; a vault with at least one saved job (`registry/workflows/*/jobs/*.toml`) and no `team.toml` fails it with the rule `file-absent`, and a vault with no job and no `team.toml` passes with an advisory to add one before the first job. The `agents.toml` migration finding is unchanged. `doctor` reports only passes and failures.

When the configuration projects definitions to Claude Code and Codex, `doctor` reads each host's folders without writing and reports, one check per finding and per host where it applies, under these kinds:

| Kind | Auto-fixable | Meaning and repair |
| --- | --- | --- |
| `projection_stale` | yes, `reproject` | A projected agent or skill is missing or differs from what the vault now defines |
| `projection_hand_edited` | yes, `reproject` | A projected file was edited outside ContextOS. Re-projecting replaces the hand edit |
| `projection_orphaned` | yes, `withdraw_projection` | A projected entry the server owns whose definition no longer exists. Withdrawing removes it. Recorded against a vault that is no longer configured, it names no vault |
| `orientation_block` | yes, `restore_orientation` | The block between the orientation markers in a host's global instructions is missing or differs. Restoring rewrites only the lines between the markers. A malformed block is reported without a remediation |
| `projection_invalid_definition` | no | A definition fails the projection contract |
| `projection_shadowed` | no | A later vault defines a slug differently from the earlier vault that wins it |
| `projection_unowned_collision` | no | A file or folder the server did not project sits at a projected slug. It is never touched |
| `projection_foreign_owner` | no | An entry is recorded as projected by another configuration, whether or not this configuration defines that name. The message names the other configuration by the first 12 hexadecimal characters of its identity; `doctor`'s `configuration` check shows this configuration's own when projection is on. It is never changed |
| `codex_read_only_unenforced` | no | A read-only agent projected to Codex, which cannot enforce it |
| `codex_override_hides_agents` | no | An `AGENTS.override.md` beside Codex's instructions makes Codex ignore them |
| `projection_busy` | no | Another ContextOS process held the host-level projection lock for longer than the bound, so projection was not assessed. The message carries `projection/busy`. Every vault check is still reported and `doctor_resolve` still repairs vault findings; retry once the other process finishes |
| `projection_lock_failed` | no | The host-level projection lock could not be opened (for example `projection.lock` is a folder), so projection was not assessed. The message carries `projection/lock-failed`. Every vault check is still reported and `doctor_resolve` still repairs vault findings, but resolves no projection finding |

Findings are host-level, so the server composes them under the host-level projection lock, taken after any vault write lock. A configuration that does not project reports none of these kinds, takes no lock, and never touches a host folder. `doctor_resolve` repairs only the four auto-fixable kinds, once per finding, and only when `path` is omitted; with `path` set, `doctor` lists every finding that names that vault, including a `projection_shadowed` finding where it is the later, shadowed vault, and `doctor_resolve` leaves them. `doctor_resolve` judges each projection repair by a fresh assessment. If that assessment reports `projection_busy` or `projection_lock_failed`, every projection outcome is `resolved: false` and says the result could not be verified because another process held the lock (or the lock could not be opened); an outcome whose repair raised a failure for that host and definition is never reported resolved. A dry run plans the same outcomes, each saying it would act, without writing. `contextos doctor` lists the same findings, and `contextos doctor --resolve` (with `--dry-run` to preview) repairs them exactly as the tool does.

A mutating tool's `warnings` can carry projection codes too, never as a failed write. `projection/cleanup-incomplete` means a skill folder was projected and recorded, but the previous copy could not be removed straight away; the next run removes it, and nothing is needed from the operator unless the warning repeats. `projection/busy` means another ContextOS process held the host-level projection lock past the bound (5 seconds for a write; start-up refresh, `doctor`, and `doctor_resolve` wait 30), so nothing was projected for that write. The other projection codes a write can return are `projection/invalid-definition`, `projection/unowned-collision`, `projection/foreign-owner`, `projection/path-rejected`, `projection/reparse-point` (a Windows reparse point that is not a link, such as a cloud placeholder), `projection/read-failed`, `projection/write-failed`, `projection/lock-failed` (the lock file could not be opened), `projection/manifest-malformed`, `projection/malformed-block`, and `projection/hand-edited-replaced`. The last means a write changed the source of an agent whose projected copy had been edited by hand, and replaced the edit; a write elsewhere in the registry leaves a hand edit in place. A nested `index.md` inside a skill folder is the vault's folder index, not part of the skill, so it is never projected and writing it is not a projection event.

### Managed `index.md` maintenance

Every directory that is not excluded carries an `index.md`, reconciled whenever a write changes its contents and rebuilt in full by `vault_index_rebuild`. Both paths run the same code, so they behave identically.

- **Links.** The managed block lists each entry as a vault-relative wikilink with forward slashes at every depth and on every platform, such as `[[memory/wiki/analytics/index.md|Analytics]]`. Text outside the managed block is never touched.
- **Frontmatter is completed, never overwritten.** A missing frontmatter block is added, and a partial one gains only its missing keys (`type`, `title`, `entity`, `created`, `updated`, `tags`, `aliases`, `description`, `summary`, `excerpt`). Existing values, comments, quoting, and key order are kept exactly. No `status` is ever written.
- **`entity`** comes from where the directory sits, using the slugs in the vault's `.contextos/entities.toml`: a top-level entity directory and everything beneath it belongs to that entity, and only that: a folder named for an entity anywhere below the first path segment, such as `memory/tasks/<entity>/`, does not make its notes that entity's. Every other path (`memory/` generally, `registry/`, `templates/`, `standards/`, `scripts/`, the vault root) is `vault`. Matching ignores ASCII case and writes the configured spelling. A vault with no `entities.toml` gets no `entity` key at all rather than a guess, and gains it on the first rebuild after the file exists. A malformed `entities.toml` fails an explicit `vault_index_rebuild` with `index/entities` before anything is written, while ordinary writes carry on maintaining indexes without an `entity` until the file is fixed.
- **`created`** for a file that lacks one is the date it entered the vault's first-parent Git history (for a file removed and later added back, when it came back) where the vault is versioned, else the filesystem birth time, else the rebuild time. Version history is only consulted for a file that exists and needs the value, and the walk stops where the file appeared. The filesystem birth time is a weak source for a file the server has rewritten, since every managed write replaces the file, and a copied or restored vault reports the copy time.
- **`updated`** is set to the rebuild time only when the file's content actually changes, so a rebuild that changes nothing produces no diff.
- **Frontmatter the editor cannot address** (flow style such as `{type: index}`, quoted keys, or a value split by blank lines) is kept exactly as found, the managed block is still rebuilt, and the rebuild result lists the file under `warnings`. Rewrite it as plain block-style YAML, one `key: value` per line, to have it completed.

`vault_index_rebuild` takes an optional `repair` flag (default `false`) for vaults where an earlier defective rebuild wrote wrong frontmatter to every index. With `repair` set, each managed `index.md` also has these corrected, and every correction is listed in the result's `repairs` (one entry per file, each with `actions` of `{property, from, to}`, where `to` is `null` for a removed property):

- an `entity` that disagrees with the directory's location becomes the derived value, unless the vault's entities are unknown;
- a `status: new` is removed; and
- a `created` that merely repeats `updated` becomes the date Git history or the filesystem reports, but only when that date is earlier, so a correction never moves `created` later.

`repair` overrides values that may have been set on purpose, so run it with `dry_run` first to read the list, and commit the vault beforehand. It never touches anything outside frontmatter that an ordinary rebuild would not, and a second `repair` run reports nothing.

## Ephemeris (optional)

Registered only when `[server] astro` (or `--astro`) is enabled; see [`configuration.md`](configuration.md). Every calculation is computed offline, with no network access.

| Tool | Purpose |
| --- | --- |
| `ephemeris_moon_phase` | Compute the Moon's phase for a calendar date: name, illumination fraction, and proximity to each primary phase |
| `ephemeris_solar_events` | Compute the exact UTC instant of both solstices and both equinoxes for a calendar year |
| `ephemeris_wheel_of_year` | Compute all eight Wheel-of-the-Year points for a calendar year, hemisphere-correctly named |
| `ephemeris_personal_year_period` | Compute which of the seven annually-recurring personal-year periods contains a given date, and its ruling planet |
| `ephemeris_boundaries` | Aggregate every horizon boundary or checkpoint crossed within a date range in one call |
