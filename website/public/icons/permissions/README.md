# Permission icons

One SVG per permission the landing page's permissions card can name, keyed by the
`icon` field of `src/lib/permission-descriptions.ts` and inlined at build time by
`src/lib/svg-icons.ts` (which drops the fixed size and sets `currentColor`).

Every file is a [Material Symbols](https://fonts.google.com/icons) glyph, Outlined
style, downloaded as-is from the Material Symbols site and named by its upstream
symbol name (`account_circle`, `bluetooth`, …). They ship with this repository so a
site build reads nothing from the network for them.

License: [Apache 2.0](https://www.apache.org/licenses/LICENSE-2.0), like every
Material Symbol. No attribution is required beyond this note.
