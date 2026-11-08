#!/usr/bin/env bash
# Regenerate every raster icon from the SVG sources in assets/brand/:
#
#   assets/brand/icon.svg           1024 px master, used for sizes above 64 px
#   assets/brand/icon-small.svg     simplified artwork, used for sizes up to 64 px
#   assets/brand/tray-*.svg         menu bar / tray silhouettes
#
# Output (committed, so a normal build needs none of the tools below):
#
#   src-tauri/icons/                the bundle icons (icon.icns, icon.ico, icon.png, ...)
#   src-tauri/icons/tray/           tray PNGs, 44 px (@2x; the OS scales them down)
#
# Tools: the Tauri CLI from ui/node_modules (`npm --prefix ui ci`) rasterizes the SVGs (it
# embeds resvg) and python3 packs icon.icns and icon.ico. It is deterministic for a given Tauri CLI
# version (the embedded resvg): running it again leaves `git status` unchanged.
#
#   scripts/gen-icons.sh
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

brand=assets/brand
icons=src-tauri/icons
tauri=ui/node_modules/.bin/tauri

fail() { echo "error: $*" >&2; exit 1; }

[ -x "$tauri" ] || fail "the Tauri CLI is missing: run 'npm --prefix ui ci' first"
command -v python3 >/dev/null || fail "python3 is required to pack icon.icns and icon.ico"
for svg in icon icon-small tray-template tray-recording tray-paused; do
  [ -f "$brand/$svg.svg" ] || fail "missing $brand/$svg.svg"
done

work="$(mktemp -d "${TMPDIR:-/tmp}/gen-icons.XXXXXX")"
trap 'rm -rf "$work"' EXIT

# render <svg> <out-dir> <size>[,<size>...]: writes <out-dir>/<size>x<size>.png
render() {
  mkdir -p "$2"
  "$tauri" icon "$1" --output "$2" --png "$3" >"$work/render.log" 2>&1 \
    || { cat "$work/render.log" >&2; fail "cannot rasterize $1"; }
}

# Sizes above 64 px come from the detailed master, up to 64 px from the simplified artwork.
render "$brand/icon.svg" "$work/large" 71,89,107,128,142,150,256,284,310,512,1024
render "$brand/icon-small.svg" "$work/small" 16,24,30,32,44,48,50,64
large() { echo "$work/large/${1}x${1}.png"; }
small() { echo "$work/small/${1}x${1}.png"; }

mkdir -p "$icons/tray"

# Bundle PNGs.
cp "$(small 32)"  "$icons/32x32.png"
cp "$(small 64)"  "$icons/64x64.png"
cp "$(large 128)" "$icons/128x128.png"
cp "$(large 256)" "$icons/128x128@2x.png"
cp "$(large 512)" "$icons/icon.png"
cp "$(small 30)"  "$icons/Square30x30Logo.png"
cp "$(small 44)"  "$icons/Square44x44Logo.png"
cp "$(large 71)"  "$icons/Square71x71Logo.png"
cp "$(large 89)"  "$icons/Square89x89Logo.png"
cp "$(large 107)" "$icons/Square107x107Logo.png"
cp "$(large 142)" "$icons/Square142x142Logo.png"
cp "$(large 150)" "$icons/Square150x150Logo.png"
cp "$(large 284)" "$icons/Square284x284Logo.png"
cp "$(large 310)" "$icons/Square310x310Logo.png"
cp "$(small 50)"  "$icons/StoreLogo.png"

# icon.icns: PNG-compressed entries. 16 px and 32 px (1x and @2x) are simplified artwork.
python3 -I - "$icons/icon.icns" \
  icp4 "$(small 16)" icp5 "$(small 32)" ic11 "$(small 32)" ic12 "$(small 64)" \
  ic07 "$(large 128)" ic13 "$(large 256)" ic08 "$(large 256)" ic14 "$(large 512)" \
  ic09 "$(large 512)" ic10 "$(large 1024)" <<'PY'
import struct
import sys

out, *args = sys.argv[1:]
chunks = b""
for kind, path in zip(args[0::2], args[1::2]):
    with open(path, "rb") as handle:
        data = handle.read()
    chunks += kind.encode("ascii") + struct.pack(">I", len(data) + 8) + data

with open(out, "wb") as handle:
    handle.write(b"icns" + struct.pack(">I", len(chunks) + 8) + chunks)
PY

# icon.ico: PNG-compressed entries (16, 24, 32, 48, 64 simplified; 256 detailed).
python3 -I - "$icons/icon.ico" \
  "$(small 16)" "$(small 24)" "$(small 32)" "$(small 48)" "$(small 64)" "$(large 256)" <<'PY'
import struct
import sys

out, *paths = sys.argv[1:]
images = []
for path in paths:
    with open(path, "rb") as handle:
        data = handle.read()
    # PNG IHDR: width and height are the big-endian u32s at offsets 16 and 20.
    width, height = struct.unpack(">II", data[16:24])
    images.append((width, height, data))

header = struct.pack("<HHH", 0, 1, len(images))
offset = len(header) + 16 * len(images)
entries = b""
for width, height, data in images:
    # A size of 256 is stored as 0 in the directory entry.
    entries += struct.pack(
        "<BBBBHHII", width % 256, height % 256, 0, 0, 1, 32, len(data), offset
    )
    offset += len(data)

with open(out, "wb") as handle:
    handle.write(header + entries + b"".join(data for _, _, data in images))
PY

# Tray PNGs at 44 px (@2x). The `-template` variants are black on transparent
# and are used as macOS template images; the others are coloured for Windows and Linux, where
# the OS does not recolour the icon. The recording silhouette is amber everywhere.
tray() { # tray <svg> <name>
  local dir="$work/tray-$2"
  render "$1" "$dir" 44
  cp "$dir/44x44.png" "$icons/tray/$2@2x.png"
}
# The silhouettes take their colour from `color` on the root element (fill="currentColor").
tint() {
  sed 's/style="color:#000"/style="color:#675ade"/' "$1" >"$2"
  grep -q 'style="color:#675ade"' "$2" || fail "cannot tint $1: no style=\"color:#000\" on its root"
}

tint "$brand/tray-template.svg" "$work/tray-idle-color.svg"
tint "$brand/tray-paused.svg" "$work/tray-paused-color.svg"

tray "$brand/tray-template.svg" idle-template
tray "$brand/tray-recording.svg" recording
tray "$brand/tray-paused.svg" paused-template
tray "$work/tray-idle-color.svg" idle-color
tray "$work/tray-paused-color.svg" paused-color

echo "icons written to $icons"
