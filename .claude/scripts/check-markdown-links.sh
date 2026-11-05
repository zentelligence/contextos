#!/usr/bin/env bash
# Validate local inline Markdown links in the supplied files.
set -euo pipefail

for markdown_file in "$@"; do
  while IFS= read -r markdown_link; do
    link_target="${markdown_link#*](}"
    link_target="${link_target%)}"
    link_target="${link_target%%#*}"

    case "${link_target}" in
      ''|'#'*|http:*|https:*|mailto:*) continue ;;
    esac

    link_target="${link_target#<}"
    link_target="${link_target%>}"
    link_source_dir="$(dirname -- "${markdown_file}")"
    if [[ ! -e "${link_source_dir}/${link_target}" ]]; then
      printf 'broken local Markdown link: %s -> %s\n' \
        "${markdown_file}" "${link_target}" >&2
      exit 1
    fi
  done < <(awk '
    # cargo-about emits verbatim licence text in fenced blocks. A fence
    # may be indented up to three spaces, and closes only with the same
    # character, at least the opening length, and no trailing info string.
    {
      line = $0
      match(line, /^ */)
      indent = RLENGTH
      candidate = substr(line, indent + 1)
      marker = substr(candidate, 1, 1)
      count = 0
      if (indent <= 3 && (marker == "`" || marker == "~")) {
        while (substr(candidate, count + 1, 1) == marker) count++
      }
      rest = substr(candidate, count + 1)
      if (fence != "") {
        if (marker == fence && count >= width && rest ~ /^[[:space:]]*$/) {
          fence = ""
        }
        next
      }
      if (count >= 3 && (marker == "~" || rest !~ /`/)) {
        fence = marker
        width = count
        next
      }
      print line
    }
  ' "${markdown_file}" | rg --no-filename --only-matching \
    '\[[^]]+\]\([^)]*\)' || true)
done
