"""Generate JogiCode icons from a source PNG.

If a source icon is available at the project root (jogicode.png), it is
used to generate all required Tauri icon sizes. Otherwise, a solid-color
RGBA placeholder is generated.

Tauri requires icon PNGs to have an alpha channel (RGBA, color_type=6).
The .ico file must be a proper multi-resolution ICO for Windows desktop
shortcuts to display the correct icon.
"""

import struct
import zlib
from pathlib import Path
from io import BytesIO

try:
    from PIL import Image
    HAS_PIL = True
except ImportError:
    HAS_PIL = False

ICON_DIR = Path(__file__).resolve().parent.parent / "icons"
ICON_DIR.mkdir(parents=True, exist_ok=True)

SOURCE_PATHS = [
    Path(__file__).resolve().parent.parent.parent / "jogicode.png",
    Path("/home/z/my-project/upload/jogicode.png"),
    Path(__file__).resolve().parent.parent / "jogicode.png",
]

FALLBACK_RGB = (0x63, 0x66, 0xF1)


def find_source() -> Path | None:
    for p in SOURCE_PATHS:
        if p.exists():
            return p
    return None


def make_placeholder_png(size: int) -> bytes:
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


def write_multi_res_ico(img: Image.Image, name: str) -> None:
    """Build a proper multi-resolution .ico file with 16, 32, 48, 64, 128, 256 sizes.

    Windows desktop shortcuts require a multi-resolution ICO to display
    the correct icon at all sizes (16x16 for taskbar, 32x32 for desktop,
    48x48 for file explorer, 256x256 for large icons view).
    """
    sizes = [16, 32, 48, 64, 128, 256]
    pngs = []
    for size in sizes:
        resized = img.resize((size, size), Image.LANCZOS)
        buf = BytesIO()
        resized.save(buf, format="PNG")
        pngs.append(buf.getvalue())

    # ICONDIR header: reserved(2)=0, type(2)=1, count(2)=num_images
    header = struct.pack("<HHH", 0, 1, len(sizes))

    # Calculate offset: header(6) + entries(16 each)
    entries_start = 6 + (16 * len(sizes))
    offset = entries_start

    entries = b""
    for i, size in enumerate(sizes):
        png_data = pngs[i]
        # ICONDIRENTRY: width(1), height(1), colors(1)=0, reserved(1)=0,
        # planes(2)=1, bitcount(2)=32, size(4), offset(4)
        w = size if size < 256 else 0
        h = size if size < 256 else 0
        entries += struct.pack(
            "<BBBBHHII",
            w, h, 0, 0, 1, 32, len(png_data), offset
        )
        offset += len(png_data)

    # Combine: header + entries + all PNG data
    ico_data = header + entries
    for png_data in pngs:
        ico_data += png_data

    (ICON_DIR / name).write_bytes(ico_data)


def write_icns(img: Image.Image, name: str) -> None:
    """Build a proper .icns with multiple sizes (32, 128, 256, 512)."""
    # ic07 = 128x128, ic08 = 256x256, ic09 = 512x512, ic10 = 1024x1024
    # icp4 = 32x32, icp5 = 64x64
    icon_types = [
        (32, b"icp4"),
        (128, b"ic07"),
        (256, b"ic08"),
        (512, b"ic09"),
    ]

    body = b""
    for size, magic in icon_types:
        resized = img.resize((size, size), Image.LANCZOS)
        buf = BytesIO()
        resized.save(buf, format="PNG")
        png_data = buf.getvalue()
        # Each icon: magic(4) + size(4) + png_data
        body += magic + struct.pack(">I", len(png_data) + 8) + png_data

    # ICNS header: magic(4) + total_size(4) + body
    (ICON_DIR / name).write_bytes(b"icns" + struct.pack(">I", len(body) + 8) + body)


def main() -> None:
    source = find_source()

    if source and HAS_PIL:
        print(f"Using source icon: {source}")
        img = Image.open(source).convert("RGBA")
        print(f"Source size: {img.size}, mode: {img.mode}")

        sizes = {
            "32x32.png": 32,
            "128x128.png": 128,
            "128x128@2x.png": 256,
            "icon.png": 512,
        }

        for name, size in sizes.items():
            resized = img.resize((size, size), Image.LANCZOS)
            out_path = ICON_DIR / name
            resized.save(out_path, format="PNG")
            print(f"  Wrote {name} ({size}x{size})")

        # Generate proper multi-resolution .ico
        write_multi_res_ico(img, "icon.ico")
        print("  Wrote icon.ico (multi-res: 16,32,48,64,128,256)")

        # Generate proper multi-resolution .icns
        write_icns(img, "icon.icns")
        print("  Wrote icon.icns (multi-res: 32,128,256,512)")

    else:
        if not source:
            print("No source icon found — generating placeholder icons")
        elif not HAS_PIL:
            print("Pillow not available — generating placeholder icons")
        write_png(make_placeholder_png(32), "32x32.png")
        write_png(make_placeholder_png(128), "128x128.png")
        write_png(make_placeholder_png(256), "128x128@2x.png")
        write_png(make_placeholder_png(512), "icon.png")
        # Placeholder single-res ico
        png = make_placeholder_png(256)
        header = struct.pack("<HHH", 0, 1, 1)
        entry = struct.pack("<BBBBHHII", 0, 0, 0, 0, 1, 32, len(png), 22)
        (ICON_DIR / "icon.ico").write_bytes(header + entry + png)
        # Placeholder icns
        png128 = make_placeholder_png(128)
        body = b"ic07" + struct.pack(">I", len(png128) + 8) + png128
        (ICON_DIR / "icon.icns").write_bytes(b"icns" + struct.pack(">I", len(body) + 8) + body)

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
