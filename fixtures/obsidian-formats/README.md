# Obsidian format conformance fixtures

These fixtures hold representative Obsidian Bases, JSON Canvas 1.0, and Mermaid files that the Base and Canvas codecs and the Mermaid tools are tested against.

`bases/every-feature.base` covers recursive global and view filters, formulas,
property presentation, a custom summary, grouping, sorting, limits, standard
summaries, all four view types, quoting-sensitive expressions, and preserved
Maps-plugin settings.

`canvases/every-feature.canvas` covers all four node types, generic geometry,
file subpaths, group backgrounds, both colour forms, and every optional edge
attribute. `canvases/group-nesting.canvas` covers nested groups, and
`canvases/dangling-edge.canvas` is expected to produce exactly one
`canvas/dangling-edge` diagnostic at `edges[0].toNode`.

The fixture tests parse, validate, serialise, reparse, and compare ordered
definitions. They deliberately avoid normalising through Obsidian so that a
round trip cannot conceal information loss.
