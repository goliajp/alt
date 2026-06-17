#!/bin/sh
# Build the public demo-binaries fixture repo for alt.golia.jp.
#
# Eight commits stage one differentiator per commit so the SPA can walk
# a tour of alt's diff renderers without having to hunt:
#
#   C1  initial site skeleton (every fixture shape)
#   C2  banner micro-edit       — PNG, perceptual-close
#   C3  banner re-layout + logo — PNG perceptual-far + SVG structural
#   C4  swap the landing photo  — PNG, very different scene
#   C5  edit press.docx + pricing.xlsx — OOXML member walk
#   C6  restructure bundle.zip + drop a dashboard PNG + rework SVG
#   C7  JSON + TOML semantic refactor
#   C8  release-day combo (docs, sheets, icon, SVG mark, CSS bundle)
#
# The fixtures are deliberately richer than a toy: PNGs carry headlines,
# tagline rows, icon strips, gradients; the SVG logo evolves from a
# single circle to a full wordmark over the course of the history.
#
# Requires a python3 venv with python-docx + openpyxl + pillow at
# $ALT_DEMO_VENV (default /tmp/alt-demo-venv).
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

cat > /tmp/alt-demo-helpers.py <<'HELPERS'
import io, json, math, random, zipfile
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont, ImageFilter
from docx import Document
from docx.shared import Pt, RGBColor
from openpyxl import Workbook
from openpyxl.styles import Font, PatternFill

def font(size: int = 20):
    for cand in (
        "/System/Library/Fonts/Supplemental/Verdana.ttf",
        "/System/Library/Fonts/Supplemental/Verdana Bold.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    ):
        try:
            return ImageFont.truetype(cand, size)
        except OSError:
            continue
    return ImageFont.load_default()

def lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))

def vertical_gradient(size, top, bottom):
    img = Image.new("RGB", size, top)
    px = img.load()
    w, h = size
    for y in range(h):
        c = lerp(top, bottom, y / max(h - 1, 1))
        for x in range(w):
            px[x, y] = c
    return img

def horizontal_gradient(size, left, right):
    img = Image.new("RGB", size, left)
    px = img.load()
    w, h = size
    for x in range(w):
        c = lerp(left, right, x / max(w - 1, 1))
        for y in range(h):
            px[x, y] = c
    return img

# ── banners ────────────────────────────────────────────────────────
def banner_v1(path):
    """Initial hero: cool blue, single block, simple wordmark."""
    img = vertical_gradient((720, 320), (10, 14, 22), (22, 28, 44))
    d = ImageDraw.Draw(img)
    # accent block
    d.rectangle([(60, 80), (660, 240)], fill=(90, 160, 250))
    d.rectangle([(60, 80), (660, 90)], fill=(140, 190, 255))  # top highlight
    d.text((90, 110), "alt", fill=(10, 14, 22), font=font(72))
    d.text((92, 200), "pure-Rust VCS", fill=(10, 14, 22), font=font(22))
    img.save(path, "PNG", optimize=True)

def banner_v2(path):
    """C2 micro-edit: identical layout, slightly warmer accent."""
    img = vertical_gradient((720, 320), (10, 14, 22), (22, 28, 44))
    d = ImageDraw.Draw(img)
    d.rectangle([(60, 80), (660, 240)], fill=(120, 175, 245))
    d.rectangle([(60, 80), (660, 90)], fill=(170, 205, 255))
    d.text((90, 110), "alt", fill=(10, 14, 22), font=font(72))
    d.text((92, 200), "pure-Rust VCS", fill=(10, 14, 22), font=font(22))
    img.save(path, "PNG", optimize=True)

def banner_v3(path):
    """C3 rebrand: copper palette, new layout — sidebar + icon strip."""
    img = horizontal_gradient((720, 320), (16, 22, 32), (28, 22, 16))
    d = ImageDraw.Draw(img)
    # left vertical accent
    d.rectangle([(0, 0), (160, 320)], fill=(232, 168, 124))
    # right column: wordmark + tagline
    d.text((200, 60), "alt", fill=(232, 168, 124), font=font(90))
    d.text((200, 170), "format-aware diff", fill=(220, 220, 230), font=font(26))
    d.text((200, 210), "PNG · OOXML · JSON · TOML", fill=(160, 175, 200), font=font(18))
    # icon strip (four squares)
    palette = [(95, 175, 250), (109, 200, 159), (232, 168, 124), (245, 159, 159)]
    for i, c in enumerate(palette):
        x = 200 + i * 56
        d.rectangle([(x, 260), (x + 40, 290)], fill=c)
        d.rectangle([(x + 6, 266), (x + 34, 284)], fill=(16, 22, 32))
    # diagonal corner stripes
    for off in range(0, 160, 12):
        d.line([(0, off), (off, 0)], fill=(255, 220, 180), width=1)
    img.save(path, "PNG", optimize=True)

def banner_v4(path):
    """C8 final: re-coloured + extra microcopy + sparkline."""
    img = horizontal_gradient((720, 320), (13, 17, 23), (32, 20, 14))
    d = ImageDraw.Draw(img)
    d.rectangle([(0, 0), (160, 320)], fill=(232, 168, 124))
    d.text((200, 50), "alt", fill=(232, 168, 124), font=font(90))
    d.text((200, 160), "format-aware diff", fill=(220, 220, 230), font=font(26))
    d.text((200, 200), "PNG · OOXML · JSON · TOML · ZIP", fill=(160, 175, 200), font=font(18))
    d.text((200, 230), "alt.golia.jp", fill=(120, 135, 160), font=font(16))
    # sparkline: trend through the icon strip
    pts = [(200 + i * 6, 290 - int(20 * math.sin(i / 4))) for i in range(80)]
    for a, b in zip(pts, pts[1:]):
        d.line([a, b], fill=(232, 168, 124), width=2)
    # corner stripes
    for off in range(0, 160, 10):
        d.line([(0, off), (off, 0)], fill=(255, 220, 180), width=1)
    img.save(path, "PNG", optimize=True)

# ── icon (a small companion mark) ──────────────────────────────────
def icon_v1(path):
    img = vertical_gradient((128, 128), (10, 14, 22), (22, 28, 44))
    d = ImageDraw.Draw(img)
    d.ellipse([(20, 20), (108, 108)], fill=(90, 160, 250))
    d.text((44, 36), "α", fill=(10, 14, 22), font=font(54))
    img.save(path, "PNG", optimize=True)

def icon_v2(path):
    """Copper version with inner ring."""
    img = vertical_gradient((128, 128), (10, 14, 22), (22, 28, 44))
    d = ImageDraw.Draw(img)
    d.ellipse([(16, 16), (112, 112)], fill=(232, 168, 124))
    d.ellipse([(36, 36), (92, 92)], fill=(10, 14, 22))
    d.text((46, 50), "α", fill=(232, 168, 124), font=font(40))
    img.save(path, "PNG", optimize=True)

def icon_v3(path):
    """Final mark: notched ring + small accent."""
    img = vertical_gradient((128, 128), (13, 17, 23), (32, 20, 14))
    d = ImageDraw.Draw(img)
    d.ellipse([(12, 12), (116, 116)], fill=(232, 168, 124))
    d.ellipse([(28, 28), (100, 100)], fill=(13, 17, 23))
    d.text((44, 38), "α", fill=(232, 168, 124), font=font(54))
    # bottom-right notch dot
    d.ellipse([(94, 94), (118, 118)], fill=(120, 175, 245))
    img.save(path, "PNG", optimize=True)

# ── landscape (the editorial photograph) ──────────────────────────
def landscape(path, *, seed, palette):
    rng = random.Random(seed)
    img = vertical_gradient((800, 420), palette["sky_top"], palette["sky_bottom"])
    d = ImageDraw.Draw(img)
    # sun / moon
    d.ellipse([(520, 60), (700, 240)], fill=palette["sun"])
    # rolling hills
    horizon = 250
    for i, hue in enumerate(palette["hills"]):
        offset = horizon + i * 30
        pts = [(0, offset)]
        for x in range(0, 801, 60):
            jitter = rng.randint(-14, 14)
            pts.append((x, offset + jitter))
        pts.append((800, 420))
        pts.append((0, 420))
        d.polygon(pts, fill=hue)
    # foreground trees
    for x in range(40, 800, 120):
        h = rng.randint(40, 90)
        d.polygon(
            [(x - 8, 420), (x + 8, 420), (x, 420 - h)],
            fill=palette["tree"],
        )
    # stars / fireflies
    for _ in range(rng.randint(30, 70)):
        x, y = rng.randint(0, 799), rng.randint(0, 200)
        d.point((x, y), fill=palette["spark"])
    img = img.filter(ImageFilter.SMOOTH)
    img.save(path, "PNG", optimize=True)

# ── dashboard (introduced in C6, evolved in C8) ───────────────────
def dashboard_v1(path):
    img = Image.new("RGB", (720, 440), (13, 17, 23))
    d = ImageDraw.Draw(img)
    d.rectangle([(0, 0), (720, 48)], fill=(22, 28, 38))
    d.text((20, 14), "alt — dashboard", fill=(232, 168, 124), font=font(20))
    d.text((560, 16), "live · v0.3", fill=(125, 135, 155), font=font(14))
    cards = [
        ("repos hosted",  "12",  (90, 160, 250)),
        ("commits today", "284", (109, 200, 159)),
        ("pushes / hr",   "47",  (232, 168, 124)),
    ]
    for i, (label, val, accent) in enumerate(cards):
        x = 20 + i * 230
        d.rectangle([(x, 70), (x + 210, 170)], fill=(22, 28, 38))
        d.rectangle([(x, 70), (x + 6, 170)], fill=accent)
        d.text((x + 20, 86), label.upper(), fill=(125, 135, 155), font=font(12))
        d.text((x + 20, 110), val, fill=(230, 235, 245), font=font(38))
    d.rectangle([(20, 200), (700, 420)], fill=(22, 28, 38))
    d.text((30, 212), "pushes by hour", fill=(232, 168, 124), font=font(14))
    bars = [12, 19, 24, 31, 28, 33, 41, 38, 30, 22, 18, 24]
    for i, h in enumerate(bars):
        x = 40 + i * 54
        bar_h = h * 4
        d.rectangle(
            [(x, 400 - bar_h), (x + 36, 400)],
            fill=(90, 160, 250) if i % 3 else (232, 168, 124),
        )
        d.text((x + 6, 405), f"{i:02d}", fill=(125, 135, 155), font=font(11))
    img.save(path, "PNG", optimize=True)

def dashboard_v2(path):
    """Refresh: bigger numbers (org tier launched), reshuffled card
    accents, replaced the bar chart with a line+area sparkline of a
    different metric. Same dimensions so the comparison reads cleanly."""
    img = Image.new("RGB", (720, 440), (13, 17, 23))
    d = ImageDraw.Draw(img)
    d.rectangle([(0, 0), (720, 48)], fill=(22, 28, 38))
    d.text((20, 14), "alt — dashboard", fill=(232, 168, 124), font=font(20))
    d.text((550, 16), "live · v1.0", fill=(109, 200, 159), font=font(14))
    cards = [
        ("repos hosted",   "248", (232, 168, 124)),
        ("commits / day",  "3.1k", (90, 160, 250)),
        ("pushes / hr",    "412",  (109, 200, 159)),
    ]
    for i, (label, val, accent) in enumerate(cards):
        x = 20 + i * 230
        d.rectangle([(x, 70), (x + 210, 170)], fill=(22, 28, 38))
        d.rectangle([(x, 70), (x + 6, 170)], fill=accent)
        d.text((x + 20, 86), label.upper(), fill=(125, 135, 155), font=font(12))
        d.text((x + 20, 110), val, fill=(230, 235, 245), font=font(38))
    # area + line chart (replaces the bar chart)
    d.rectangle([(20, 200), (700, 420)], fill=(22, 28, 38))
    d.text((30, 212), "concurrent connections", fill=(232, 168, 124), font=font(14))
    series = [
        38, 42, 51, 60, 72, 85, 96, 108, 121, 132,
        142, 150, 156, 161, 165, 168, 169, 168, 165, 162,
        158, 153, 146, 138,
    ]
    pts = []
    for i, v in enumerate(series):
        x = 40 + i * 27
        y = 410 - int(v * 1.15)
        pts.append((x, y))
    # filled area
    area = pts + [(pts[-1][0], 415), (pts[0][0], 415)]
    d.polygon(area, fill=(28, 38, 60))
    # line
    for a, b in zip(pts, pts[1:]):
        d.line([a, b], fill=(90, 160, 250), width=2)
    # markers
    for (x, y) in pts:
        d.ellipse([(x - 2, y - 2), (x + 2, y + 2)], fill=(232, 168, 124))
    img.save(path, "PNG", optimize=True)

# ── OOXML helpers ─────────────────────────────────────────────────
def docx_doc(path, *, title, sections):
    doc = Document()
    h = doc.add_heading(title, level=0)
    h.runs[0].font.color.rgb = RGBColor(0x1F, 0x4E, 0xD8)
    for heading, paragraphs in sections:
        doc.add_heading(heading, level=1)
        for p in paragraphs:
            run = doc.add_paragraph().add_run(p)
            run.font.size = Pt(11)
    doc.save(path)

def xlsx_book(path, *, sheets):
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

# ── zip ───────────────────────────────────────────────────────────
def zip_bundle(path, members):
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in members.items():
            z.writestr(name, data if isinstance(data, (bytes, bytearray)) else data.encode())
    path.write_bytes(buf.getvalue())

# ── SVG (kept as plain text so git / alt's text diff renders it,
#         but the layout deliberately changes structurally between
#         versions so the diff carries real value). ──────────────
SVG_V1 = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" width="128" height="128">
  <rect width="128" height="128" fill="#0d1117"/>
  <circle cx="64" cy="64" r="44" fill="#5aa0fa"/>
  <text x="64" y="78" text-anchor="middle"
        font-family="JetBrains Mono, monospace" font-size="48" fill="#0d1117">α</text>
</svg>
"""

SVG_V2 = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" width="128" height="128">
  <defs>
    <linearGradient id="g" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#e8a87c"/>
      <stop offset="100%" stop-color="#d68953"/>
    </linearGradient>
  </defs>
  <rect width="128" height="128" fill="#0d1117"/>
  <circle cx="64" cy="64" r="48" fill="url(#g)"/>
  <circle cx="64" cy="64" r="30" fill="#0d1117"/>
  <text x="64" y="78" text-anchor="middle"
        font-family="JetBrains Mono, monospace" font-size="40" fill="#e8a87c">α</text>
</svg>
"""

SVG_V3 = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 224 96" width="224" height="96">
  <defs>
    <linearGradient id="copper" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#e8a87c"/>
      <stop offset="100%" stop-color="#d68953"/>
    </linearGradient>
    <linearGradient id="ring" x1="0%" y1="0%" x2="0%" y2="100%">
      <stop offset="0%" stop-color="#f3c79a"/>
      <stop offset="100%" stop-color="#b8723a"/>
    </linearGradient>
  </defs>
  <rect width="224" height="96" rx="16" fill="#0d1117"/>
  <g transform="translate(48, 48)">
    <circle r="32" fill="url(#copper)"/>
    <circle r="22" fill="#0d1117"/>
    <circle r="22" fill="none" stroke="url(#ring)" stroke-width="2"/>
    <text x="0" y="8" text-anchor="middle"
          font-family="JetBrains Mono, monospace" font-size="28"
          fill="#e8a87c" font-weight="700">α</text>
  </g>
  <g transform="translate(96, 36)" font-family="Inter, sans-serif" fill="#e6edf3">
    <text x="0" y="0" font-size="32" font-weight="700">alt</text>
    <text x="0" y="22" font-size="12" fill="#7d8590" letter-spacing="1.5">PURE-RUST VCS</text>
    <text x="0" y="38" font-size="11" fill="#5aa0fa">PNG · OOXML · JSON · TOML</text>
  </g>
</svg>
"""
HELPERS

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

mkdir -p assets/img assets/docs assets/vector config archive

# ─── C1: skeleton ───────────────────────────────────────────────────
step_python "demo: c1 — initial site skeleton

Every fixture shape is in place: a hero PNG with a gradient + wordmark,
a companion PNG icon, an editorial landscape photo, a press release
.docx, a pricing .xlsx, a JSON config, a TOML manifest, an SVG logo
mark and a ZIP bundle. Subsequent commits each touch one facet so the
product page can point at a specific diff renderer per commit." "
banner_v1(ROOT/'assets/img/banner.png')
icon_v1(ROOT/'assets/img/icon.png')
landscape(ROOT/'assets/img/landing.png', seed=11, palette={
    'sky_top': (24, 32, 60), 'sky_bottom': (60, 70, 110),
    'sun': (220, 200, 120),
    'hills': [(80,90,140),(60,70,110),(40,50,80)],
    'tree': (24, 32, 48), 'spark': (220, 210, 150),
})
(ROOT/'assets/vector/logo.svg').write_text(SVG_V1)

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
}, indent=2) + '\\n')

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

# ─── C2: banner micro-edit (perceptual-close) ─────────────────────
step_python "demo: c2 — banner micro-edit (perceptual-close PNG)

Same composition; the accent block is one shade brighter and the top
highlight strip warms by 30%. The PNG chunk diff sees IDAT change but
the perceptual fingerprint distance stays near zero because the image
is structurally identical." "
banner_v2(ROOT/'assets/img/banner.png')
"

# ─── C3: copper rebrand — full layout change + new SVG ────────────
step_python "demo: c3 — copper rebrand (perceptual-far PNG + SVG redesign)

Banner re-laid-out: gradient flipped horizontal, a left vertical
accent stripe, the wordmark moved right of centre, a four-square icon
strip and diagonal corner stripes added — the PNG perceptual
fingerprint distance jumps because the silhouette is genuinely
different. The SVG logo gains a gradient definition + an inner cut-out
circle to match." "
banner_v3(ROOT/'assets/img/banner.png')
icon_v2(ROOT/'assets/img/icon.png')
(ROOT/'assets/vector/logo.svg').write_text(SVG_V2)
"

# ─── C4: swap the landing photograph + new svg accent ─────────────
step_python "demo: c4 — swap the landing photograph

Brand-new editorial photo (warm desert palette in place of the cool
dusk), and the SVG mark picks up a stroke ring around the centre cut.
Both PNG fingerprints and SVG text diff carry real change here." "
landscape(ROOT/'assets/img/landing.png', seed=22, palette={
    'sky_top': (96, 60, 50), 'sky_bottom': (240, 180, 120),
    'sun': (255, 220, 140),
    'hills': [(200,120,80),(160,90,70),(110,60,50)],
    'tree': (60, 30, 20), 'spark': (255, 240, 200),
})
# Tweak the SVG mark — add a faint stroke for the ring.
svg = (ROOT/'assets/vector/logo.svg').read_text()
svg = svg.replace('<circle cx=\"64\" cy=\"64\" r=\"30\" fill=\"#0d1117\"/>',
                  '<circle cx=\"64\" cy=\"64\" r=\"30\" fill=\"#0d1117\" stroke=\"#e8a87c\" stroke-width=\"2\"/>')
(ROOT/'assets/vector/logo.svg').write_text(svg)
"

# ─── C5: docx + xlsx walks ─────────────────────────────────────────
step_python "demo: c5 — expand the press doc + update pricing sheet

Adds a Differentiation section to the .docx press release; bumps the
Org pricing tier and adds two feature rows to the .xlsx workbook. The
OOXML walk reports per-internal-part changes: only word/document.xml
on the .docx side, two sheet xmls on the .xlsx side. git would show
both files as opaque binary." "
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

# ─── C6: bundle reshape + dashboard PNG + SVG wordmark ────────────
step_python "demo: c6 — bundle reshape + dashboard mockup + SVG wordmark

Three things happen at once so each renderer has work to do:

1. archive/bundle.zip is rewritten into a real site layout (nested
   docs / assets / styles), drops the loose manifest.json, adds a CSS
   stylesheet and an SVG logo.
2. assets/img/dashboard.png lands as a complex PNG (gradient title
   bar, three accent-rail KPI cards, a 12-bar chart) — the first time
   PNG content carries a meaningful amount of structure.
3. assets/vector/logo.svg is redesigned end-to-end into a horizontal
   wordmark with two gradient defs + multi-line text. The SVG text
   diff now spans both the structure and the content." "
dashboard_v1(ROOT/'assets/img/dashboard.png')
(ROOT/'assets/vector/logo.svg').write_text(SVG_V3)
zip_bundle(ROOT/'archive/bundle.zip', {
    'docs/README.md': 'alt — pure-Rust VCS demo bundle\\n',
    'docs/CHANGELOG.md': 'v0.2: copper rebrand, larger bundle, OOXML diff demos.\\n',
    'docs/PRESS.md': 'See assets/docs/press.docx for the press kit.\\n',
    'assets/style/site.css': 'body { background: #0d1117; color: #e6edf3; }\\n',
    'assets/img/logo.svg': '<svg viewBox=\"0 0 96 96\"><circle cx=\"48\" cy=\"48\" r=\"36\" fill=\"#e8a87c\"/></svg>\\n',
})
"

# ─── C7: semantic refactor (JSON + TOML) ──────────────────────────
step_python "demo: c7 — semantic refactor: JSON config + Cargo manifest

JSON: max_size 1024 → 4096, search false → true, add 'sync', drop the
endpoints array, add metadata.updated_at. TOML: bump version, add toml
+ sha2 deps, set lto + codegen-units, list authors. The semantic
renderer reports per-jq-path changes; git would just show line diffs." "
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

# ─── C8: release-day combo ────────────────────────────────────────
step_python "demo: c8 — release-day combo

A realistic landing-day commit: banner picks up a sparkline trend +
URL footer, icon gains a final notch dot, press release grows a
Roadmap section, pricing book adds a Limits sheet, the SVG mark gets
the gradient stroke ring, and the CSS bundle gains a print stylesheet.
Every renderer has at least one row to show." "
banner_v4(ROOT/'assets/img/banner.png')
icon_v3(ROOT/'assets/img/icon.png')
dashboard_v2(ROOT/'assets/img/dashboard.png')

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

# tighten the SVG mark — solid stroke ring with a brand-specific colour stop
svg = (ROOT/'assets/vector/logo.svg').read_text()
svg = svg.replace('stroke-width=\"2\"', 'stroke-width=\"3\"')
svg = svg.replace('PNG · OOXML · JSON · TOML',
                  'PNG · OOXML · JSON · TOML · ZIP')
(ROOT/'assets/vector/logo.svg').write_text(svg)

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
