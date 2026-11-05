#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "${script_dir}/../.." && pwd)"
cd "${repo_root}"

required_files=(
  AGENTS.md
  README.md
  .claude/README.md
  .claude/rules/00-index.md
  .claude/rules/architecture.md
  .claude/rules/mcp-contracts.md
  .claude/rules/rust-quality.md
  .claude/rules/security.md
  .claude/rules/testing.md
  .claude/workflows/change-loop.md
  .claude/workflows/quality-gate.md
  .claude/workflows/tdd.md
  .claude/reviewers/code-review.md
  .claude/reviewers/specification-review.md
  .claude/templates/change-brief.md
  .claude/templates/requirement-test-matrix.md
)

for required_file in "${required_files[@]}"; do
  if [[ ! -s "${required_file}" ]]; then
    printf 'missing or empty required file: %s\n' "${required_file}" >&2
    exit 1
  fi
done

if ! command -v rg >/dev/null 2>&1; then
  printf 'ripgrep (rg) is required to validate repository Markdown files.\n' >&2
  exit 1
fi

if rg --line-number '[[:blank:]]+$' AGENTS.md README.md .claude --glob '*.md'; then
  printf 'trailing whitespace found in Markdown files\n' >&2
  exit 1
fi

# A crate's test fixtures are vault notes, whose links name vault paths rather than files in this repository.
mapfile -t markdown_files < <(rg --files -g '*.md' -g '!crates/*/tests/fixtures/**' . | sort)
bash "${script_dir}/check-markdown-links.sh" "${markdown_files[@]}"

if ! command -v shellcheck >/dev/null 2>&1; then
  printf 'shellcheck is required to validate repository shell scripts.\n' >&2
  exit 1
fi

shellcheck .claude/scripts/*.sh .claude/hooks/*.sh
bash "${script_dir}/check-markdown-links-test.sh"

if [[ ! -f Cargo.toml ]]; then
  printf 'Guidance checks passed. Cargo workspace not yet present; Rust checks skipped.\n'
  exit 0
fi

if command -v just >/dev/null 2>&1 && just --list 2>/dev/null | rg --quiet '^\s*ci\b'; then
  just ci
  exit 0
fi

cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
bash "${script_dir}/check-plugin-skill.sh"

if ! command -v cargo-audit >/dev/null 2>&1; then
  printf 'cargo-audit is required for the completion gate but is not installed.\n' >&2
  exit 1
fi

cargo audit
