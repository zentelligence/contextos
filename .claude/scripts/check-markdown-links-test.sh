#!/usr/bin/env bash
# Exercise the actual link checker against isolated Markdown files.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
fixture_dir="$(mktemp -d)"
trap 'rm -rf -- "${fixture_dir}"' EXIT
mkdir -p "${fixture_dir}/notes"
touch "${fixture_dir}/notes/present.md"

assert_passes() {
  if ! bash "${script_dir}/check-markdown-links.sh" "${fixture_dir}/notes/input.md" \
    >"${fixture_dir}/output" 2>&1; then
    printf 'expected valid Markdown: %s\n' "$1" >&2
    cat "${fixture_dir}/output" >&2
    exit 1
  fi
}

assert_rejects_missing_link() {
  if bash "${script_dir}/check-markdown-links.sh" "${fixture_dir}/notes/input.md" \
    >"${fixture_dir}/output" 2>&1; then
    printf 'expected broken local link rejection: %s\n' "$1" >&2
    exit 1
  fi
  rg -q 'broken local Markdown link: .* -> missing.md$' "${fixture_dir}/output"
}

cat >"${fixture_dir}/notes/input.md" <<'MARKDOWN'
[Existing note](present.md#heading)
[External](https://example.com/absent)
MARKDOWN
assert_passes 'relative links resolve from their source directory'

cat >"${fixture_dir}/notes/input.md" <<'MARKDOWN'
[Broken](missing.md)
MARKDOWN
assert_rejects_missing_link 'ordinary prose still validates local targets'

cat >"${fixture_dir}/notes/input.md" <<'MARKDOWN'
# Third-party licence text
```
under the original license, see [COPYING](oniguruma/COPYING) for more information.
```
[Existing note](present.md)
MARKDOWN
assert_passes 'cargo-about licence text is literal, not a local link'

cat >"${fixture_dir}/notes/input.md" <<'MARKDOWN'
   ~~~~text
[Literal](missing.md)
```
~~~
[Still literal](missing.md)
   ~~~~~
[Existing note](present.md)
MARKDOWN
assert_passes 'tilde fences close only with the matching character and sufficient length'

cat >"${fixture_dir}/notes/input.md" <<'MARKDOWN'
````markdown
```
[Literal](missing.md)
```` not a closing fence
[Still literal](missing.md)
````
[Broken](missing.md)
MARKDOWN
assert_rejects_missing_link 'links after a valid closing fence are checked'

cat >"${fixture_dir}/notes/input.md" <<'MARKDOWN'
```text
[Literal through end of file](missing.md)
MARKDOWN
assert_passes 'an unclosed fence extends to the end of the file'

cat >"${fixture_dir}/notes/input.md" <<'MARKDOWN'
```invalid`info
[Broken](missing.md)
MARKDOWN
assert_rejects_missing_link 'a backtick in the info string prevents opening a backtick fence'

printf 'Markdown link checks passed.\n'
