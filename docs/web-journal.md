# The Journal screen

`/{vault}/journal/{date}` shows one day's entry in the vault's journal. The screen knows no section by name: a day's tabs, their headings, their order, and the guidance shown with them all come from the day's entry and the vault's daily-note template. How a vault structures its days is the operator's choice, made in the template.

## Where entries live

`.contextos/journal.toml` sets where entries are filed and which template a new day starts from. It is edited from the Journal pane of the vault settings (`/{vault}/settings/#journal`). Every key is optional.

```toml
journal_dir = "journal"                       # the folder entries are filed under
template_path = "templates/daily-journal.md"  # the daily-note template a new day starts from
date_format = "YYYY/MM/YYYY-MM-DD"            # a new entry's path within journal_dir, Templater date tokens
date_structure_side = "right"                 # which side the date tree is on: "left" or "right"
task_opened_section = "New actions"           # list a new task under this section of the day's entry
task_closed_section = "Completed actions"     # list a closed task under this section of the day's entry
read_only_sections = ["Agent log"]            # sections shown but never offered for editing
```

The three section settings name a section by its heading text, without the `#` marks. A name is matched as the heading it would write reads, trimmed and ignoring case: `Tasks *new*` matches the heading `## Tasks *new*`, whose text is `Tasks new`. A name that spans lines, holds a control character, opens with `#`, is longer than 200 characters, or makes a heading with no text is refused, and nothing is written. All three are unset by default: no task is listed in the journal and every section is editable until the operator says otherwise. A file that names an unknown key, or cannot be parsed, is not used; the screen falls back to the defaults.

## A day's tabs

- A day that has an entry has one tab per top-level `##` heading in it, in the order written. A `## ` line inside a code block, a `%%comment%%`, a block quote, or a list item does not start a section, and neither does anything in the frontmatter. Deeper headings (`###`), and any `#` heading after the first `##`, belong to the section they are in. Text between the frontmatter and the first `##` heading (the day's title, navigation links) belongs to no tab and is never changed.
- A line of text directly followed by a `---` line is a heading too (CommonMark's underlined form), so an entry written that way outside the app gains a tab there; a `---` rule set off by a blank line is only a rule.
- An entry with no `##` heading at all is one tab, Entry, covering everything after the frontmatter.
- An entry that exists but cannot be read as text (too large to attach, not text, or refused by the vault) is never shown or saved as a new day: the screen says why, offers no tabs, and saves to it are refused.
- A day with no entry yet shows the template, rendered for that day, exactly as its first save will write it. The template's Templater expressions are filled in for the day's date (the subset `tp.date.now`, `tp.date.yesterday`, and `tp.date.tomorrow` with their format tokens). A template that uses any other expression cannot be used for a new day, and the screen says so; an absent template gives a new day an empty entry. Viewing a day never creates its entry.

## Guidance

A template introduces each section with block quotes and `<!-- comments -->`, and closes it with a `---` rule. Where a day's section still opens with exactly that guidance, the guidance is shown as a hint above the section and kept out of the text being edited, and where it still closes with that rule, the rule is kept out too. Both stay in the file, byte for byte. A section the template has no heading for, or one whose guidance has been edited, is edited whole, guidance and rule included.

## Editing

Every section of every day, past, present, or future, is editable as the Markdown it holds, except the sections `read_only_sections` names. Edit opens a section's text; Save writes it; Cancel returns to the read view.

- A save replaces that section's text and nothing else: every other byte of the entry, including other sections, the frontmatter, the title, and the guidance and rule around the edited text, is kept exactly as it is in the file at the moment of writing. The saved text is set off from what is around it by blank lines, so it never runs on into the guidance above it, nor turns the rule below it into a heading; surrounding blank lines and Windows line endings in what was typed are not kept.
- The first save on a day with no entry writes the template, rendered for that day, with that section filled in.
- A save is checked against the version of that section the editor was opened on, never the whole file, so text another tool appended to a different section since does not refuse it. If that section itself changed, the save is refused: the editor keeps what was typed, shows the section's current content beside it, and a second save replaces the current content deliberately. If the day's sections were added, removed, or renamed so the section is no longer where the editor found it, the save is refused and the typed text kept, to copy before reloading.
- Text that holds a heading of its own (a `## ` line, or a line underlined with `---`) adds sections; after such a save the whole day is shown again so its tabs match.
- Text that would change how any other section reads is refused, the typed text kept: a code block, `%%comment%%`, or `<!-- comment -->` it opens without closing would swallow the sections after it.
- Saved text follows the entry's line endings: an entry written with Windows line endings keeps them.

| Request | Answer |
| --- | --- |
| `GET /{vault}/journal/{date}` | The day, first tab shown |
| `GET /{vault}/journal/{date}/sections/{index}` | The section's read view (HTMX), or the day with that tab shown |
| `GET /{vault}/journal/{date}/sections/{index}/edit` | The section's edit form (HTMX), or the day with that form open; `404` for a read-only section |
| `PATCH /{vault}/journal/{date}/sections/{index}` | Saves `{"heading", "base", "content"}`: the section's read view, the whole day when sections were added, the edit form with the reason when refused, or the read view with the reason for a read-only section; `422` for a body with any other field, a missing one, or a `base` that is not 64 lowercase hexadecimal digits |

`index` is the section's place among the day's sections, counting from 0, written as a plain decimal number. A `date` that is not a real calendar date, or an `index` that is not such a number or names no section, is `404`. When the vault cannot be reached, the request is answered with the unreachable page.

## Tasks in the journal

When `task_opened_section` is set, creating a task lists it in that day's entry under that section; when `task_closed_section` is set, closing a task (moving it into one of its type's closing states) lists it under that one. Each is listed as a bullet naming the task and linking it by its key. The bullet goes at the end of the section, above its closing rule, in place of any empty list items the section ends with (a template's `- [ ]` placeholder). A day with no entry yet is first written from the template; an entry without the section gains it at the end. A task whose listing would change how the rest of the entry reads (a title with a lone `%%` in an entry with `%%comments%%`, say) is not listed. Listing a task never fails the task change: a failure is logged as a warning.
