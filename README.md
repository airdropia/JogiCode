# JogiCode

**Lightweight Windows IDE powered by [code-server](https://github.com/coder/code-server).**

JogiCode wraps the VS Code-powered code-server in a native Windows desktop
application using Tauri v2. It runs code-server silently on `127.0.0.1:8080`
and loads it in a native WebView — no browser needed, no separate terminal.

## Architecture

```
JogiCode.exe (Tauri native window)
  ├── Rust main process
  │     ├── Spawns code-server via bundled node.exe (no console window)
  │     ├── Polls 127.0.0.1:8080 until code-server is ready
  │     ├── Navigates the WebView to http://127.0.0.1:8080
  │     └── Kills code-server process when the window closes
  │
  ├── app/index.html (splash screen — shown while code-server starts)
  │
  └── Bundled sidecar (in resource dir)
        ├── binaries/node.exe (Node.js portable Windows x64)
        └── binaries/code-server/node_modules/code-server/ (npm package)
              └── out/node/entry.js (entry point)
```

### Why bundle Node.js + code-server npm?

code-server does not publish a standalone Windows binary. The CI workflow
downloads the official Node.js portable Windows x64 build and installs the
code-server npm package, then bundles both as Tauri resources. Rust spawns
`node.exe entry.js` at runtime — no system Node.js installation required.

### Why not just use a browser?

- No browser tab/URL bar — feels like a native app
- Automatic startup — double-click JogiCode.exe and the IDE opens
- Clean process lifecycle — closing the window kills code-server
- No port conflicts — code-server only runs while JogiCode is open

## Portable mode (zero footprint)

JogiCode runs as a true portable "green" app. When the exe is placed in a
writable folder (e.g. extracted from the portable ZIP), ALL runtime data lives
in a `data` folder next to the exe:

```
JogiCode/
├── JogiCode.exe
├── binaries/             (bundled Node.js + code-server sidecar)
└── data/                 (created on first run)
    ├── userdata/         (VS Code settings, workspace state)
    ├── extensions/       (installed extensions, e.g. Kilo Code)
    ├── webview/          (WebView2 profile)
    ├── home/             (Kilo Code config: home\.config\kilo, etc.)
    ├── appdata/          (redirected APPDATA for code-server's child process)
    └── tmp/              (temp files)
```

Nothing is written to `%APPDATA%`, `%LOCALAPPDATA%`, `ProgramData`, or the
registry during normal use. To uninstall, just delete the folder.

- On first run, an existing code-server setup (`%LOCALAPPDATA%\code-server`)
  and Kilo Code config (`~\.config\kilo`) are migrated into `data` automatically.
- SSH keys are **never** copied automatically. If you push to git from inside
  JogiCode, copy your `~/.ssh` folder into `data\home\.ssh` first.
- Installed mode (NSIS installer, Program Files) falls back to
  `%APPDATA%\JogiCode` because Program Files is not writable. The app works the
  same either way.

## Build

The GitHub Actions workflow (`.github/workflows/build-windows.yml`)
automatically:

1. Downloads the official Node.js portable Windows x64 binary
2. Installs code-server npm package (pinned version)
3. Compiles the Tauri Rust shell
4. Produces `JogiCode_x.y.z_x64-setup.exe` (NSIS installer) and
   `JogiCode_x.y.z-portable-x64.zip` (portable build — extract and run)

Push a `v*` tag to trigger a release build:

```bash
git tag v1.0.0
git push origin v1.0.0
```

## Tech Stack

| Component | Technology |
|-----------|-----------|
| Desktop shell | Tauri v2 (Rust) |
| IDE backend | code-server v4.129.0 (npm package) |
| JS runtime | Node.js portable (bundled, not system-installed) |
| Frontend | Static HTML splash → code-server WebView |
| Installer | NSIS (.exe) |
| Target | Windows x86_64 only |

## Memory notes

The code-server Node.js heap is capped at 384MB (`NODE_OPTIONS` in
`src-tauri/src/lib.rs`) and the WebView2 V8 heap at 256MB. These are already
aggressive; raise the 384 to 512 only if you see extension-host
out-of-memory errors in `data\jogicode.log`.
