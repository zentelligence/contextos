# Search

Three tools search a vault's notes: `query_text` (full-text), `query_semantic` (vector similarity), and `query_blended` (both, fused into one ranking). They share one set of result controls, one paging rule, and one description of each hit. `query_graph` and the index tools are covered in [`mcp-tools.md`](mcp-tools.md).

## Choosing a tool

| Tool | Use it when | Needs |
| --- | --- | --- |
| `query_text` | You know words the note contains, or need tag and frontmatter field filters | `[vault.search] text = true` (the default) |
| `query_semantic` | You know the idea but not the words | `[vault.search] semantic = true` and an embedding provider |
| `query_blended` | You want the best of both in one list, for example a search box | Either index; both for fusion |

## Result controls

Every search tool accepts these optional parameters alongside its query.

| Parameter | Meaning | Default |
| --- | --- | --- |
| `limit` | Page size, at most 100. `0` returns only `total` | 20 (`query_semantic`: 10) |
| `offset` | Number of ranked results to skip | 0 |
| `sort` | `relevance`, `title`, or `modified` | `relevance` |
| `order` | `asc` or `desc` | The natural direction for the key: most relevant first, most recently modified first, titles alphabetical |
| `min_relevance` | Drop hits below this relevance, from 0 to 1 | 0 (keep everything) |
| `note_type` | Keep only notes whose frontmatter `type` equals this value | No filter |
| `path_prefix`, `exclude_paths` | Scope the search to, or away from, folders (whole path segments) | No scope |

`query_text` additionally accepts `tags` and `fields` (frontmatter equality filters). Those are properties of the text index, so `query_blended` does not offer them: applying them to semantic hits would let notes that violate the filter through.

Ties on any sort key are broken by ascending path, so the order is always total and repeatable. This includes hits with equal full-text scores, which are ordered by path. Sorting by title is case-insensitive, and a note that cannot be read sorts by its path (title) or as oldest (modified). Notes saved within the same second are still ordered by their full-precision modification time.

## The paging rule

One rule applies to all three tools: a page must end within the first 200 results, so `offset + limit` cannot exceed 200, and `limit` cannot exceed 100. A request outside that is rejected with the stable code `index/invalid-query` and a remediation hint, before any index is read.

Behind that rule the tools differ in how many matches they count.

| Tool | `total` | `total_capped` |
| --- | --- | --- |
| `query_text` | Every matching note, exactly, even beyond the 200 you can page to | Not reported (always exact) |
| `query_semantic` | Chunks among the 200 most similar that pass the controls | `true` when more than 200 chunks matched, so more exist than `total` counts |
| `query_blended` | Exact in `text` mode. Otherwise the files across both candidate pools that pass the controls | `true` when a pool was truncated |

To narrow a large result set, add filters or raise `min_relevance` instead of paging deeper.

Sorting works over different sets. `query_text` orders the whole match set and then applies the 200-result window, so `sort=title` really starts at the alphabetically first match. `query_semantic` and `query_blended` sort within their 200-candidate pools, because the pools are all the vector search produces.

## What relevance means

Relevance is a fraction of an ideal match, from 0 to 1, so a threshold means the same thing for every query. What it is a fraction of depends on the tool.

| Tool and mode | Relevance |
| --- | --- |
| `query_text`, and `query_blended` in `text` mode | The hit's score divided by the best hit's score for the same query. The best hit is always 1 |
| `query_semantic`, and `query_blended` in `semantic` mode | Cosine similarity, clamped to 0 to 1 |
| `query_blended` in `blended` mode, including when it degrades to one index | The fused rank score divided by the best score a single file could reach across the indexes that ran |

`query_text` and `query_semantic` also report the raw `score`, comparable only within one result set.

## Blended search

`query_blended` runs the text and semantic searches and merges their rankings with reciprocal rank fusion. Each index contributes `1 / (60 + rank)` for every file it ranks, and a file's fused score is the sum. Fusing by rank rather than by score is deliberate: full-text scores and cosine similarities are on unrelated scales.

- Semantic chunks are collapsed to one entry per file, keeping the best chunk.
- Files found by both indexes tend to rank above those found by one, but rank fusion does not guarantee it: a file ranked first by one index can outscore a file ranked far down both. A file ranked first by only one of two indexes has relevance 0.5, because it could have scored twice as much.
- Each hit lists the indexes that matched it in `matched_by` (`text`, `semantic`, or both) and carries a `snippet`: the highlighted text snippet when the text index matched, otherwise the best semantic chunk.

`mode` selects what is consulted: `blended` (the default), `text`, or `semantic`. The single-index modes use that index's own relevance definition above and never fall back.

### When an index is unavailable

In `blended` mode, an index that is disabled, an embedding provider or vector store that fails, and a query the text index cannot parse are each skipped, and the other index's results are returned with an entry in `warnings`. Each warning is a string of the form `code: message`, where the code is the stable code of the underlying condition, for example `index/disabled`, `embedding/timeout`, or `index/invalid-query`. The remaining results are ranked by rank fusion over the index that ran, so relevance means the same thing however many indexes answered: the top hit is 1, and each rank after it scores `61 / (60 + rank)`.

A natural-language query is a common cause of the last case. Full-text search reads a colon as a field search and parentheses and quotes as grouping, so `why did we pick sqlite: notes` does not parse as text. Blended search still answers it by meaning and says so in a warning. In `semantic` mode the query is embedded exactly as written and full-text syntax is never read.

| Situation | `mode = blended` | `mode = text` or `semantic` |
| --- | --- | --- |
| Semantic search off, or its provider failing | Text results and a warning | `semantic`: the stable error |
| Text index off | Semantic results and a warning | `text`: `index/disabled` |
| Query the text index cannot parse | Semantic results and an `index/invalid-query` warning; `index/invalid-query` if semantic search cannot answer either | `text`: `index/invalid-query`. `semantic`: answered |
| Both indexes off | `index/disabled` | Whichever index the mode needs: `index/disabled` |
| Text index off and the semantic provider failing | `index/disabled`, one generic error whose message names the cause and whose remediation points at the vault health check and activity log | `semantic`: the provider's own stable error |

The generic error in the last blended row is deliberate: a non-technical operator sees one consistent outcome ("search is unavailable"), and the underlying provider or configuration problem is found through the health check and the activity log rather than a provider-specific code.

## The link graph

The link graph reads every link form Obsidian does: wikilinks and their embeds (`[[note]]`, `![[image.png]]`) and inline Markdown links and images, with or without a title, including angle-bracketed paths, URL encoding, reference-style links with a label definition anywhere in the note, and a link to a folder (a path ending in `/`), which reaches the `index.md` inside it. Links in frontmatter count. External URLs, autolinks, raw HTML, links to a heading or block of the same note, and anything inside a code block, a code span, or a `%%comment%%` do not. A managed `index.md` contributes no links.

Every link is resolved by the one rule link expansion uses ([`link-expansion.md`](link-expansion.md)): from the linking note's folder up through each ancestor to the vault root, nearest first, then by file name across the whole vault, against every file in it, whether or not the search indexes it. A link to an image, PDF, canvas, base, or a note in an excluded folder therefore reaches that file. Such a file appears in query results as a node of kind `file`, only while a link reaches it, and is never reported as an orphan. A link that names no file reaches a `not-found` node keyed by the target as written. A name several files answer to, none nearer than the others, reaches an `ambiguous` node that keeps the candidates.

Links are resolved again when files change. A file appearing, going, or moving can make a missing target resolve, a nearer match take over, a name turn ambiguous, or a working link break, and the graph updates the links it affects without a rebuild. A note whose links cannot be read (for example a wikilink with no closing brackets) stays in the graph with the reason recorded and no outgoing links; it is not reported as an orphan, because what it links to is not known.

A wikilink names a note, never an address, so `[[Re: meeting]]` is a link to a note called that, and reaches a `not-found` node when there is none. Only a Markdown link whose destination has a URL scheme (`https:`, `mailto:`) is external. A name several files answer to is settled by the base a note declares in its `base` property, as link expansion settles it: if exactly one of the candidates sits inside the folder that base keeps its records in, the link reaches it. A note with no declared base, or one whose base names no single folder, leaves the name ambiguous.

The graph lists the vault's files when its store opens, before any change is applied, and keeps the listing in step with every change it is told of. Each file added or removed is recorded in the store's change history, even when no link changed, so several `contextos` processes sharing one `sqlite` store learn of each other's files; a process that reloads the whole graph lists the vault again, because the store holds no listing. The single-process `fjall` and `serde` backends have nothing to share. Files hidden by the vault's `hidden` patterns, the same patterns the listing tools honour, are no part of the graph: a link to one is not found. The `.git` and `.contextos` folders are always hidden.

Files created, changed, or deleted outside `contextos` (in Obsidian, by a sync client, or from a shell) are brought in by reconciling, which the graph questions (`neighbours`, `backlinks`, `path`, `orphans`, `unresolved`, `unlinked`) and `doctor`'s `link_integrity` check do before they answer. Reconciling lists the vault, adds the files the graph does not hold, removes those that are gone, and re-reads only the notes whose modification time or length differs from the stamp recorded when the graph last read them. It costs one walk of the vault's directories and one metadata read per note: about 35 ms on a vault of 12,000 files, against about 480 ms for a rebuild that reads every note. A rebuild remains the way to repair a graph that is damaged or has never been built.

## Link integrity

Two `query_graph` operations report on the graph as a whole, and `doctor` summarises the first.

`unresolved` lists every link that does not resolve, so an author or an unattended librarian can mend them. Each entry has `source` (the note), `line` (1-based), `text` (the link exactly as written), `target` (as resolved: heading and block reference removed, angle brackets and URL encoding undone), `form`, `kind`, and, for an ambiguous link, `candidates`. `kind` is `not-found`, `ambiguous`, or `unreadable`; an unreadable entry is a note whose links cannot be parsed, listed once with the reason in `text` and no `line`, `target`, or `form`, so that a missing entry always means a sound link. Entries are sorted by `source` then `line` and paged with `offset` and `limit` (default 200, at most 1000), and `total` is the length of the whole list. Template placeholders (`{{...}}` anywhere in a wikilink target, or a wikilink whose whole target is in angle brackets) are passed over, and so is everything the scanner does not read as a link: code, code spans, and `%%comments%%`. Angle brackets around a Markdown link's destination are syntax, not a placeholder. The line and the text come from the edges the graph stores, so the notes are not read again to answer.

`unlinked` lists every note that no other note links to, whatever it links out to, sorted by path and paged the same way. A link from the note to itself does not count. `index.md` files are never reported, and the managed index of a folder contributes no links, so a note listed only in its folder's `index.md` is unlinked. `orphans` keeps its narrower meaning: a note with no links in or out.

Both take an optional `path` limiting the result to a folder of the vault (validated like every other `path` parameter, so `memory/wiki` and `{vault}://memory/wiki` both work, and a traversal, symlink escape, or Windows path is refused with its `path/*` code; it must name an existing folder of the vault being queried, and a folder that does not exist is refused with `path/not-found`, a file with `path/not-a-directory`, and a folder of another vault with `index/invalid-query`, since an empty answer for a mistyped folder would read as a clean vault), and both reject `from`, `to`, `depth`, and `direction`, which belong to the traversal operations, with `index/invalid-query`; so does a `limit` above 1000 or a negative `offset`, and so does `path`, `offset`, or `limit` given with `neighbours`, `backlinks`, `path`, or `orphans`.

Notes in the folders `[vault.links] exclude` names ([`configuration.md`](configuration.md#vaultlinks)) are not checked as link sources, and a link from one does not count towards `unlinked`; links into them still resolve. Excluded sources are skipped after resolution, so excluding a folder never turns a working link into a broken one.

`doctor`'s `link_integrity` check asks the same question of the same graph. It passes with the number of links checked, or fails with the numbers of not-found links, ambiguous links, unreadable notes, and notes affected, listing the first ten as `source:line -> target (kind)` and counting the rest. It is never auto-fixable, and `doctor_resolve` leaves it alone. A vault with its link graph off, or whose search state cannot be opened, passes with a message saying it was not checked. Called through the server (`doctor`, `doctor_resolve`) it uses the server's own graph; `contextos doctor` opens the vault's graph for the assessment, which brings the derived graph store up to date, and `contextos doctor --resolve --dry-run` opens nothing and reports the check as not made.

## Hit metadata

Each hit carries the note's `title` and a `metadata` object with the frontmatter fields `description`, `summary`, `excerpt`, `status`, `updated` (exactly as written), and `note_type` (the frontmatter `type`). A field the note does not declare, or declares as something other than a non-blank string, is omitted, never `null`. Hits also report `modified`, the file's modification time, which is what `sort=modified` orders by.

Metadata is read from the source files of the results returned, not stored in the index, so it is always current and no index rebuild is needed to change what it says. Only the first 64 KiB of a note is read: a frontmatter block that does not close within that is treated as absent. A note that has moved or vanished since it was indexed, or that resolves outside the vault, simply yields empty metadata and never an error.

## Index health and recovery

Every `contextos` instance on a host shares a vault's derived stores, kept in its state directory: the text index (`index/`), the link graph store (`graph/`), and the vector store (`vectors.db`). `query_index_status`, and `vault_info`'s `indexes`, report each store's `health`:

- `ready`: the store opens and reads.
- `empty` and `incompatible` (link graph only): the graph has not been rebuilt from the vault, or its store was written by another version; `query_index_rebuild` with `index: graph` fixes both.
- `unreadable`: the store cannot be read, and `failure` gives the stable `code`, `message`, and `remediation`. Queries against that store are refused with the same code, the other stores keep answering, and a vault write still succeeds, reporting the failure as a warning.
- `disabled`: the store is not enabled for the vault.

A store that cannot be read never fails the status call, and never stops the server from starting: it is reported, and every later use tries to open it again, except a damaged one, which stays closed until it is recovered here or another instance has recovered it. A vault or state directory that cannot be scanned for staleness is reported in the top-level `failure`, and every store still reports its own health.

A damaged store (`index/corrupt`) cannot be rebuilt in place, so it is moved aside and recreated. The damaged files are moved into their own directory under `quarantine/` in the state directory (for example `quarantine/graph-20261019T024419Z/`) and kept for diagnosis; deleting them is the operator's decision. The fresh store is repopulated from the vault: the link graph is rebuilt at once, the vault is queued to be embedded again, and the text index is refilled by its next reconciliation. This happens:

- when an instance opens the vault and finds a store damaged;
- when a query needs a store found damaged (a link graph query can therefore run a graph rebuild before it answers);
- when `query_index_rebuild` needs a store found damaged, or finds it damaged while rebuilding; and
- when `doctor_resolve` acts on a store check the doctor reported damaged, after giving up the vault's write lock, since repairing a store writes no vault content.

A vault write that finds a store damaged only reports it as a warning and releases the store, so recovery never holds up a write.

A store is moved aside only while nothing has it open: no other `contextos` process, no call still running in this one, and no doctor inspection. Every open handle on a store carries a registration as one of its holders (a locked file under `holders/` in the state directory), dropped when the last call using the handle finishes. An instance releases a store as soon as it finds it damaged, so the last to release it can move it; checking for holders, moving the files, and registering for the fresh store happen under one lock, so two instances recovering at once move the store once and the other opens the replacement. While anything holds it, recovery is refused with `index/in-use`, the store stays where it is, and the status reports it unreadable. A move that fails part-way puts back what it moved, so a store is never left half in place. Instances of a build older than this one do not register as holders, so upgrade every instance together. `last_recovery` on each store's status, and `recovered` on a rebuild result, say when a store was moved aside and where.

A rebuild never puts back an entry older than a write made while it ran: each store re-checks, under its own write lock, what the rebuild read, so a note written meanwhile is read again, a note created meanwhile is included, and a note deleted meanwhile is left out. The link graph re-reads only the notes its store records as written since the rebuild began, which keeps its write lock short; a note changed outside `contextos` while a rebuild runs is caught by the next rebuild or write of that note. The embedding worker does the same for each note it embeds, embedding it again if it changed while its chunks were being embedded.

The doctor checks each enabled store (`text_index_store`, `link_graph_store`, and `vector_store`) without changing it: whether it has been built, whether `SQLite` or tantivy finds it intact, and how many other `contextos` processes hold it. A damaged store is auto-fixable only while no other process holds it.

## Errors

| Code | Cause |
| --- | --- |
| `index/busy` | Another `contextos` process held the store for longer than the call waits (5 seconds); a write reports it as a warning |
| `index/corrupt` | The store's files are damaged; see [Index health and recovery](#index-health-and-recovery) |
| `index/undecodable` | The store holds a record this build cannot decode; rebuild that index |
| `index/in-use` | A damaged store cannot be moved aside because another `contextos` process still has it open |
| `index/storage` | The store could not be read from or written to disk |
| `index/invalid-query` | Unparsable full-text query syntax (`query_text`, and `query_blended` when semantic search cannot answer instead), a page outside the paging rule, or `min_relevance` outside 0 to 1 or not a number |
| `index/disabled` | The index the call needs is not enabled for the vault |
| `embedding/*`, `vector/*` | The semantic provider or vector store failed (`semantic` mode and `query_semantic`; `blended` mode degrades to text with a warning) |

Unknown parameters, an unknown `sort`, `order`, or `mode` value, and values of the wrong type (for example a string for `min_relevance`, `limit`, or `offset`) are rejected as malformed input before the search runs, as for every tool.

## Measuring latency

Query latency is part of the non-functional requirements (text queries under 200 ms at the 95th percentile on a 10,000-note vault). The measurement is a manual acceptance script rather than a test, because it depends on wall-clock time:

```sh
cargo test -p contextos-search --release --test manual_query_latency -- --ignored --nocapture
```

It builds a deterministic 10,000-note vault and reports p50 and p95 for each query shape through the same entry points the tools use, including the per-query index refresh.
