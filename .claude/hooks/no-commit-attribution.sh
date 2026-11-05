#!/usr/bin/env bash
# PreToolUse hook: block a git commit or gh pr create whose message carries attribution text.
# Enforces AGENTS.md/CLAUDE.md: never add attribution to commits or pull requests.
set -uo pipefail

payload="$(cat)"
command="$(jq -r '.tool_input.command // empty' <<<"${payload}")"

[[ -n "${command}" ]] || exit 0

grep -qE 'git[[:space:]]+commit|gh[[:space:]]+pr[[:space:]]+create' <<<"${command}" || exit 0

if grep -qiE 'co-authored-by|generated with \[claude|claude\.com/claude-code|noreply@anthropic\.com|sonnet|opus|fable|openai' <<<"${command}"; then
  printf 'Blocked: this command adds attribution to a commit or PR. AGENTS.md and CLAUDE.md both prohibit attribution in commits. Remove the Co-Authored-By / "Generated with" / claude.com / noreply@anthropic.com text and retry.\n' >&2
  exit 2
fi

exit 0
