# Bundled fonts

The renderer ships two subsetted variable fonts so macOS and Windows render the
same UI. Both are licensed under the SIL Open Font License 1.1; the full texts
are next to this file.

| File | Family | Upstream | Version | License |
| --- | --- | --- | --- | --- |
| `NotoSansSC-Variable.woff2` | `Noto Sans SC` | [google/fonts `ofl/notosanssc`](https://github.com/google/fonts/tree/2e61f4355afd22b801791b0df176065082423b87/ofl/notosanssc) | 2.004 | `OFL-NotoSansSC.txt` |
| `DMSans-Variable.woff2` | `DM Sans` | [google/fonts `ofl/dmsans`](https://github.com/google/fonts/tree/2e61f4355afd22b801791b0df176065082423b87/ofl/dmsans) | 4.004 | `OFL-DMSans.txt` |

Both files are variable fonts with a `wght` axis, so every `font-weight` used
by the renderer resolves without shipping separate static files. DM Sans also
keeps its `opsz` axis.

## Subset

The subset covers every character in `i18n/messages.json`, printable ASCII,
common punctuation, and GB2312 level-1 (3755 hanzi). Characters outside the
subset (for example unusual monitor names) fall back to the platform sans-serif
listed in `app.css`.

Regenerate after changing UI copy:

```sh
python3 desktop/scripts/build-font-subsets.py
```

The script downloads the pinned upstream files into `desktop/.font-cache/`,
verifies their SHA-256 digests, writes the `.woff2` files, and refreshes
`desktop/scripts/font-subset.json`. `npm test` fails when `messages.json`
contains a character missing from that manifest.
