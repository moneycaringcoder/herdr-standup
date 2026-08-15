# Diagram sources

Sources live here, rendered output in `docs/img/`. Both are committed, so a
reader never needs a toolchain to see the picture and a maintainer never has to
reverse-engineer an SVG to change one.

Regenerate after editing a source:

```sh
d2 --theme 0 --dark-theme 200 --pad 24 docs/diagrams/pipeline.d2 docs/img/pipeline.svg
```

The `--dark-theme` flag matters: it emits one SVG carrying both palettes behind
a `prefers-color-scheme` query, so a single file reads correctly on GitHub's
light and dark themes.

`docs/img/logo.svg` is hand-authored rather than generated. Its colours are
mid-tone on purpose so it needs no dark variant.

d2's bundled font has no glyphs for box-drawing or symbol characters, and
renders them as tofu. Keep labels to plain ASCII.
