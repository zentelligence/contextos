#!/usr/bin/env bash
# PostToolUse hook: flag requirement/decision-id and phase references left in edited file content.
# AGENTS.md forbids comments or code naming an identifier that exists only in a private planning
# document; change briefs and requirement matrices that legitimately use these IDs are maintained
# outside this repository, so any hit inside a tracked file is worth a second look.
set -uo pipefail

payload="$(cat)"
tool_name="$(jq -r '.tool_name // empty' <<<"${payload}")"

# Scope to the local Edit/Write tools only. MCP tools (contextos fs_edit_file,
# fs_write_file, and similar) operate on vault content outside this repository,
# where requirement and phase identifiers are the authoritative, expected form.
case "${tool_name}" in
  Edit | Write) ;;
  *) exit 0 ;;
esac

file_path="$(jq -r '.tool_input.file_path // empty' <<<"${payload}")"

[[ -n "${file_path}" && -f "${file_path}" ]] || exit 0

pattern='\b(R|FR|NFR|A|D)-[0-9]+\b|[Pp]hase[[:space:]]+[0-9]+'

hits="$(grep -nE "${pattern}" "${file_path}" 2>/dev/null || true)"

[[ -n "${hits}" ]] || exit 0

printf 'Possible tracking-id/phase reference left in %s (AGENTS.md: comments/code must not name a requirement, decision, or phase identifier that exists only in a private planning document):\n%s\nRemove or rephrase unless this is a genuine exception.\n' \
  "${file_path}" "${hits}" >&2
exit 2
