#!/bin/sh
# Build a tiny git repository at $1 (default: /tmp/alt-demo-binaries-src)
# that holds a handful of binary fixtures designed to show off alt's
# format-aware diff: PNG (perceptual + chunk), JSON / TOML (semantic),
# ZIP (member-level). Each fixture is committed twice (initial + a
# deliberate edit), so the resulting `alt log` has obvious diffs to
# render in the web browser.
#
# Run via `scripts/build-demo-binaries.sh` and follow up with
# `alt import` to produce a .alt store the server can host.
set -eu
DIR="${1:-/tmp/alt-demo-binaries-src}"
rm -rf "$DIR"
mkdir -p "$DIR"
cd "$DIR"

git init -q -b main
git config user.name "alt-demo"
git config user.email "alt-demo@golia.jp"
git config commit.gpgsign false

python3 - "$DIR" <<'PY'
import os, sys, json, zipfile, io
from pathlib import Path
from PIL import Image, ImageDraw

ROOT = Path(sys.argv[1])

# ----- PNG --------------------------------------------------------------
# A 240×160 banner with a colour block + a label.
def make_banner(path: Path, body: str, fg: tuple, bg: tuple):
    img = Image.new("RGB", (240, 160), bg)
    draw = ImageDraw.Draw(img)
    draw.rectangle([(20, 30), (220, 130)], fill=fg)
    draw.text((28, 56), body, fill=bg)
    img.save(path, "PNG", optimize=True)

(ROOT / "art").mkdir()
make_banner(ROOT / "art" / "banner.png", "alt v1", (90, 160, 250), (15, 20, 30))

# ----- JSON -------------------------------------------------------------
(ROOT / "config").mkdir()
config = {
    "name": "alt-demo",
    "limits": {"max_size": 1024, "max_files": 32},
    "features": {"signing": True, "diff": True, "search": False},
    "endpoints": ["wire", "api"],
}
(ROOT / "config" / "settings.json").write_text(json.dumps(config, indent=2) + "\n")

# ----- TOML -------------------------------------------------------------
toml = """[package]
name = "alt-demo"
version = "0.1.0"
edition = "2024"

[dependencies]
ureq = "2"
serde = "1.0"

[profile.release]
opt-level = 3
"""
(ROOT / "config" / "Cargo.toml").write_text(toml)

# ----- ZIP --------------------------------------------------------------
(ROOT / "archive").mkdir()
buf = io.BytesIO()
with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr("docs/README.md", "alt — pure-Rust VCS demo bundle\n")
    z.writestr("docs/CHANGELOG.md", "v1: initial release.\n")
    z.writestr("manifest.json", '{"version": 1, "files": 2}\n')
(ROOT / "archive" / "bundle.zip").write_bytes(buf.getvalue())
PY

git add .
git commit -q -m "demo: initial PNG / JSON / TOML / ZIP fixtures

A banner PNG plus a JSON config, a TOML manifest, and a small ZIP
bundle — the four binary shapes alt has format-aware diff for. The
follow-up commits each touch one of these so the web product page can
show the per-format renderer in action."

# ----- Edit pass --------------------------------------------------------
python3 - "$DIR" <<'PY'
import sys, json, zipfile, io
from pathlib import Path
from PIL import Image, ImageDraw

ROOT = Path(sys.argv[1])

# PNG: re-paint the block + change the label text — same dimensions so
# the perceptual fingerprint can compare like-for-like.
img = Image.new("RGB", (240, 160), (15, 20, 30))
draw = ImageDraw.Draw(img)
draw.rectangle([(20, 30), (220, 130)], fill=(232, 168, 124))  # copper
draw.text((28, 56), "alt v2", fill=(15, 20, 30))
img.save(ROOT / "art" / "banner.png", "PNG", optimize=True)

# JSON: change a nested value, add a key, drop one.
config = json.loads((ROOT / "config" / "settings.json").read_text())
config["limits"]["max_size"] = 4096
config["features"]["search"] = True
config["features"]["sync"] = True
del config["endpoints"]
(ROOT / "config" / "settings.json").write_text(json.dumps(config, indent=2) + "\n")

# TOML: bump version, swap dep version, add a new dep.
toml = """[package]
name = "alt-demo"
version = "0.2.0"
edition = "2024"

[dependencies]
ureq = "3"
serde = "1.0"
toml = "0.8"

[profile.release]
opt-level = 3
lto = "thin"
"""
(ROOT / "config" / "Cargo.toml").write_text(toml)

# ZIP: rewrite the changelog, add a new file, drop the manifest.
buf = io.BytesIO()
with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr("docs/README.md", "alt — pure-Rust VCS demo bundle\n")
    z.writestr(
        "docs/CHANGELOG.md",
        "v2: per-actor signing, format-aware diff, daemon path.\n",
    )
    z.writestr("docs/PRESS.md", "alt is a pure-Rust VCS.\n")
(ROOT / "archive" / "bundle.zip").write_bytes(buf.getvalue())
PY

git add .
git commit -q -m "demo: edit pass — exercise each format's diff renderer

- art/banner.png: re-paint the block (copper) + change the label so the
  perceptual fingerprint distance is non-trivial.
- config/settings.json: change a nested numeric, add a key, remove a
  key — the structured renderer shows three change kinds in one file.
- config/Cargo.toml: version bumps + dep additions — semantic TOML diff
  across multiple sections.
- archive/bundle.zip: replace one member's content, add a new member,
  drop another — exercises the part-aware ZIP renderer."

# Echo the resulting tip oid so the caller can pipe straight into alt import.
git log --oneline -5
