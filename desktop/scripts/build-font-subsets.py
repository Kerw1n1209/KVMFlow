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
import zipfile
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
    "file": "NotoSerifSC-Variable.woff2",
    "family": "Noto Serif SC",
    "rename": None,
    "version": "2.003",
    "license": "OFL-NotoSerifSC.txt",
    "url": (
        "https://raw.githubusercontent.com/google/fonts/"
        "2e61f4355afd22b801791b0df176065082423b87/ofl/notoserifsc/NotoSerifSC%5Bwght%5D.ttf"
    ),
    "sha256": "050080d9255a86808f2945bffac582b31ef32bc36411ce29563b4961670c66f9",
    "license_url": (
        "https://raw.githubusercontent.com/google/fonts/"
        "2e61f4355afd22b801791b0df176065082423b87/ofl/notoserifsc/OFL.txt"
    ),
    "license_sha256": "5e0da210fb04058a8c0087985d2d456b931c2579811a49655721d3cf0c36b6d6",
}

SOURCE_SERIF = {
    "file": "KVMFlowSerif-Variable.woff2",
    "family": "KVMFlow Serif",
    "rename": "KVMFlow Serif",
    "version": "4.005",
    "license": "OFL-SourceSerif4.txt",
    "url": (
        "https://github.com/adobe-fonts/source-serif/releases/download/"
        "4.005R/source-serif-4.005_Desktop.zip"
    ),
    "sha256": "549fdb8f9a682bd06944298621404969f6de77c2e422ff3b8244a1dcd6a0c425",
    "member": "source-serif-4.005_Desktop/VAR/SourceSerif4Variable-Roman.ttf",
    "license_member": "source-serif-4.005_Desktop/LICENSE.md",
    "license_sha256": "75784a295293a8992f5a8d99210566e0064a012e6dab6731305e3787f15896c7",
}

SOURCES = [NOTO, SOURCE_SERIF]


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


def rename_font(font: TTFont, family: str) -> None:
    name = font["name"]
    for record in list(name.names):
        if record.nameID in {1, 2, 3, 4, 6, 16, 17, 18, 20, 21, 22, 25}:
            name.names.remove(record)
    values = {
        1: family,
        2: "Regular",
        3: family.replace(" ", "") + ";subset;KVMFlow",
        4: family,
        6: family.replace(" ", ""),
        16: family,
        17: "Regular",
        21: family,
        22: "Regular",
    }
    for name_id, value in values.items():
        name.setName(value, name_id, 3, 1, 0x409)
        name.setName(value, name_id, 1, 0, 0)


def subset_font(source: Path, target: Path, text: str, rename: str | None) -> None:
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
    if rename:
        rename_font(font, rename)
    font.flavor = "woff2"
    target.parent.mkdir(parents=True, exist_ok=True)
    font.save(target)
    font.close()


def prepare_source(spec: dict, cache: Path) -> tuple[Path, Path]:
    if "member" in spec:
        archive = download(spec["url"], cache / Path(spec["url"]).name, spec["sha256"])
        with zipfile.ZipFile(archive) as bundle:
            font_bytes = bundle.read(spec["member"])
            license_bytes = bundle.read(spec["license_member"])
        font_path = cache / Path(spec["member"]).name
        font_path.write_bytes(font_bytes)
        license_path = cache / "source-serif-LICENSE.md"
        license_path.write_bytes(license_bytes)
    else:
        font_path = download(spec["url"], cache / Path(spec["url"]).name, spec["sha256"])
        license_path = download(
            spec["license_url"], cache / "noto-OFL.txt", spec["license_sha256"]
        )
    if sha256(license_path) != spec["license_sha256"]:
        raise SystemExit(f"license checksum mismatch for {license_path}")
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
        subset_font(source, target, text, spec["rename"])
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
                "modified": bool(spec["rename"]),
                "rename_reason": (
                    "OFL Reserved Font Name 'Source' requires a new primary name"
                    if spec["rename"] else None
                ),
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
