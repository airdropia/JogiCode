"""Generate minimal placeholder RGBA icons for JogiCode.

Tauri requires icon PNGs to have an alpha channel (RGBA, color_type=6).
This script produces simple solid-color RGBA PNG/ICO/ICNS placeholders.
Replace with real brand icons before public release.
"""

import struct
import zlib
from pathlib import Path

ICON_DIR = Path(__file__).resolve().parent.parent / "icons"
ICON_DIR.mkdir(parents=True, exist_ok=True)

# JogiCode brand color: indigo-violet gradient base (#6366F1)
R, G, B, A = (0x63, 0x66, 0xF1, 0xFF)


def make_png(size: int) -> bytes:
    sig = b"\x89PNG\r\n\x1a\n"

    def chunk(name: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + name
            + data
            + struct.pack(">I", zlib.crc32(name + data) & 0xFFFFFFFF)
        )

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    raw = b"".join(b"\x00" + bytes((R, G, B, A)) * size for _ in range(size))
    idat = zlib.compress(raw, 9)
    return sig + chunk(b"IHDR", ihdr) + chunk(b"IDAT", idat) + chunk(b"IEND", b"")


def write_png(name: str, size: int) -> None:
    (ICON_DIR / name).write_bytes(make_png(size))


def write_ico(name: str, size: int) -> None:
    png = make_png(size)
    header = struct.pack("<HHH", 0, 1, 1)
    entry = struct.pack(
        "<BBBBHHII",
        size if size < 256 else 0,
        size if size < 256 else 0,
        0,
        0,
        1,
        32,
        len(png),
        22,
    )
    (ICON_DIR / name).write_bytes(header + entry + png)


def write_icns(name: str) -> None:
    png = make_png(128)
    body = b"ic07" + struct.pack(">I", len(png) + 8) + png
    (ICON_DIR / name).write_bytes(b"icns" + struct.pack(">I", len(body) + 8) + body)


def main() -> None:
    write_png("32x32.png", 32)
    write_png("128x128.png", 128)
    write_png("128x128@2x.png", 256)
    write_png("icon.png", 512)
    write_ico("icon.ico", 256)
    write_icns("icon.icns")
    print("Wrote RGBA icons to", ICON_DIR)


if __name__ == "__main__":
    main()
