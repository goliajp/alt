#!/bin/sh
# Build a richer fixture git repository at $1 (default
# /tmp/alt-demo-binaries-src) that exists to show alt's diff capabilities
# the public product page wants to demonstrate.
#
# Two axes:
#
#   - Binary-aware (the real differentiation): PNG chunks + perceptual
#     fingerprint, ZIP/OOXML members, real .docx and .xlsx walks. git
#     reports these as `Binary files differ`; alt reports each member's
#     change.
#
#   - Semantic over text (alt's upgrade of git's line diff): JSON + TOML
#     by jq-path rather than line, so a re-indented file is `(no
#     semantic changes)` and a key bump shows `$.foo.bar 1 → 2` instead
#     of two opaque line diffs.
#
# The repo is a tiny website project (homepage assets, content config,
# docs, archive) that evolves over multiple commits — each commit
# touches a different facet so every renderer has at least one real
# example to point at.
#
# Requires a python3 venv with python-docx + openpyxl + pillow.  See
# `/tmp/alt-demo-venv` for the one this script creates / reuses.
set -eu
DIR="${1:-/tmp/alt-demo-binaries-src}"
VENV="${ALT_DEMO_VENV:-/tmp/alt-demo-venv}"

if [ ! -x "$VENV/bin/python" ]; then
    echo "[demo] creating venv at $VENV"
    python3 -m venv "$VENV"
    "$VENV/bin/pip" install --quiet python-docx openpyxl pillow
fi
PYTHON="$VENV/bin/python"

rm -rf "$DIR"
mkdir -p "$DIR"
cd "$DIR"

git init -q -b main
git config user.name "alt-demo"
git config user.email "alt-demo@golia.jp"
git config commit.gpgsign false

# Shared helpers exported as one file so each commit step can re-import.
cat > /tmp/alt-demo-helpers.py <<'HELPERS'
import io, json, random, zipfile
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont
from docx import Document
from docx.shared import Pt, RGBColor, Inches
from openpyxl import Workbook
from openpyxl.styles import Font, PatternFill

def font(size: int = 20):
    for cand in (
        "/System/Library/Fonts/Supplemental/Verdana.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ):
        try:
            return ImageFont.truetype(cand, size)
        except OSError:
            continue
    return ImageFont.load_default()

def banner(path: Path, *, bg, fg_block, text, secondary=None):
    img = Image.new("RGB", (480, 240), bg)
    d = ImageDraw.Draw(img)
    d.rectangle([(30, 30), (450, 210)], fill=fg_block)
    d.text((45, 60), text, fill=bg, font=font(36))
    if secondary:
        d.text((45, 130), secondary, fill=bg, font=font(20))
    img.save(path, "PNG", optimize=True)

def icon(path: Path, *, bg, fg):
    img = Image.new("RGB", (96, 96), bg)
    d = ImageDraw.Draw(img)
    d.ellipse([(12, 12), (84, 84)], fill=fg)
    d.text((34, 28), "α", fill=bg, font=font(40))
    img.save(path, "PNG", optimize=True)

def landscape(path: Path, seed: int, *, palette):
    rng = random.Random(seed)
    img = Image.new("RGB", (640, 360), palette["sky"])
    d = ImageDraw.Draw(img)
    # sun
    d.ellipse([(420, 50), (560, 190)], fill=palette["sun"])
    # rolling hills
    horizon = 220
    for i, hue in enumerate(palette["hills"]):
        offset = horizon + i * 25
        pts = [(0, offset)]
        for x in range(0, 641, 80):
            jitter = rng.randint(-12, 12)
            pts.append((x, offset + jitter))
        pts.append((640, 360))
        pts.append((0, 360))
        d.polygon(pts, fill=hue)
    # scattered "stars" (texture)
    for _ in range(rng.randint(20, 60)):
        x, y = rng.randint(0, 639), rng.randint(0, 200)
        d.point((x, y), fill=palette["star"])
    img.save(path, "PNG", optimize=True)

def docx_doc(path: Path, *, title, sections):
    doc = Document()
    h = doc.add_heading(title, level=0)
    h.runs[0].font.color.rgb = RGBColor(0x1F, 0x4E, 0xD8)
    for heading, paragraphs in sections:
        doc.add_heading(heading, level=1)
        for p in paragraphs:
            run = doc.add_paragraph().add_run(p)
            run.font.size = Pt(11)
    doc.save(path)

def xlsx_book(path: Path, *, sheets):
    wb = Workbook()
    wb.remove(wb.active)
    for name, header, rows in sheets:
        ws = wb.create_sheet(title=name)
        for c, h in enumerate(header, start=1):
            cell = ws.cell(row=1, column=c, value=h)
            cell.font = Font(bold=True, color="FFFFFF")
            cell.fill = PatternFill("solid", fgColor="1F4ED8")
        for r, row in enumerate(rows, start=2):
            for c, value in enumerate(row, start=1):
                ws.cell(row=r, column=c, value=value)
    wb.save(path)

def zip_bundle(path: Path, members):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in members.items():
            z.writestr(name, data if isinstance(data, (bytes, bytearray)) else data.encode())
    path.write_bytes(buf.getvalue())
HELPERS

run_python() {
    "$PYTHON" - "$DIR" "$1" <<EOF
import sys
sys.path.insert(0, "/tmp")
from alt_demo_helpers import *  # noqa
from pathlib import Path
ROOT = Path(sys.argv[1])
STEP = int(sys.argv[2])
EOF
}

# We embed each step's body in a separate heredoc so it can use the
# helpers via `exec(open('/tmp/alt-demo-helpers.py').read())`.
step_python() {
    LABEL="$1"
    BODY="$2"
    "$PYTHON" - "$DIR" <<EOF
import sys
from pathlib import Path
ROOT = Path(sys.argv[1])
exec(open("/tmp/alt-demo-helpers.py").read(), globals())
${BODY}
EOF
    git add .
    git commit -q -m "$LABEL"
}

# ─── C1: skeleton ───────────────────────────────────────────────────
mkdir -p assets/img assets/docs config archive site
step_python "demo: c1 — initial site skeleton

PNG banner + icon, a Word press release, an Excel pricing book, a JSON
config, a TOML manifest, a zip bundle. Sets up the four binary shapes
alt's diff renderer can speak about, plus the two text shapes the
semantic renderer handles." "
banner(ROOT/'assets/img/banner.png', bg=(15,20,30), fg_block=(90,160,250), text='alt v1', secondary='pure-Rust VCS')
icon(ROOT/'assets/img/icon.png', bg=(15,20,30), fg=(90,160,250))
landscape(ROOT/'assets/img/landing.png', seed=1, palette={
    'sky':(28, 36, 60), 'sun':(220, 200, 120),
    'hills':[(80,90,140),(60,70,110),(40,50,80)],
    'star':(220,210,150)})

docx_doc(ROOT/'assets/docs/press.docx',
    title='alt — pure-Rust VCS',
    sections=[
        ('Overview', [
            'alt is a version control system rebuilt from first principles in safe Rust.',
            'It speaks the git wire so existing servers keep working.',
        ]),
        ('Status', ['Pre-1.0 dogfood. Install from source.']),
    ])

xlsx_book(ROOT/'assets/docs/pricing.xlsx',
    sheets=[
        ('Plans', ['Tier', 'Repos', 'Storage GB', 'USD/mo'], [
            ('Personal', 5, 2, 0),
            ('Team', 50, 50, 12),
            ('Org', 500, 500, 99),
        ]),
        ('Features', ['Feature', 'Personal', 'Team', 'Org'], [
            ('Signing', 'yes', 'yes', 'yes'),
            ('Search',  'no',  'no',  'no'),
            ('SSO',     'no',  'no',  'yes'),
        ]),
    ])

(ROOT/'config/settings.json').write_text(json.dumps({
    'name': 'alt-demo',
    'limits': {'max_size': 1024, 'max_files': 32, 'concurrency': 4},
    'features': {'signing': True, 'diff': True, 'search': False},
    'endpoints': ['wire', 'api'],
    'metadata': {'created_at': '2026-06-01', 'license': 'MIT OR Apache-2.0'},
}, indent=2) + '\n')

(ROOT/'config/Cargo.toml').write_text('''[package]
name = \"alt-demo\"
version = \"0.1.0\"
edition = \"2024\"
license = \"MIT OR Apache-2.0\"

[dependencies]
ureq = \"2\"
serde = { version = \"1.0\", features = [\"derive\"] }

[profile.release]
opt-level = 3
''')

zip_bundle(ROOT/'archive/bundle.zip', {
    'docs/README.md': 'alt — pure-Rust VCS demo bundle\\n',
    'docs/CHANGELOG.md': 'v0.1: initial public bundle.\\n',
    'manifest.json': '{\"version\": 1, \"files\": 2}\\n',
})
"

# ─── C2: small visual tweak (PNG perceptual close) ────────────────
step_python "demo: c2 — re-tone the banner (perceptual-close edit)

Same composition, slightly warmer accent. PNG chunk-level diff shows
IDAT changed; perceptual fingerprint distance is tiny because the image
is structurally the same." "
banner(ROOT/'assets/img/banner.png', bg=(15,20,30),
       fg_block=(120,170,240), text='alt v1', secondary='pure-Rust VCS')
"

# ─── C3: big visual swap + new icon (perceptual-far) ──────────────
step_python "demo: c3 — copper rebrand (perceptual-far PNG change)

Hero banner re-laid-out in the copper accent that ships on alt.golia.jp:
the colour block moves, a new bottom stripe is added, the text shifts.
A structural change so the PNG perceptual fingerprint distance is much
larger than C2's. Icon repainted to match." "
from PIL import Image, ImageDraw
def banner_v2(path, *, bg, fg, accent, text, secondary):
    img = Image.new('RGB', (480, 240), bg)
    d = ImageDraw.Draw(img)
    # offset block
    d.rectangle([(160, 20), (460, 150)], fill=fg)
    # bottom stripe
    d.rectangle([(0, 180), (480, 240)], fill=accent)
    d.text((176, 50), text, fill=bg, font=font(38))
    d.text((176, 110), secondary, fill=bg, font=font(18))
    d.text((20, 198), '⌘ alt.golia.jp', fill=bg, font=font(16))
    img.save(path, 'PNG', optimize=True)
banner_v2(ROOT/'assets/img/banner.png',
          bg=(15,20,30), fg=(232,168,124),
          accent=(220, 140, 90),
          text='alt v2', secondary='binary-aware diff')
icon(ROOT/'assets/img/icon.png', bg=(15,20,30), fg=(232,168,124))
"

# ─── C4: replace the photo (different scene) ──────────────────────
step_python "demo: c4 — swap the landing photograph

Brand-new landscape (warm desert palette instead of cool dusk). PNG
chunks IHDR + IDAT both change; perceptual distance jumps near 1." "
landscape(ROOT/'assets/img/landing.png', seed=2, palette={
    'sky':(96, 60, 50), 'sun':(255, 220, 140),
    'hills':[(200,120,80),(160,90,70),(110,60,50)],
    'star':(255,240,200)})
"

# ─── C5: doc + ooxml edits ─────────────────────────────────────────
step_python "demo: c5 — expand the press doc + update pricing sheet

Adds a Differentiation section to the press release (.docx walks Word
parts), and bumps the Org tier price plus toggles a few feature flags
in the pricing workbook (.xlsx walks the Excel parts)." "
docx_doc(ROOT/'assets/docs/press.docx',
    title='alt — pure-Rust VCS',
    sections=[
        ('Overview', [
            'alt is a version control system rebuilt from first principles in safe Rust.',
            'It speaks the git wire so existing servers keep working.',
        ]),
        ('Status', ['Pre-1.0 dogfood. Install from source.']),
        ('Differentiation', [
            'Single static binary; no system C dependencies in the default build.',
            'Format-aware diff for PNG, ZIP, OOXML, JSON, and TOML — out of the box.',
        ]),
    ])

xlsx_book(ROOT/'assets/docs/pricing.xlsx',
    sheets=[
        ('Plans', ['Tier', 'Repos', 'Storage GB', 'USD/mo'], [
            ('Personal', 5, 2, 0),
            ('Team', 50, 50, 12),
            ('Org', 500, 1000, 149),
        ]),
        ('Features', ['Feature', 'Personal', 'Team', 'Org'], [
            ('Signing', 'yes', 'yes', 'yes'),
            ('Search',  'yes',  'yes',  'yes'),
            ('SSO',     'no',  'yes',  'yes'),
            ('Audit log', 'no', 'no', 'yes'),
        ]),
    ])
"

# ─── C6: zip restructure ───────────────────────────────────────────
step_python "demo: c6 — restructure the bundle archive

Adds a nested layout (assets/img + assets/style), drops the
manifest.json, rewrites the changelog. ZIP member-level diff shows
adds, removes, and renames across a non-trivial tree." "
zip_bundle(ROOT/'archive/bundle.zip', {
    'docs/README.md': 'alt — pure-Rust VCS demo bundle\\n',
    'docs/CHANGELOG.md': 'v0.2: copper rebrand, larger bundle, OOXML diff demos.\\n',
    'docs/PRESS.md': 'See assets/docs/press.docx for the press kit.\\n',
    'assets/style/site.css': 'body { background: #0d1117; color: #e6edf3; }\\n',
    'assets/img/logo.svg': '<svg viewBox=\"0 0 96 96\"><circle cx=\"48\" cy=\"48\" r=\"36\" fill=\"#e8a87c\"/></svg>\\n',
})
"

# ─── C7: semantic refactor — JSON + TOML ──────────────────────────
step_python "demo: c7 — semantic refactor: JSON config + Cargo manifest

JSON: max_size 1024 → 4096, search false → true, add 'sync', drop
endpoints array. TOML: bump version, add toml + sha2 deps, drop one,
flip an inline table field. The semantic renderer reports per-jq-path
changes rather than the raw text diff." "
import json
(ROOT/'config/settings.json').write_text(json.dumps({
    'name': 'alt-demo',
    'limits': {'max_size': 4096, 'max_files': 64, 'concurrency': 8},
    'features': {'signing': True, 'diff': True, 'search': True, 'sync': True},
    'metadata': {'created_at': '2026-06-01', 'updated_at': '2026-06-18', 'license': 'MIT OR Apache-2.0'},
}, indent=2) + '\\n')

(ROOT/'config/Cargo.toml').write_text('''[package]
name = \"alt-demo\"
version = \"0.2.0\"
edition = \"2024\"
license = \"MIT OR Apache-2.0\"
authors = [\"alt-demo <alt-demo@golia.jp>\"]

[dependencies]
ureq = \"3\"
serde = { version = \"1.0\", features = [\"derive\", \"rc\"] }
toml = \"0.8\"
sha2 = \"0.10\"

[profile.release]
opt-level = 3
lto = \"thin\"
codegen-units = 1
''')
"

# ─── C8: combo commit — visual + content together ─────────────────
step_python "demo: c8 — release-day combo: new section, new icon, new sheet tab

A realistic landing commit: the press release gains a Roadmap section,
the icon picks up a tighter ring, the pricing book gets a new Limits
sheet, and the bundle archive ships the matching style sheet." "
icon(ROOT/'assets/img/icon.png', bg=(13,17,23), fg=(232,168,124))

docx_doc(ROOT/'assets/docs/press.docx',
    title='alt — pure-Rust VCS',
    sections=[
        ('Overview', [
            'alt is a version control system rebuilt from first principles in safe Rust.',
            'It speaks the git wire so existing servers keep working.',
        ]),
        ('Status', ['Pre-1.0 dogfood. Install from source.']),
        ('Differentiation', [
            'Single static binary; no system C dependencies in the default build.',
            'Format-aware diff for PNG, ZIP, OOXML, JSON, and TOML — out of the box.',
        ]),
        ('Roadmap', [
            'Multi-machine sync (in progress), partial-clone wire (M10), and a managed altd server (M9+).',
        ]),
    ])

xlsx_book(ROOT/'assets/docs/pricing.xlsx',
    sheets=[
        ('Plans', ['Tier', 'Repos', 'Storage GB', 'USD/mo'], [
            ('Personal', 10, 5, 0),
            ('Team', 100, 100, 19),
            ('Org', 1000, 2000, 199),
        ]),
        ('Features', ['Feature', 'Personal', 'Team', 'Org'], [
            ('Signing', 'yes', 'yes', 'yes'),
            ('Search',  'yes',  'yes',  'yes'),
            ('SSO',     'no',  'yes',  'yes'),
            ('Audit log', 'no', 'no', 'yes'),
            ('Partial clone', 'yes', 'yes', 'yes'),
        ]),
        ('Limits', ['Tier', 'Repos', 'Concurrent push', 'Webhook calls/day'], [
            ('Personal', 10, 4, 5_000),
            ('Team', 100, 32, 100_000),
            ('Org', 1000, 256, 1_000_000),
        ]),
    ])

zip_bundle(ROOT/'archive/bundle.zip', {
    'docs/README.md': 'alt — pure-Rust VCS demo bundle\\n',
    'docs/CHANGELOG.md': 'v0.3: roadmap section, new pricing sheet, copper icon.\\n',
    'docs/PRESS.md': 'See assets/docs/press.docx for the press kit.\\n',
    'assets/style/site.css': '''body { background: #0d1117; color: #e6edf3; font-family: \"Inter\", sans-serif; }
.hero { color: #e8a87c; }
'''.replace('\\n', '\\n'),
    'assets/style/print.css': '@media print { .nav { display: none; } }\\n',
    'assets/img/logo.svg': '<svg viewBox=\"0 0 96 96\"><circle cx=\"48\" cy=\"48\" r=\"36\" fill=\"#e8a87c\"/><circle cx=\"48\" cy=\"48\" r=\"24\" fill=\"#0d1117\"/></svg>\\n',
})
"

echo
git log --oneline
