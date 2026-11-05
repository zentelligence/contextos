# Link expansion (`fs_attach_file` with `expand_links`)

`fs_attach_file` returns a Markdown note, or a canvas, with every link rewritten in place to a vault-rooted path, so a client never resolves link targets itself. The resolution happens inside the server, against the vault, and the client (the web application, an agent) only maps the rooted paths to whatever it presents.

## Contract

- Input: `expand_links: true`, on a path ending `.md` or `.canvas`. Any other path is rejected with `format/not-markdown`; the flag is never ignored. `frontmatter_read` accepts `expand_links` on `.md` only.
- Output: the note as one embedded text resource, frontmatter included, with the same resource URI as a plain read. The text block after it is `{"links_expanded": true}`. It has no `content_hash` key. The text is not the file's content, so no hash of it could be a valid `expected_hash`, and the server already refuses to overwrite an existing file without one unless `force` is set.
- Never edit from expanded content. Read the plain file before changing it.
- The size cap applies to the file on disk, and to the expanded text too: expansion only lengthens a note, a link can be far shorter than the path it becomes, and so the result is checked as it grows and refused with `io/too-large` the moment it passes 10 MiB.

## The rewrite

Every link is rewritten in place and carries its own outcome in its target. There is no separate inventory.

| Written | Becomes |
| --- | --- |
| `[[Foo]]` | `[[/wiki/Foo.md\|Foo]]`, the original text kept as the display |
| `[[Foo\|shown]]` | `[[/wiki/Foo.md\|shown]]` |
| `[[Foo#Heading]]`, `[[Foo#^block]]` | the fragment follows the path, and the display is `Foo#Heading` |
| `![[Foo]]`, `![[pic.png\|300]]` | the target is rooted; an embed gains no display, because its `\|` part is a size hint |
| a standard link with destination `Foo.md#H` | destination `/wiki/Foo.md#H`, link text kept |
| a standard image with destination `pic.png` | the destination is rooted; alt text and title are kept |
| a reference definition `[id]: Foo.md "Title"` | the destination is rooted as an inline link's is, so `[text][id]` resolves through it; the label and title are kept |
| a link naming no file | `/:!` followed by the original target text, for example `[[/:!typo\|typo]]` |
| a link naming several files | `/:?` followed by the original target text |
| `https:`, `mailto:`, any `scheme:`, `//host`, `#anchor`, `{vault}://path` | left as written |

A standard link keeps exactly the text it was written with and gains only the folder prefix that makes it rooted, in the form the author used: a bare destination has the prefix percent-encoded where it needs it (whitespace, `%`, `#`, `<`, `>`, and parentheses), and an angle-bracketed one stays angle-bracketed. A destination that climbs with `..` is replaced by the path it names. Inside a table row a literal `|` is written `\|`, so an escaped pipe separates a wikilink's display there and is kept, and a wikilink that gains its display in a table row gets an escaped pipe. Links inside fenced and indented code blocks (a fence is closed only by the same character, at least as long), inline code, and `%%comments%%` are left alone. A link ending in `/` names a folder and resolves to the `index.md` inside it; a folder without one is not found. A file whose path contains `|`, `#`, `[`, or `]` cannot be written in a wikilink, and Obsidian does not allow such names, so a wikilink that would resolve to one is marked not found.

Expansion is idempotent. A link already rooted and resolving, or already marked, is left alone, so running it twice changes nothing.

## Canvases

A canvas is returned as JSON with the same top-level keys in the same order, pretty-printed, so it is a reading form like an expanded note and never the file's bytes. Every text node's `text` is expanded as a note's body is, from the canvas's own folder. Every file node's `file`, which Obsidian writes as a path from the vault root, is checked exactly and becomes `/` and that path when the file exists, or `/:!` and the original text when it does not; it is never looked up by name. A file node's `subpath` is kept. Text nodes and file nodes count together towards the limit on distinct targets. A canvas that is not JSON, or whose `nodes` is not an array of objects, is refused with `format/canvas-schema`.

## One resolver

Link expansion, `links_read`, the link graph, `query_graph` (`unresolved` and `unlinked`), and `doctor`'s `link_integrity` check all resolve a link by the same rule, against every file in the vault, so a link works or fails the same way wherever the question is asked. `links_read` reports what each outgoing link reached in `resolved`, and lists a link that finds no file, or several, under `unresolved` with its `form`, its `kind` (`not-found` or `ambiguous`), and the candidates of an ambiguous one. A note that declares a base settles a name several files answer to by the folder that base keeps its records in, in all of them.

## The markers

`/:!` and `/:?` cannot collide with a real vault path. `VaultPath::try_from` in `contextos-core` rejects any `:` in a path, as an alternate data stream, so no real folder or file can begin with a segment containing one. That rule is a ContextOS invariant, not an Obsidian or filesystem standard; a change to it would break this convention. The markers are internal. A consumer must recognise one on the raw link destination before any URL parsing, treat what follows as opaque (`?` would otherwise begin a query string), and present the link as unavailable or ambiguous with the original text as its display. A marker must never reach a reader, a URL, or a rendered page.

## Resolution

For a target written in a note, the first rule that applies wins:

1. A rooted target (`/a/b.md`) is checked exactly, and only that.
2. A `./` or `../` target is checked exactly, from the note's folder.
3. Any other target, including a multi-segment one such as `foo/bar.md`, is checked as its full path from the note's folder and then from each ancestor folder up to the vault root, nearest first. Each check is a single existence check of the regular file. A name with no extension is a note, so `.md` is tried first and the bare name after it; a name with another extension is tried as written and then as a note, which covers names that contain a dot.
4. A target found nowhere in that tree is looked up by file name in the vault's file name index, using its whole trailing path and ignoring case, as Obsidian does (the existence checks do not ignore case). The index holds every file of every type, so an image or a PDF is found the same way as a note. One match is found and none is not found. Several are ambiguous, unless the note declares its base in its `base` property and exactly one of them lies in that base's records folder (`file.inFolder` or `file.folder ==` in its top-level filters), which is then found: a record's links are written relative to its base. A `base` property that names no readable base settles nothing. The name is tried in the same order as in rule 3.

Repeated targets are answered once per expansion, and the work per distinct target is a few existence checks and one index lookup. The vault is not walked for a link that rules 1 to 3 answer.

## The file name index

- One index per vault, held in memory, mapping each lower-cased file name to the paths carrying it. Hidden paths, directories, and symlinks are not indexed.
- It is built by one walk of the vault, started in the background when the server starts and otherwise on the first lookup that needs it. It does not watch the vault.
- It is consulted first. An entry is confirmed with an existence check before it is used, so a file deleted since the walk is never linked.
- When a name is not found, the index is rebuilt by a fresh walk, once per expansion, and the name is looked up again. That is how a file created since the last walk, by the server or outside it, is found. A name that does not exist at all costs that one walk.
- A duplicate created since the last walk is not seen as ambiguity until the index is next rebuilt.
- A lookup never waits for a walk in progress: it is answered from the previous contents, and a rebuild replaces them whole when it finishes. Several expansions that each miss a name each walk the vault, in parallel.
- A directory the server may not read, or an entry removed during the walk, is left out of the index and does not fail it.
- The walk skips symlinks, so a name lookup never finds one. An existence check follows a symlink that stays inside the vault, so a link written as a path can resolve to one that a link written as a bare name cannot.
- The background walk at startup cannot be cancelled. Shutting down waits for the walks still running, one walk of each vault at most.

## Limits

- A note naming more than 10,000 distinct link targets is refused with `format/too-many-links` and returns no content, because a note only partly expanded would break the guarantee that every link is.
- A vault holding more than 500,000 files cannot build the index. Links resolved by rules 1 to 3 are unaffected; a name lookup fails with `io/too-many-files`.
- A failed lookup fails the expansion with a typed error. It is never read as "not found".
- An existence check is not restricted by the vault's hidden patterns, as a direct read is not, so an explicit path under a hidden folder resolves. The name lookup never lists hidden paths.
- A vault path that the path rules reject (traversal, a `:`, a symlink leaving the vault) is never a file, so a link to it is marked not found.
- Not rewritten: a reference definition whose destination is on the line after its label, the contents of HTML comments, links written inside the text of another link, and a link whose destination or title contains `[`.
- Recognised as code, so links inside are left alone: a fence inside a blockquote, a callout, or a list item (indented by a tab or spaces, and ending with the quote or item it sits in), an indented code block (four columns past the enclosing list item or quote, after a blank line, a heading, a closing fence, or at the start of the document), and an inline code span that runs over several lines of one paragraph (searched for its closing backticks over at most 50 lines). A backslash escapes a backtick outside a code span and is literal inside one. A document's frontmatter block is YAML: its indented lines are not code, and `%%` in it opens no comment.
- A wikilink names a note and never an address, so `[[Re: meeting]]` is looked up like any other name and marked not found when there is none. Only a Markdown destination with a URL scheme is left as written.
- The scanner is linear in the length of the note. A note of one very long line of unclosed openers costs no more than the same number of bytes of ordinary text.
