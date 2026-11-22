#!/usr/bin/env python3
"""Writes the largest image of a .ico file as a PNG (standard library only).

The app icon is drawn in code (`csm_ui::app::app_icon`) and committed as
`apps/codeshell/assets/codeshell.ico` (a test keeps the two in sync). The Linux and macOS
packages need PNGs; the release workflow makes them from the .ico with this script, so there is
no second committed copy to go stale.

    python3 scripts/ico-to-png.py apps/codeshell/assets/codeshell.ico out.png

Handles PNG entries and 32-bit BMP entries (BGRA, bottom-up), which is what the app's .ico holds.
"""

import struct
import sys
import zlib


def largest_entry(data):
    reserved, kind, count = struct.unpack_from("<HHH", data, 0)
    if reserved != 0 or kind != 1 or count == 0:
        raise SystemExit("not an .ico file")
    best = None
    for i in range(count):
        w, h, _c, _r, _planes, bpp, size, offset = struct.unpack_from("<BBBBHHII", data, 6 + 16 * i)
        w, h = w or 256, h or 256
        if best is None or (w * h, bpp) > (best[0] * best[1], best[2]):
            best = (w, h, bpp, size, offset)
    return best


def png_chunk(tag, payload):
    return struct.pack(">I", len(payload)) + tag + payload + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF)


def bmp_to_png(blob):
    header_size, width, height2, _planes, bpp = struct.unpack_from("<IiiHH", blob, 0)
    height = height2 // 2  # an icon's DIB height counts the AND mask too
    if bpp != 32 or width <= 0 or height <= 0:
        raise SystemExit(f"unsupported icon entry: {width}x{height} at {bpp} bpp")
    pixels = blob[header_size:]
    stride = width * 4
    if len(pixels) < stride * height:
        raise SystemExit("truncated icon entry")
    rows = []
    for y in range(height):
        src = pixels[(height - 1 - y) * stride:(height - y) * stride]  # bottom-up
        row = bytearray(stride)
        row[0::4], row[1::4], row[2::4], row[3::4] = src[2::4], src[1::4], src[0::4], src[3::4]
        rows.append(b"\x00" + bytes(row))
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)  # 8-bit RGBA
    return b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", ihdr) + png_chunk(b"IDAT", zlib.compress(b"".join(rows), 9)) + png_chunk(b"IEND", b"")


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    with open(sys.argv[1], "rb") as f:
        data = f.read()
    w, h, _bpp, size, offset = largest_entry(data)
    blob = data[offset:offset + size]
    png = blob if blob.startswith(b"\x89PNG") else bmp_to_png(blob)
    with open(sys.argv[2], "wb") as f:
        f.write(png)
    print(f"{sys.argv[2]}: {w}x{h}")


if __name__ == "__main__":
    main()
