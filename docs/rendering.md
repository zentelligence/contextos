# Rendering vault content in the web interface

`contextos-web` shows every vault note, and every Base, Canvas, and Mermaid diagram file, through one pipeline. The pipeline is split in two:

- `contextos-markdown` is pure. It parses a note into its parts, prepares it, and renders it to HTML. It performs no I/O and resolves no link.
- `contextos-web::note_render` is the only place a note is read for display. It reads the note, fetches what the preparation says the note needs, and hands the result to the pure crate.

The same input always renders the same HTML, which the golden suite (`crates/contextos-web/tests/rendering_golden.rs`) checks run to run and against checked-in files.

## A note's parts

A note (`Note`) is its frontmatter (`Frontmatter`, YAML between `---` lines at the very start) and its body (`Body`). A piece of a note that is all body, such as a journal section or a canvas text node, is read as a `Body` directly, so a section that happens to start with `---` is never mistaken for frontmatter. A template is rendered in template mode: its frontmatter is not parsed (a template's frontmatter is often not valid YAML until it is filled in), and every Templater token (`<% ... %>`, `{{ ... }}`) outside code is shown as written, as `<code class="template-token">`.

## Reading and preparing

1. The note is read once with every link already expanded by the server (`fs_attach_file` with `expand_links`, see [link expansion](link-expansion.md)), so rendering asks the vault nothing per link. A note that refers to nothing in the vault (no vault link or embed in its body, no wikilink in its frontmatter) renders the same without expansion, and the screen's own read of it is used.
2. `PreparedNote` parses the body once and lists what rendering will need (`Needs`): the notes it embeds, its Mermaid diagrams, its embedded base views, and the vault paths its links name.
3. `note_render` fetches all of those together: embedded notes (each read with its links expanded and narrowed to the heading or block the embed names), diagrams (`mermaid_render`), base views (`base_query`), the route for every linked path, and, only when the body has a `:::` line, the vault's fence vocabulary.
4. `RenderInput` (the prepared note, those `Resources`, and a `LinkRouter`) renders to `RenderedNote`: the HTML and the note's metadata (headings, tags, block ids, and links), which the outline and link panels read without parsing the note again.

Several notes or pieces shown together (a journal day's sections, a canvas's text nodes) go through `render_texts`, which fetches what any of them needs once for all of them.

## The dialect

The dialect is layered. Each layer is recognised by the pure crate; nothing is left to the browser.

| Layer | What it adds |
| --- | --- |
| CommonMark | The base grammar (`pulldown-cmark`). GitHub's extensions that Obsidian also renders are on: tables, strikethrough, task lists, footnotes. Smart punctuation is on. |
| Obsidian Flavoured Markdown | Wikilinks and embeds (`[[...]]`, `![[...]]`), callouts (`> [!type]`, with `+` or `-` to fold), highlights (`==text==`), tags (`#tag`), block ids (`^id`, which become the anchor `id="^id"`), comments (`%%...%%`, removed), and math (`$...$`, `$$...$$`). |
| Math | Typeset on the server as MathML Core (`math-core`), which browsers draw with no script. An expression that cannot be typeset is shown as its source, flagged. |
| Diagrams | A `mermaid` code block is rendered on the server (`mermaid_render`) to SVG. A diagram with problems shows a panel naming them, not a broken page. |
| Fences | Triple-colon containers (`:::name` ... `:::`), recognised from the vault's fence vocabulary (below). |
| Author HTML | Passed through an allowlist (below). |

Obsidian's inline fields (`key:: value`) are not part of the dialect; they render as the text they are.

Headings get anchors from their text, made unique within the page. Footnote anchors carry a scope per rendered note (`fn-{scope}-{n}`), so a page that shows several notes never has two footnotes with the same id.

## Fences and the fence vocabulary

Fences are found line by line, outside code, before any Markdown is parsed: a `:::name` line with no blank line around it would otherwise become paragraph text whose extent cannot be recovered. Fences nest: a closing line closes the innermost open fence when it has at least as many colons as that fence's opening line, so an outer fence is written with more colons than the fences inside it.

What a fence name means comes from the vault's fence vocabulary, `.contextos/fences.toml`, which the vault scaffold ships. Each fence has a role (`callout`, a titled container; `layout`; `status`, a labelled banner; or `page`, a page break), a tone (`neutral`, `positive`, `caution`, `critical`, `emphasis`, or `analysis`, which the theme turns into colours), optional typed attributes, and optional aliases. A fence the vocabulary does not list renders plainly, flagged as unknown. The vocabulary this build carries is the same file (`crates/contextos-markdown/default-fences.toml`); a vault without a manifest uses it, and a manifest that cannot be used (unreadable, invalid, or of an unsupported `schema`) falls back to it, is logged, and is reported by Vault health and by `doctor`'s `fence_vocabulary` check.

## Author HTML

The interface runs on the operator's own machine and is reached only through localhost, and the operator can already read every file in every vault. The allowlist exists so that a note copied in from elsewhere cannot run script or submit anything from inside the interface's own origin. Nothing an author writes reaches the page as written: the HTML is tokenised and rebuilt from the tokens.

- Allowed: the elements Obsidian renders that cannot run script, load a page, or submit anything (text formatting, headings, lists, tables, `details`/`summary`, `img`, `a`, and similar), each with only its allowed attributes. Every element may carry `title`, `lang`, `dir`, `class`, and a `style` that only sets properties.
- `href` must be `http:`, `https:`, `mailto:`, or a place on the page (`#...`); `src` must be `http:`, `https:`, or an inline raster image (`data:image/png`, and similar). Values are checked after entity decoding.
- Removed with their content: `script`, `style`, `iframe`, `textarea`, and the other raw-text elements, and `svg`, `math`, `template`, `object`, `frameset`, `head`, and `select`.
- Anything else becomes escaped text, so a token the filter misreads shows as text and can never become markup.

Only HTML the author wrote is filtered. The markup the renderer generates (callouts, MathML, diagrams) is not.

Apps (`registry/apps`) are not notes and do not pass through this pipeline: an app is meant to carry its own HTML and scripts.

## Links

The renderer never resolves a link. Each destination in the expanded note is read as a `LinkTarget`: a vault path (with an optional heading or block `Fragment`), a target the server marked not found (`/:!`) or ambiguous (`/:?`), or an external address. The `LinkRouter` gives each vault path its URL, from the route table described in [links in the web interface](web-links.md). A link with no route, no target, or several targets is shown as unavailable, with its original text and a hint ("No screen shows this note yet.", or "More than one note answers to this name.").

## Base, Canvas, and diagram files

A `.base`, `.canvas`, or `.mermaid` file is shown on the screen that owns its folder, read-only, through `file_view`: a Base as its view (`base_query`), with its view tabs switching it in place; a Canvas as SVG from its own positions, its text nodes rendered as notes (expanded and rendered together); a diagram as SVG, or the panel naming its problems. These files are never edited from the interface, so their screens show no edit, rename, or metadata controls for them. Like every file page, theirs has a download link in its title bar, the page's own address with `?raw=1`, which answers with the file as it is stored (see [links](web-links.md#file-pages)).

## One URL module

Every URL the interface builds for a vault comes from `contextos-web::urls::VaultUrls`. A guard test (`crates/contextos-web/tests/one_route_module.rs`) fails when vault URL text is assembled anywhere else, in a source file or a template.

## Performance

A note costs one expanded read and one batched fetch of whatever it needs, however many links it has. `crates/contextos-web/tests/render_calls.rs` counts the MCP calls a page makes: a plain wiki page must cost no more than its recorded baseline, links add no calls, and each embed adds one.
