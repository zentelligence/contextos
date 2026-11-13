#!/usr/bin/env bash
# PreToolUse hook: block a commit or pull request whose message carries attribution text.
# Enforces AGENTS.md and CLAUDE.md: never add attribution to commits or pull requests.
#
# Covers:
# - Bash: any git invocation that reaches `commit`, whatever options precede it (-C <dir>, -c key=value,
#   --git-dir=...), and `gh pr create`. The whole command is checked, so a message given with -m or through a
#   heredoc is seen, and a message file named with -F/--file is read and checked too, found through any `cd` or
#   `git -C` in the command; a message file that cannot be found is refused.
# - Git commit tools that bypass Bash (mcp__git__git_commit, mcp__contextos__git_commit, or any tool whose name
#   ends in git_commit): their message is checked.
# A matching command that only mentions attribution words without committing is never blocked, and a payload that
# cannot be read is allowed.
set -uo pipefail

PATTERN='co-authored-by|generated with \[claude|claude\.com/claude-code|noreply@anthropic\.com|sonnet|opus|fable|openai'

block() {
  printf 'Blocked: this %s adds attribution to a commit or PR. AGENTS.md and CLAUDE.md both prohibit attribution in commits. Remove the Co-Authored-By / "Generated with" / claude.com / noreply@anthropic.com text and retry.\n' "$1" >&2
  exit 2
}

payload="$(cat)"
tool="$(jq -r '.tool_name // empty' <<<"${payload}" 2>/dev/null)"

if [[ "${tool}" == *git_commit ]]; then
  message="$(jq -r '.tool_input.message // .tool_input.msg // empty' <<<"${payload}" 2>/dev/null)"
  grep -qiE "${PATTERN}" <<<"${message}" && block "${tool} call"
  exit 0
fi

command="$(jq -r '.tool_input.command // empty' <<<"${payload}" 2>/dev/null)"
[[ -n "${command}" ]] || exit 0

# The command's shell structure: heredoc bodies dropped, and quoted text kept without its quotes, whitespace, or
# separators, so words inside a message or a script never read as a commit, a -F file, a cd, or a -C folder.
skeleton="$(awk '
  {
    if (heredoc != "") { line = $0; if (strip) sub(/^\t+/, "", line); if (line == heredoc) heredoc = ""; next }
    out = ""; n = length($0); i = 1
    while (i <= n) {
      c = substr($0, i, 1)
      if (sq) { if (c == "\047") sq = 0; else if (c !~ /[[:space:];&|]/) out = out c; i++; continue }
      if (dq) {
        if (c == "\\") { i += 2; continue }
        if (c == "\"") dq = 0; else if (c !~ /[[:space:];&|]/) out = out c
        i++; continue
      }
      if (c == "\047") { sq = 1; i++; continue }
      if (c == "\"") { dq = 1; i++; continue }
      if (substr($0, i, 2) == "<<" && substr($0, i, 3) != "<<<") {
        rest = substr($0, i + 2); strip = (substr(rest, 1, 1) == "-"); if (strip) rest = substr(rest, 2)
        sub(/^[[:space:]]*/, "", rest)
        if (match(rest, /^["\047]?[A-Za-z_][A-Za-z0-9_]*/)) {
          pending = substr(rest, RSTART, RLENGTH); gsub(/["\047]/, "", pending)
        }
        out = out " <<HEREDOC "; i += 2 + strip; continue
      }
      out = out c; i++
    }
    printf "%s%s", out, ((sq || dq) ? "" : " ; ")
    if (pending != "" && !sq && !dq) { heredoc = pending; pending = "" }
  }' <<<"${command}")"

# Any git invocation reaching `commit` before a command separator, or `gh pr create`.
grep -qE '(^|[^[:alnum:]_-])git([[:space:]]+[^;&|[:space:]]+)*[[:space:]]+commit([[:space:]]|$)|gh[[:space:]]+pr[[:space:]]+create' \
  <<<"${skeleton}" || exit 0

grep -qiE "${PATTERN}" <<<"${command}" && block "command"

unquote() { local v="${1#\"}"; v="${v%\"}"; v="${v#\'}"; printf '%s' "${v%\'}"; }

# A message file: check its contents. A relative path is tried against the session's working folder and every
# folder the command changes to with `cd` or `git -C`; a file that cannot be found there is refused, since a
# message that cannot be read cannot be checked.
cwd="$(jq -r '.cwd // empty' <<<"${payload}" 2>/dev/null)"
bases=("${cwd:-.}")
while IFS= read -r dir; do
  dir="$(unquote "${dir}")"
  [[ -z "${dir}" ]] && continue
  [[ "${dir}" == "~"* ]] && dir="${HOME}${dir#\~}"
  [[ "${dir}" != /* ]] && dir="${cwd:-.}/${dir}"
  bases+=("${dir}")
done < <(grep -oE '(^|[;&|[:space:]])cd[[:space:]]+[^[:space:];&|]+|-C[[:space:]]+[^[:space:];&|]+' <<<"${skeleton}" |
  sed -E 's/^[;&|[:space:]]?cd[[:space:]]+//; s/^-C[[:space:]]+//')

while IFS= read -r file; do
  file="$(unquote "${file}")"
  [[ -z "${file}" || "${file}" == "-" ]] && continue
  [[ "${file}" == "~"* ]] && file="${HOME}${file#\~}"
  found=""
  if [[ "${file}" == /* ]]; then
    [[ -f "${file}" ]] && found="${file}"
  else
    for base in "${bases[@]}"; do
      [[ -f "${base}/${file}" ]] && { found="${base}/${file}"; break; }
    done
  fi
  if [[ -z "${found}" ]]; then
    printf 'Blocked: the commit message file %s could not be found to check for attribution. Give it as an absolute path, or pass the message with -m or a heredoc.\n' "${file}" >&2
    exit 2
  fi
  grep -qiE "${PATTERN}" "${found}" && block "commit message file"
done < <(grep -oE '(-F|--file)(=|[[:space:]]+)[^[:space:];&|]+' <<<"${skeleton}" | sed -E 's/^(-F|--file)(=|[[:space:]]+)//')

exit 0
