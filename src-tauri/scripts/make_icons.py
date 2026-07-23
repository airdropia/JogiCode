"""Generate JogiCode icons from a source PNG.

If a source icon is available at the project root (jogicode.png) or in
the upload directory, it is used to generate all required Tauri icon
sizes. Otherwise, a solid-color RGBA placeholder is generated.

Tauri requires icon PNGs to have an alpha channel (RGBA, color_type=6).
"""

import struct
import zlib
from pathlib import Path

try:
    from PIL import Image
    HAS_PIL = True
except ImportError:
    HAS_PIL = False

ICON_DIR = Path(__file__).resolve().parent.parent / "icons"
ICON_DIR.mkdir(parents=True, exist_ok=True)

# Source icon search paths (in priority order)
SOURCE_PATHS = [
    Path(__file__).resolve().parent.parent.parent / "jogicode.png",
    Path("/home/z/my-project/upload/jogicode.png"),
    Path(__file__).resolve().parent.parent / "jogicode.png",
]

# Fallback brand color (indigo-violet #6366F1)
FALLBACK_RGB = (0x63, 0x66, 0xF1)


def find_source() -> Path | None:
    for p in SOURCE_PATHS:
        if p.exists():
            return p
    return None


def make_placeholder_png(size: int) -> bytes:
    """Generate a solid-color RGBA placeholder PNG."""
    R, G, B, A = (*FALLBACK_RGB, 0xFF)
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


def write_png(data: bytes, name: str) -> None:
    (ICON_DIR / name).write_bytes(data)


def write_ico(png_256: bytes, name: str) -> None:
    """Build a multi-resolution .ico from a 256x256 PNG."""
    header = struct.pack("<HHH", 0, 1, 1)
    entry = struct.pack(
        "<BBBBHHII",
        0,  # 0 means 256
        0,  # 0 means 256
        0,
        0,
        1,
        32,
        len(png_256),
        22,
    )
    (ICON_DIR / name).write_bytes(header + entry + png_256)


def write_icns(png_128: bytes, name: str) -> None:
    """Build a minimal .icns containing a single 128x128 PNG (ic07 magic)."""
    body = b"ic07" + struct.pack(">I", len(png_128) + 8) + png_128
    (ICON_DIR / name).write_bytes(b"icns" + struct.pack(">I", len(body) + 8) + body)


def main() -> None:
    source = find_source()

    if source and HAS_PIL:
        print(f"Using source icon: {source}")
        img = Image.open(source).convert("RGBA")
        print(f"Source size: {img.size}, mode: {img.mode}")

        # Generate all required sizes
        sizes = {
            "32x32.png": 32,
            "128x128.png": 128,
            "128x128@2x.png": 256,
            "icon.png": 512,
        }

        for name, size in sizes.items():
            resized = img.resize((size, size), Image.LANCZOS)
            buf = resized.tobytes()  # not used directly
            # Save via PIL to get proper RGBA PNG
            out_path = ICON_DIR / name
            resized.save(out_path, format="PNG")
            print(f"  Wrote {name} ({size}x{size})")

        # Generate .ico from 256x256
        img_256 = img.resize((256, 256), Image.LANCZOS)
        import io
        buf = io.BytesIO()
        img_256.save(buf, format="PNG")
        write_ico(buf.getvalue(), "icon.ico")
        print("  Wrote icon.ico (256x256)")

        # Generate .icns from 128x128
        img_128 = img.resize((128, 128), Image.LANCZOS)
        buf = io.BytesIO()
        img_128.save(buf, format="PNG")
        write_icns(buf.getvalue(), "icon.icns")
        print("  Wrote icon.icns (128x128)")

    else:
        if not source:
            print("No source icon found — generating placeholder icons")
        elif not HAS_PIL:
            print("Pillow not available — generating placeholder icons")
        write_png(make_placeholder_png(32), "32x32.png")
        write_png(make_placeholder_png(128), "128x128.png")
        write_png(make_placeholder_png(256), "128x128@2x.png")
        write_png(make_placeholder_png(512), "icon.png")
        write_ico(make_placeholder_png(256), "icon.ico")
        write_icns(make_placeholder_png(128), "icon.icns")

    # Verify all PNGs are RGBA (color_type=6)
    print("\nVerifying RGBA:")
    for f in ICON_DIR.glob("*.png"):
        data = f.read_bytes()
        assert data[:8] == b"\x89PNG\r\n\x1a\n", f"{f} is not a PNG"
        ct = struct.unpack(">IIBB", data[16:16 + 10])[3]
        status = "OK" if ct == 6 else "FAIL"
        print(f"  [{status}] {f.name}: color_type={ct}")

    print(f"\nIcons written to {ICON_DIR}")


if __name__ == "__main__":
    main()
