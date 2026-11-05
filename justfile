set shell := ["bash", "-euo", "pipefail", "-c"]

fmt:
    cargo fmt --all --check

clippy:
    cargo clippy --workspace --all-targets --features semantic-local -- -D warnings

test:
    cargo test --workspace --all-features --no-fail-fast

audit:
    cargo audit

# The note page's latency budget is the shipped product's, so it is measured in a release build; a debug run skips it.
performance:
    cargo test --release -p contextos-web --test performance

plugin-skill:
    bash .claude/scripts/check-plugin-skill.sh

# Regenerate THIRD_PARTY_LICENSES.md after any dependency change. Some dependencies' licence texts
# carry CRLF line endings; they are stripped so the file matches the LF that `.gitattributes` commits.
licences:
    raw="$(mktemp)"; trap 'rm -f "${raw}"' EXIT; \
    cargo about generate --locked --fail about.hbs -o "${raw}"; \
    tr -d '\r' < "${raw}" > THIRD_PARTY_LICENSES.md

# Fail if a dependency is under an unaccepted licence or THIRD_PARTY_LICENSES.md has drifted.
licences-check:
    raw="$(mktemp)"; generated="$(mktemp)"; trap 'rm -f "${raw}" "${generated}"' EXIT; \
    cargo about generate --locked --fail about.hbs -o "${raw}"; \
    tr -d '\r' < "${raw}" > "${generated}"; \
    diff -u THIRD_PARTY_LICENSES.md "${generated}" \
      || { echo 'THIRD_PARTY_LICENSES.md is stale; run `just licences`' >&2; exit 1; }

ci: fmt clippy test performance audit licences-check plugin-skill
    shellcheck .claude/scripts/*.sh .claude/hooks/*.sh
    bash .claude/scripts/check-markdown-links-test.sh
