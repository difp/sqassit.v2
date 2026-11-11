"""Offline Python reference for the opt-in search_norm comparison.

Emits the reference normalization for curated vectors and, as a compact
exhaustive check, the case-folded text of every Unicode scalar value in
chunks. Case folding is a per-codepoint operation, so comparing each chunk
is equivalent to comparing every codepoint individually.
"""
import json
import sys

sys.path.insert(0, "legacy")
from app.database import search_norm

CURATED = [
    "",
    "Ёж",
    "ёж",
    "ЁЖ",
    "Еж",
    "Straße",
    "STRASSE",
    "ẞ",
    "ΣΟΦΟΣ",
    "σοφος",
    "ς",
    "İstanbul",
    "ıI",
    "ﬀﬁﬂﬃﬄ",
    "ＡＢＣ１２３",
    "Привет, МИР!",
    "  spaced  ",
    "Ёлка-палка",
    "e\u0301",
    "É",
    "ǅ",
    "ᾈ",
    "𐐀",
    "😀Ё😀",
    "%_\\",
    "Ｋ",
    "Ⅻ",
    "①",
]

CHUNK = 0x1000


def main():
    chunks = []
    for start in range(0, 0x110000, CHUNK):
        text = "".join(
            chr(code)
            for code in range(start, min(start + CHUNK, 0x110000))
            if not 0xD800 <= code <= 0xDFFF
        )
        chunks.append({"start": start, "normalized": search_norm(text)})
    print(json.dumps({
        "curated": [{"input": value, "normalized": search_norm(value)} for value in CURATED],
        "chunks": chunks,
    }, ensure_ascii=False))


if __name__ == "__main__":
    main()
