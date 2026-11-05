# Relevance smoke fixtures

This vault and `queries.json` support the relevance smoke harness: 20 known-answer queries, each expected to return its target file in the top three ranked results.

## Provenance

Everything under `mock/` except this file and `queries.json` is a small, ContextOS-shaped demonstration vault, self-authored for this gate: two fictional projects (`aurora`, a solar telemetry platform, and `beacon`, coastal beacon firmware), their people, journal entries, meetings, and shared reference notes. Vocabulary deliberately overlaps between notes — for example, several notes mention an "ingestion worker" or repeat a phrase from another note in passing — so that ranking the correct target above its distractors is a real test of the engine, not an artefact of a vault with only one plausible answer per query. No content is copied from an operator vault.

## `queries.json` schema

A JSON array of exactly 20 entries, each shaped as:

```json
{
  "query": "telemetry dashboard",
  "target": "projects/aurora/overview.md",
  "note": "why this target is the known answer"
}
```

- `query` — the literal text passed to `IndexesText::query`. Most entries are plain terms or multi-word queries (parsed as an OR of terms, ranked by score); one is a quoted phrase (`"incident response"`); one is a tantivy field-qualified query (`title:Glossary`).
- `target` — the note's path relative to `mock/`, using forward slashes.
- `note` — a short explanation of why that target is the known answer, including which other notes are the deliberate distractors. This field is documentation for humans; the test harness does not read it.

## Running the in-repo smoke test

```sh
cargo test -p contextos-search --test relevance
```

`relevance_smoke_set_ranks_targets_top_three` copies `mock/` (minus this file and `queries.json`) into a temporary directory (the checked-in fixture is never indexed in place), indexes every `.md` file with a fixed modified timestamp, runs all 20 queries with `limit: 20` and no path, tag, or field filters, and asserts each target appears in the first three hits. It prints one summary line, for example `20/20 targets in top-3`, and on any regression names the failing query, the expected target, and the actual top-3 paths.

## Running the same harness against a vault copy

The acceptance step for this gate item is the same 20 queries against a copy of the operator's vault, not the fixture vault. The harness for that step is `acceptance_relevance_against_operator_vault` in the same test file. It is marked `#[ignore]` and is a no-op unless both environment variables below are set, so it never touches the operator's home directory or the network by default:

```sh
CONTEXTOS_RELEVANCE_VAULT=/path/to/vault-copy \
CONTEXTOS_RELEVANCE_QUERIES=/path/to/queries.json \
cargo test -p contextos-search --test relevance -- --ignored
```

- `CONTEXTOS_RELEVANCE_VAULT` — an absolute path to a disposable copy of the vault to index (never the operator's live vault; the harness indexes in place and does not write back to it, but a copy keeps the run reproducible). Directories named `.contextos`, `.git`, and `.obsidian` are skipped.
- `CONTEXTOS_RELEVANCE_QUERIES` — an absolute path to a `queries.json` file in the schema above, written by the operator against real vault content and known answers.

If either variable is unset, the test returns `Ok(())` immediately without indexing anything or printing a summary line. Both variables must be set, and the test must be selected with `--ignored`, for the acceptance run to do any work.
