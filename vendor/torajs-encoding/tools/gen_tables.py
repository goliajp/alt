#!/usr/bin/env python3
"""Generate torajs-encoding's encoding.rs, index.rs and cjk_index/*.rs
from the WHATWG Encoding Standard data files (encodings.json,
index-<name>.txt).

usage: gen_tables.py <data dir> <crate src dir>
then: rustfmt --edition 2024 <crate src dir>/index.rs <crate src dir>/cjk_index/*.rs

The data dir holds the standard's encodings.json, the index-<name>.txt
of every single-byte encoding, and index-gb18030.txt,
index-gb18030-ranges.txt, index-big5.txt, index-jis0208.txt,
index-jis0212.txt and index-euc-kr.txt. Nothing in the build runs this
script.
"""
import json
import re
import sys

data, out = sys.argv[1], sys.argv[2]
groups = json.load(open(f"{data}/encodings.json"))
encs = [e for g in groups for e in g["encodings"]]
single = [e["name"] for e in groups[1]["encodings"]]


def variant(name):
    parts = re.split(r"[-_]", name)
    s = ""
    for p in parts:
        if s and s[-1].isdigit() and p[0].isdigit():
            s += "_"
        s += p[0].upper() + p[1:].lower()
    return s


def table_of(name):
    # ISO-8859-8-I decodes with ISO-8859-8's index
    n = name.lower()
    return "iso-8859-8" if n == "iso-8859-8-i" else n


def static_of(table):
    return table.upper().replace("-", "_")


HEAD = """// CODEGEN: tools/gen_tables.py over the WHATWG Encoding Standard's data
// files https://encoding.spec.whatwg.org/encodings.json and
// https://encoding.spec.whatwg.org/index-<name>.txt, then rustfmt; do not
// edit by hand.
"""

# encoding.rs
lines = [HEAD, "//! The encodings of the Encoding Standard (§4) and their labels.\n"]
lines.append("/// An encoding of the Encoding Standard, in the order of its table (§4.2).")
lines.append("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]")
lines.append("#[repr(u8)]")
lines.append("pub enum Encoding {")
for e in encs:
    lines.append(f"    /// `{e['name']}`")
    lines.append(f"    {variant(e['name'])},")
lines.append("}\n")
lines.append("impl Encoding {")
lines.append("    /// Every encoding, in discriminant order.")
lines.append(f"    pub const ALL: [Encoding; {len(encs)}] = [")
for e in encs:
    lines.append(f"        Encoding::{variant(e['name'])},")
lines.append("    ];\n")
lines.append("    /// The encoding's name, as the standard spells it.")
lines.append("    pub const fn name(self) -> &'static str {")
lines.append("        match self {")
for e in encs:
    lines.append(f"            Encoding::{variant(e['name'])} => \"{e['name']}\",")
lines.append("        }")
lines.append("    }\n")
lines.append("    /// The index a legacy single-byte encoding decodes with.")
lines.append("    pub fn single_byte_index(self) -> Option<&'static crate::single_byte::Index> {")
lines.append("        use crate::index::*;")
lines.append("        Some(match self {")
for n in single:
    lines.append(f"            Encoding::{variant(n)} => &{static_of(table_of(n))},")
lines.append("            _ => return None,")
lines.append("        })")
lines.append("    }")
lines.append("}\n")
labels = sorted((l, e["name"]) for e in encs for l in e["labels"])
assert all(l == l.lower() and l.isascii() and " " not in l for l, _ in labels)
# for_label's table: a perfect hash over the labels. A key is read as
# three little-endian u64 words of its bytes zero-padded to 24, with bit
# 5 of every byte set so that ASCII case does not change the hash; the
# hash is the words times LABEL_HASH, xored together. Its top 7 bits
# pick a bucket, bits 32..40 xored with the bucket's displacement pick a
# slot.
M64 = (1 << 64) - 1
LONGEST = 20
assert all(len(l) <= LONGEST for l, _ in labels)


def words(l):
    b = bytes(c | 0x20 for c in l.encode().ljust(24, b"\0"))
    return [int.from_bytes(b[i:i + 8], "little") for i in (0, 8, 16)]


def splitmix(state):
    state = (state + 0x9E3779B97F4A7C15) & M64
    z = state
    z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & M64
    z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & M64
    return state, z ^ (z >> 31)


def place(keys):
    rng = 0
    while True:
        k = []
        for _ in range(3):
            rng, z = splitmix(rng)
            k.append(z | 1)
        hs = []
        for l, _ in keys:
            w = words(l)
            hs.append(((w[0] * k[0]) ^ (w[1] * k[1]) ^ (w[2] * k[2])) & M64)
        buckets = [[] for _ in range(128)]
        for i, h in enumerate(hs):
            buckets[h >> 57].append(i)
        slots = [None] * 256
        disp = [0] * 128
        ok = True
        for b in sorted(range(128), key=lambda b: -len(buckets[b])):
            members = buckets[b]
            if not members:
                continue
            for d in range(256):
                want = [((hs[i] >> 32) & 255) ^ d for i in members]
                if len(set(want)) == len(want) and all(slots[s] is None for s in want):
                    for i, s in zip(members, want):
                        slots[s] = i
                    disp[b] = d
                    break
            else:
                ok = False
                break
        if ok:
            return k, disp, slots


hash_k, disp, slots = place(labels)
blob, slot_rows = "", []
for s in slots:
    if s is None:
        slot_rows.append("(0, u8::MAX, Encoding::Replacement)")
        continue
    l, n = labels[s]
    slot_rows.append(f"({len(blob)}, {len(l)}, Encoding::{variant(n)})")
    blob += l
assert len(blob) < 1 << 16
lines.append("/// The labels' perfect hash multipliers, one per eight bytes of key.")
lines.append("pub(crate) const LABEL_HASH: [u64; 3] = [" + ", ".join(f"0x{v:016X}" for v in hash_k) + "];\n")
lines.append("/// Each hash bucket's displacement.")
lines.append("pub(crate) static LABEL_DISPLACEMENTS: [u8; 128] = [")
lines += ["    " + " ".join(f"{v}," for v in disp[i:i + 16]) for i in range(0, 128, 16)]
lines.append("];\n")
lines.append("/// Each slot's label (its start in [`LABEL_BYTES`] and its length,")
lines.append("/// `u8::MAX` for an empty slot) and that label's encoding.")
lines.append("pub(crate) static LABEL_SLOTS: [(u16, u8, Encoding); 256] = [")
lines += [f"    {r}," for r in slot_rows]
lines.append("];\n")
lines.append("/// Every label in slot order, unseparated, then 24 zero bytes so a")
lines.append("/// 24-byte read from any label's start stays in bounds.")
lines.append("pub(crate) static LABEL_BYTES: &[u8] = b\"\\")
blob += "\0" * 24
for i in range(0, len(blob), 80):
    lines.append("    " + blob[i:i + 80].replace("\0", "\\0") + "\\")
lines[-1] = lines[-1][:-1] + "\";"
open(f"{out}/encoding.rs", "w").write("\n".join(lines) + "\n")

# index.rs
lines = [HEAD, "//! The legacy single-byte indexes (§9): entry `p` is the code point of",
         "//! byte `0x80 + p`, 0 where the index has no entry.\n",
         "use crate::single_byte::Index;\n"]
seen = []
for n in single:
    t = table_of(n)
    if t in seen:
        continue
    seen.append(t)
    vals = [0] * 128
    for ln in open(f"{data}/index-{t}.txt", encoding="utf-8"):
        if ln.startswith("#") or not ln.strip():
            continue
        p, cp = ln.split("\t")[:2]
        cp = int(cp, 16)
        # the decoder writes an entry as two or three bytes of UTF-8
        assert 0x80 <= cp <= 0xFFFF and not 0xD800 <= cp <= 0xDFFF
        vals[int(p)] = cp
    lines.append(f"/// index-{t}.txt")
    lines.append(f"pub static {static_of(t)}: Index = Index([")
    for i in range(0, 128, 8):
        lines.append("    " + " ".join(f"0x{v:04X}," for v in vals[i:i + 8]))
    lines.append("]);\n")
open(f"{out}/index.rs", "w").write("\n".join(lines))


def read_index(name):
    rows = []
    for ln in open(f"{data}/index-{name}.txt", encoding="utf-8"):
        if ln.startswith("#") or not ln.strip():
            continue
        p, cp = ln.split("\t")[:2]
        rows.append((int(p), int(cp, 16)))
    return rows


def hex_rows(vals, width, per_row):
    return ["    " + " ".join(f"0x{v:0{width}X}," for v in vals[i:i + per_row])
            for i in range(0, len(vals), per_row)]


# cjk_index/<name>.rs: one file per multi-byte index, each its own
# statics, linked only where a decoder reads them
for name in ["gb18030", "big5", "jis0208", "jis0212", "euc-kr"]:
    rows = read_index(name)
    first, last = rows[0][0], rows[-1][0]
    assert [p for p, _ in rows] == sorted({p for p, _ in rows})
    vals = [0] * (last - first + 1)
    plane2 = []
    for p, cp in rows:
        # 0 marks no entry
        assert cp != 0
        # the decoders read an entry as a scalar value unchecked, and a
        # surrogate stored in Big5 stands for a plane 2 entry
        assert not 0xD800 <= cp <= 0xDFFF
        if cp > 0xFFFF:
            # Big5 alone reaches past the BMP, and only into plane 2
            assert name == "big5" and cp >> 16 == 2
            vals[p - first] = 0xD800 + len(plane2)
            plane2.append(cp & 0xFFFF)
        else:
            vals[p - first] = cp
    assert len(plane2) <= 0x800
    lines = [HEAD, f"//! index-{name}.txt: entry `p` of [`CODE_POINTS`] is the code point",
             "//! of pointer [`FIRST`] + `p`, 0 where the index has no entry."]
    if name == "big5":
        lines.append("//! An entry 0xD800 + `k`, a surrogate, stands for the plane 2 code")
        lines.append("//! point U+2xxxx whose low 16 bits are entry `k` of [`PLANE_2`].")
    lines.append("")
    lines.append("/// The first pointer of the index.")
    lines.append(f"pub(crate) const FIRST: usize = {first};\n")
    lines.append(f"/// index-{name}.txt from pointer [`FIRST`] on.")
    lines.append(f"pub(crate) static CODE_POINTS: [u16; {len(vals)}] = [")
    lines += hex_rows(vals, 4, 12)
    lines.append("];")
    if name == "big5":
        lines.append("\n/// The low 16 bits of the plane 2 entries, in pointer order.")
        lines.append(f"pub(crate) static PLANE_2: [u16; {len(plane2)}] = [")
        lines += hex_rows(plane2, 4, 12)
        lines.append("];")
    if name == "gb18030":
        ranges = read_index("gb18030-ranges")
        assert ranges[0] == (0, 0x80)
        lines.append("\n/// index-gb18030-ranges.txt: each range's first pointer and code point.")
        lines.append(f"pub(crate) static RANGES: [(u32, u32); {len(ranges)}] = [")
        for i in range(0, len(ranges), 4):
            row = ranges[i:i + 4]
            lines.append("    " + " ".join(f"({p}, 0x{cp:04X})," for p, cp in row))
        lines.append("];")
    path = f"{out}/cjk_index/{name.replace('-', '_')}.rs"
    open(path, "w").write("\n".join(lines) + "\n")

print(len(encs), "encodings", len(labels), "labels", len(seen), "indexes")
