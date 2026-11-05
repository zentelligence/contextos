#!/usr/bin/env bash
# Checks that the ContextOS plugin's `mcp-tools` skill still matches this server's tool catalogue, that the fence
# vocabulary the plugin's vault scaffold ships is the one the renderer carries as its default, and that the event kind
# definitions the scaffold ships pass the checks this server applies to a vault's definitions.
#
# The plugin is a separate repository. Point CONTEXTOS_PLUGIN_DIR at its checkout, or keep it beside this repository as
# `context-os-plugin`. With no checkout the check is reported as skipped, never as passed. A checkout that lacks the
# skill fails, since deleting the skill is drift too.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/../.." && pwd)"
plugin_dir="${CONTEXTOS_PLUGIN_DIR:-${repo_root}/../marketplace/plugins/context-os}"

if [[ ! -d "${plugin_dir}" ]]; then
  # A checkout the operator named explicitly must exist, so a mistyped path cannot quietly disable the check. Only the
  # default sibling location may be absent.
  if [[ -n "${CONTEXTOS_PLUGIN_DIR:-}" ]]; then
    printf 'CONTEXTOS_PLUGIN_DIR is set to %s, which is not a directory.\n' "${plugin_dir}" >&2
    exit 1
  fi
  printf 'SKIPPED plugin skill check: no plugin checkout at %s (set CONTEXTOS_PLUGIN_DIR to the context-os-plugin checkout).\n' \
    "${plugin_dir}" >&2
  exit 0
fi

# Resolve now: the script changes directory below, and a relative CONTEXTOS_PLUGIN_DIR is relative to the caller.
plugin_dir="$(cd -- "${plugin_dir}" && pwd)"

skill_dir="${plugin_dir}/skills/mcp-tools"
if [[ ! -f "${skill_dir}/SKILL.md" ]]; then
  printf 'plugin checkout %s has no skills/mcp-tools/SKILL.md; the skill must exist and cover every tool.\n' \
    "${plugin_dir}" >&2
  exit 1
fi

cd "${repo_root}"
CONTEXTOS_PLUGIN_SKILL_DIR="$(cd -- "${skill_dir}" && pwd)" \
  cargo test -p contextos-mcp --test plugin_skill_contract -- --ignored

# A new vault's `.contextos/fences.toml` and the vocabulary a vault without one falls back to must be the same file, or
# the same note renders differently depending on whether its vault was scaffolded.
shipped_fences="${plugin_dir}/workflows/vault/templates/shipped/.contextos/fences.toml"
if [[ ! -f "${shipped_fences}" ]]; then
  printf 'plugin checkout %s has no workflows/vault/templates/shipped/.contextos/fences.toml; a new vault must ship the fence vocabulary.\n' \
    "${plugin_dir}" >&2
  exit 1
fi
if ! cmp -s "${shipped_fences}" crates/contextos-markdown/default-fences.toml; then
  printf '%s differs from crates/contextos-markdown/default-fences.toml; copy one over the other so they match.\n' \
    "${shipped_fences}" >&2
  exit 1
fi

# The event kinds a new vault ships must be ones the server accepts, with the fields the requirement names.
shipped_kinds="${plugin_dir}/workflows/vault/templates/shipped/registry/events"
if [[ ! -d "${shipped_kinds}" ]]; then
  printf 'plugin checkout %s has no workflows/vault/templates/shipped/registry/events; a new vault must ship its event kinds.\n' \
    "${plugin_dir}" >&2
  exit 1
fi
CONTEXTOS_PLUGIN_EVENT_KINDS_DIR="$(cd -- "${shipped_kinds}" && pwd)" \
  cargo test -p contextos-events --test plugin_event_kinds -- --ignored
