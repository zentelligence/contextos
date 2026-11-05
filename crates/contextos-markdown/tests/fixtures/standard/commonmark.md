---
title: Standard Markdown
tags: [fixture]
---
# Standard Markdown

A paragraph with *emphasis*, **strong**, ~~struck~~, `code`, and "smart quotes" -- with a dash.

## Lists

1. First
2. Second
   - Nested
   - [x] Done task
   - [ ] Open task

## Table

| Left | Right |
| :--- | ----: |
| a    |     1 |
| b    |     2 |

## Code

```rust
fn main() { println!("<hi>"); }
```

    indented code

> A plain quotation.

## Links

An [external link](https://example.org "Example"), a [mail link](mailto:someone@example.org), an [anchor](#lists), and a [reference link][ref].

[ref]: https://example.org/ref

A footnote.[^note] Another.[^second]

[^note]: The note's text.
[^second]: The second note.

## HTML

<details>
<summary onclick="steal()">Expand</summary>

Hidden *Markdown*.

</details>

Inline <kbd>Ctrl</kbd> and <script>alert("x")</script> and <img src="javascript:alert(1)" alt="bad">.

---
