# Bundled fonts

The renderer ships two subsetted variable fonts so macOS and Windows render the
same UI. Both are licensed under the SIL Open Font License 1.1; the full texts
are next to this file.

| File | Family | Upstream | Version | License |
| --- | --- | --- | --- | --- |
| `NotoSerifSC-Variable.woff2` | `Noto Serif SC` | [google/fonts `ofl/notoserifsc`](https://github.com/google/fonts/tree/2e61f4355afd22b801791b0df176065082423b87/ofl/notoserifsc) | 2.003 | `OFL-NotoSerifSC.txt` |
| `KVMFlowSerif-Variable.woff2` | `KVMFlow Serif` | [adobe-fonts/source-serif `4.005R`](https://github.com/adobe-fonts/source-serif/releases/tag/4.005R) | 4.005 | `OFL-SourceSerif4.txt` |

`KVMFlow Serif` is a subset of Source Serif 4 Variable Roman. Source Serif
declares the Reserved Font Name "Source", so the subset uses a new primary
name; the design and version are unchanged.

Both files are variable fonts with a `wght` axis of 200-900, so every
`font-weight` used by the renderer resolves without shipping separate static
files. The Latin font also keeps the `opsz` axis.

## Subset

The subset covers every character in `i18n/messages.json`, printable ASCII,
common punctuation, and GB2312 level-1 (3755 hanzi). Characters outside the
subset (for example unusual monitor names) fall back to the platform serif
listed in `app.css`.

Regenerate after changing UI copy:

```sh
python3 desktop/scripts/build-font-subsets.py
```

The script downloads the pinned upstream files into `desktop/.font-cache/`,
verifies their SHA-256 digests, writes the `.woff2` files, and refreshes
`desktop/scripts/font-subset.json`. `npm test` fails when `messages.json`
contains a character missing from that manifest.
