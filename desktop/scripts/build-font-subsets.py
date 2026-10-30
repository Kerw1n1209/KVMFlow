#!/usr/bin/env python3
"""Build the bundled WOFF2 font subsets.

Sources are pinned to official releases and verified by SHA-256 before use.
The character set is the union of every character in messages.json, printable
ASCII, common punctuation, and GB2312 level-1 (3755 hanzi). Regenerate with:

    python3 desktop/scripts/build-font-subsets.py

Fonts are cached under desktop/.font-cache/ (gitignored).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import urllib.request
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont

DESKTOP = Path(__file__).resolve().parents[1]
MESSAGES = DESKTOP / "src/renderer/i18n/messages.json"
FONTS = DESKTOP / "src/renderer/assets/fonts"
CACHE = DESKTOP / ".font-cache"
MANIFEST = DESKTOP / "scripts/font-subset.json"

ASCII = "".join(chr(code) for code in range(0x20, 0x7F))
PUNCTUATION = (
    "·—–…“”‘’「」『』《》〈〉、。，！？；：（）【】〔〕～￥％＃＆＊＋－／＠｜＼"
    "＾＿｀×÷°′″€£¥§¶•◦▪→←↑↓●○◆◇■□▲▼"
)
LAYOUT_FEATURES = ["kern", "liga", "clig", "calt", "ccmp", "locl", "mark", "mkmk", "rlig"]

NOTO = {
    "file": "NotoSansSC-Variable.woff2",
    "family": "Noto Sans SC",
    "version": "2.004",
    "license": "OFL-NotoSansSC.txt",
    "url": (
        "https://raw.githubusercontent.com/google/fonts/"
        "2e61f4355afd22b801791b0df176065082423b87/ofl/notosanssc/NotoSansSC%5Bwght%5D.ttf"
    ),
    "sha256": "a3041811a78c361b1de50f953c805e0244951c21c5bd412f7232ef0d899af0da",
    "license_url": (
        "https://raw.githubusercontent.com/google/fonts/"
        "2e61f4355afd22b801791b0df176065082423b87/ofl/notosanssc/OFL.txt"
    ),
    "license_sha256": "1c05c68c34f9708415aada51f17e1b0092d2cea709bf4a94cd38114f9e73d7d9",
}

DM_SANS = {
    "file": "DMSans-Variable.woff2",
    "family": "DM Sans",
    "version": "4.004",
    "license": "OFL-DMSans.txt",
    "url": (
        "https://raw.githubusercontent.com/google/fonts/"
        "2e61f4355afd22b801791b0df176065082423b87/ofl/dmsans/DMSans%5Bopsz,wght%5D.ttf"
    ),
    "sha256": "8cd08d97e89c24d0aa92edd2f0f4c8ee6195eee9b7c9f154865a58b02f0c1c0d",
    "license_url": (
        "https://raw.githubusercontent.com/google/fonts/"
        "2e61f4355afd22b801791b0df176065082423b87/ofl/dmsans/OFL.txt"
    ),
    "license_sha256": "9af36190332437f5ecd09974de43c1f7c77a310a996cdd8ceb25628b458840e1",
}

SOURCES = [NOTO, DM_SANS]


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def download(url: str, target: Path, expected: str) -> Path:
    if target.exists() and sha256(target) == expected:
        return target
    target.parent.mkdir(parents=True, exist_ok=True)
    print(f"download {url}")
    with urllib.request.urlopen(url, timeout=300) as response, target.open("wb") as handle:
        while chunk := response.read(1024 * 1024):
            handle.write(chunk)
    actual = sha256(target)
    if actual != expected:
        raise SystemExit(f"checksum mismatch for {target}: {actual} != {expected}")
    return target


def gb2312_level1() -> str:
    chars = []
    for high in range(0xB0, 0xD8):
        for low in range(0xA1, 0xFA if high == 0xD7 else 0xFF):
            try:
                chars.append(bytes([high, low]).decode("gb2312"))
            except UnicodeDecodeError:
                continue
    return "".join(chars)


def message_characters(value) -> set[str]:
    chars: set[str] = set()

    def walk(node) -> None:
        if isinstance(node, str):
            chars.update(node)
        elif isinstance(node, dict):
            for key, item in node.items():
                walk(key)
                walk(item)
        elif isinstance(node, list):
            for item in node:
                walk(item)

    walk(value)
    return chars


def wanted_characters() -> tuple[list[str], dict[str, int]]:
    messages = json.loads(MESSAGES.read_text(encoding="utf-8"))
    chars = set(ASCII) | set(PUNCTUATION) | set(gb2312_level1())
    chars |= message_characters(messages)
    chars = {char for char in chars if char.isprintable()}
    counts = {
        "messages": len(message_characters(messages)),
        "ascii": len(ASCII),
        "punctuation": len(PUNCTUATION),
        "gb2312_level_1": len(set(gb2312_level1())),
        "total": len(chars),
    }
    return sorted(chars), counts


def subset_font(source: Path, target: Path, text: str) -> None:
    font = TTFont(source)
    options = subset.Options()
    options.flavor = "woff2"
    options.hinting = False
    options.layout_features = LAYOUT_FEATURES
    options.name_IDs = ["*"]
    options.name_legacy = True
    options.name_languages = ["*"]
    options.notdef_outline = True
    options.recalc_bounds = True
    options.recalc_timestamp = False
    subsetter = subset.Subsetter(options=options)
    subsetter.populate(text=text)
    subsetter.subset(font)
    font.flavor = "woff2"
    target.parent.mkdir(parents=True, exist_ok=True)
    font.save(target)
    font.close()


def prepare_source(spec: dict, cache: Path) -> tuple[Path, Path]:
    font_path = download(spec["url"], cache / Path(spec["url"]).name, spec["sha256"])
    license_path = download(
        spec["license_url"], cache / spec["license"], spec["license_sha256"]
    )
    (FONTS / spec["license"]).write_bytes(license_path.read_bytes())
    return font_path, license_path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache-dir", type=Path, default=CACHE)
    args = parser.parse_args()

    chars, counts = wanted_characters()
    text = "".join(chars)
    print(f"characters: {counts['total']} (messages {counts['messages']}, "
          f"ASCII {counts['ascii']}, punctuation {counts['punctuation']}, "
          f"GB2312-1 {counts['gb2312_level_1']})")

    FONTS.mkdir(parents=True, exist_ok=True)
    fonts = []
    for spec in SOURCES:
        source, _ = prepare_source(spec, args.cache_dir)
        target = FONTS / spec["file"]
        subset_font(source, target, text)
        size = target.stat().st_size
        print(f"  {target.name}: {size:,} bytes")
        fonts.append({
            "file": spec["file"],
            "family": spec["family"],
            "bytes": size,
            "sha256": sha256(target),
        })

    manifest = {
        "generated_by": "desktop/scripts/build-font-subsets.py",
        "sources": [
            {
                "font": spec["family"],
                "version": spec["version"],
                "url": spec["url"],
                "sha256": spec["sha256"],
            }
            for spec in SOURCES
        ],
        "counts": counts,
        "characters": text,
        "fonts": fonts,
    }
    MANIFEST.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    total = sum(font["bytes"] for font in fonts)
    print(f"total: {total:,} bytes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
