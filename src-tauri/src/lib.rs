use std::fs::OpenOptions;
use std::io::{BufWriter, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Manager;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Windows process creation flag that prevents a console/CMD window
/// from appearing when spawning child processes from a GUI app.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Redirect code-server's APPDATA into the JogiCode data dir (portable mode).
///
/// Some native modules (e.g. `@vscode/deviceid`) read APPDATA at startup to
/// fingerprint the machine. Redirecting it keeps the app fully self-contained.
/// If any extension misbehaves because of this, set this to `false` and rebuild
/// — everything else (USERPROFILE/HOME/TEMP) stays redirected either way.
const REDIRECT_APPDATA: bool = true;

/// TCP port polling timeout (seconds).
const TCP_POLL_TIMEOUT_SECS: u64 = 60;
/// HTTP health check timeout after TCP port opens (seconds).
const HTTP_HEALTH_TIMEOUT_SECS: u64 = 30;

/// Thread-safe log writer. Uses Box<dyn Write + Send> so it can hold
/// either a real File or a no-op Sink as a fallback.
type LogFile = Arc<Mutex<BufWriter<Box<dyn Write + Send>>>>;

/// Write a timestamped log line to both console and the log file.
fn log_line(log: &LogFile, msg: &str) {
    let line = format!("[jogicode] {}", msg);
    println!("{}", line);
    if let Ok(mut writer) = log.lock() {
        let _ = writeln!(writer, "{}", line);
        let _ = writer.flush();
    }
}

/// Find a free TCP port on 127.0.0.1.
///
/// Binds a TcpListener to port 0 (which tells the OS to assign a free
/// port), reads the assigned port, then drops the listener.
///
/// This fixes the `EACCES: permission denied` error that occurs when
/// Windows (Hyper-V, WSL2, Docker) dynamically reserves port ranges
/// that include hardcoded ports like 8080. After a reboot or Hyper-V
/// restart, previously-working ports can become reserved.
///
/// There is a tiny race window between dropping the listener and
/// code-server binding to it, but in practice this is microseconds
/// and the OS won't reassign the port that quickly.
fn find_free_port(log: &LogFile) -> Result<u16, String> {
    // Try preferred ports first (8080, 8081, 8082) for familiarity.
    // If none are available, let the OS pick a random port.
    let preferred = [8080u16, 8081, 8082, 8083, 8084, 8085];

    for &port in &preferred {
        match TcpListener::bind(format!("127.0.0.1:{}", port)) {
            Ok(listener) => {
                log_line(log, &format!("found free preferred port: {}", port));
                drop(listener);
                return Ok(port);
            }
            Err(e) => {
                log_line(log, &format!("preferred port {} not available: {}", port, e));
            }
        }
    }

    // Fall back to OS-assigned random port.
    match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => {
            let port = listener
                .local_addr()
                .map_err(|e| format!("failed to get local addr: {}", e))?
                .port();
            log_line(log, &format!("OS assigned random port: {}", port));
            drop(listener);
            Ok(port)
        }
        Err(e) => Err(format!("failed to bind to any port: {}", e)),
    }
}

/// Poll 127.0.0.1:PORT until a TCP connection succeeds or timeout.
fn wait_for_tcp(port: u16, timeout_secs: u64, log: &LogFile) -> bool {
    let addr = format!("127.0.0.1:{}", port);
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    while start.elapsed() < timeout {
        match TcpStream::connect_timeout(
            &addr.parse().expect("invalid addr"),
            Duration::from_secs(1),
        ) {
            Ok(_) => {
                log_line(log, &format!("TCP port {} is open", port));
                return true;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(500)),
        }
    }
    false
}

/// HTTP health check: send a GET / request and check for any HTTP response.
/// Returns true if the server responds with an HTTP status line.
fn http_health_check(port: u16, timeout_secs: u64, log: &LogFile) -> bool {
    let addr = format!("127.0.0.1:{}", port);
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    while start.elapsed() < timeout {
        match TcpStream::connect_timeout(
            &addr.parse().expect("invalid addr"),
            Duration::from_secs(2),
        ) {
            Ok(mut stream) => {
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let request = format!(
                    "GET / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
                    port
                );
                if stream.write_all(request.as_bytes()).is_ok() {
                    let mut buf = [0u8; 256];
                    match stream.read(&mut buf) {
                        Ok(n) if n > 0 => {
                            let response = String::from_utf8_lossy(&buf[..n]);
                            if response.starts_with("HTTP/") {
                                log_line(log, "HTTP health check passed — code-server is responding");
                                return true;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(1000));
    }
    log_line(log, "HTTP health check timed out");
    false
}

/// Recursively search for a file matching `name` under `dir`.
fn find_file(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let found @ Some(_) = find_file(&path, name) {
                return found;
            }
        } else if path.file_name().map(|n| n == name).unwrap_or(false) {
            return Some(path);
        }
    }
    None
}

/// Strip the Windows UNC `\\?\` prefix from a path.
fn strip_unc_prefix(path: &std::path::Path) -> std::path::PathBuf {
    let s = path.to_string_lossy();
    if s.starts_with(r"\\?\") {
        let stripped = &s[4..];
        std::path::PathBuf::from(stripped)
    } else if s.starts_with(r"\\?\UNC\") {
        let stripped = &s[7..];
        std::path::PathBuf::from(format!(r"\\{}", stripped))
    } else {
        path.to_path_buf()
    }
}

/// Resolve the path to the bundled node.exe and code-server entry point.
fn resolve_sidecar_paths(
    app: &tauri::App,
    log: &LogFile,
) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let resource_dir = app
        .path()
        .resource_dir()
        .map_err(|e| format!("failed to resolve resource dir: {}", e))?;

    log_line(log, &format!("resource_dir: {:?}", resource_dir));

    let binaries_dir = resource_dir.join("binaries");

    if let Ok(entries) = std::fs::read_dir(&binaries_dir) {
        let contents: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        log_line(log, &format!("binaries/ contents: {:?}", contents));
    }

    let node_exe = binaries_dir.join("node.exe");
    log_line(log, &format!("node.exe path: {:?}", node_exe));
    if !node_exe.exists() {
        return Err(format!("node.exe not found at {:?}", node_exe));
    }
    let node_exe = node_exe
        .canonicalize()
        .map_err(|e| format!("failed to canonicalize node.exe path: {}", e))?;
    let node_exe = strip_unc_prefix(&node_exe);
    log_line(log, &format!("node.exe canonical (UNC stripped): {:?}", node_exe));

    let cs_base = binaries_dir.join("code-server");
    let candidate_paths = [
        cs_base
            .join("node_modules")
            .join("code-server")
            .join("out")
            .join("node")
            .join("entry.js"),
        cs_base.join("out").join("node").join("entry.js"),
    ];

    let mut cs_entry = None;
    for path in &candidate_paths {
        log_line(log, &format!("checking entry.js at: {:?}", path));
        if path.exists() {
            cs_entry = Some(path.clone());
            log_line(log, &format!("found entry.js at: {:?}", path));
            break;
        }
    }

    if cs_entry.is_none() {
        log_line(log, "entry.js not found at expected paths, searching recursively...");
        if let Some(found) = find_file(&cs_base, "entry.js") {
            log_line(log, &format!("found entry.js via search: {:?}", found));
            cs_entry = Some(found);
        }
    }

    let cs_entry = cs_entry.ok_or_else(|| {
        format!(
            "code-server entry.js not found under {:?}. \
             Check that the code-server npm package was installed correctly.",
            cs_base
        )
    })?;

    let cs_entry = cs_entry
        .canonicalize()
        .map_err(|e| format!("failed to canonicalize entry.js path: {}", e))?;
    let cs_entry = strip_unc_prefix(&cs_entry);
    log_line(log, &format!("entry.js canonical (UNC stripped): {:?}", cs_entry));

    Ok((node_exe, cs_entry))
}

/// Recursively copy a directory and its contents.
/// Skips files that already exist in the destination (no overwrite).
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path, log: &LogFile) -> Result<u64, String> {
    if !src.is_dir() {
        return Err(format!("source is not a directory: {:?}", src));
    }
    std::fs::create_dir_all(dst)
        .map_err(|e| format!("failed to create dst dir {:?}: {}", dst, e))?;

    let mut copied: u64 = 0;
    for entry in std::fs::read_dir(src)
        .map_err(|e| format!("failed to read src dir {:?}: {}", src, e))?
        .flatten()
    {
        let src_path = entry.path();
        let file_name = entry.file_name();
        let dst_path = dst.join(&file_name);

        if src_path.is_dir() {
            // Recurse into subdirectory
            match copy_dir_recursive(&src_path, &dst_path, log) {
                Ok(n) => copied += n,
                Err(e) => {
                    log_line(log, &format!("  skip subdir {:?}: {}", src_path, e));
                }
            }
        } else {
            // Copy file if it doesn't already exist in destination
            if !dst_path.exists() {
                if let Err(e) = std::fs::copy(&src_path, &dst_path) {
                    log_line(log, &format!("  skip file {:?}: {}", src_path, e));
                } else {
                    copied += 1;
                }
            }
        }
    }
    Ok(copied)
}

/// Migrate data from old code-server default locations to the new
/// %APPDATA%\JogiCode\ location.
///
/// Before v1.0.7, JogiCode used code-server's default paths:
///   %LOCALAPPDATA%\code-server\Data\          (user data)
///   %USERPROFILE%\.vscode\extensions\         (extensions, maybe)
///
/// Starting v1.0.7, all data goes to:
///   %APPDATA%\JogiCode\userdata\
///   %APPDATA%\JogiCode\extensions\
///
/// This function checks if the old locations have data and the new
/// location is empty (first migration). If so, it copies the old data
/// to the new location so users don't lose their settings, extensions
/// (like Kilo Code), and workspace state.
fn migrate_old_data(
    new_userdata_dir: &std::path::Path,
    new_extensions_dir: &std::path::Path,
    log: &LogFile,
) {
    use std::env;

    // ── Old user-data-dir locations ──
    // code-server defaults to %LOCALAPPDATA%\code-server\Data
    let old_userdata_candidates: Vec<std::path::PathBuf> = {
        let mut v = Vec::new();
        if let Ok(local_appdata) = env::var("LOCALAPPDATA") {
            v.push(std::path::PathBuf::from(&local_appdata).join("code-server").join("Data"));
            // Some code-server versions use this path
            v.push(std::path::PathBuf::from(&local_appdata).join("code-server"));
        }
        v
    };

    // ── Old extensions locations ──
    // code-server may store extensions in the user-data-dir/Extensions
    // or in %USERPROFILE%\.vscode\extensions
    let old_extensions_candidates: Vec<std::path::PathBuf> = {
        let mut v = Vec::new();
        if let Ok(local_appdata) = env::var("LOCALAPPDATA") {
            v.push(
                std::path::PathBuf::from(&local_appdata)
                    .join("code-server")
                    .join("Data")
                    .join("extensions"),
            );
            v.push(
                std::path::PathBuf::from(&local_appdata)
                    .join("code-server")
                    .join("extensions"),
            );
        }
        if let Ok(userprofile) = env::var("USERPROFILE") {
            v.push(std::path::PathBuf::from(&userprofile).join(".vscode").join("extensions"));
        }
        v
    };

    // ── Migrate user data if new location is empty ──
    let new_userdata_is_empty = std::fs::read_dir(new_userdata_dir)
        .map(|mut d| d.next().is_none())
        .unwrap_or(true);

    if new_userdata_is_empty {
        log_line(log, "checking for old code-server user data to migrate...");
        for old_dir in &old_userdata_candidates {
            if old_dir.is_dir() {
                log_line(log, &format!("found old user data at {:?}", old_dir));
                match copy_dir_recursive(old_dir, new_userdata_dir, log) {
                    Ok(count) => {
                        log_line(log, &format!("migrated {} files from {:?} to {:?}", count, old_dir, new_userdata_dir));
                        break;
                    }
                    Err(e) => {
                        log_line(log, &format!("migration of {:?} failed: {}", old_dir, e));
                    }
                }
            }
        }
    } else {
        log_line(log, "new user data dir is not empty, skipping migration");
    }

    // ── Migrate extensions if new location is empty ──
    let new_extensions_is_empty = std::fs::read_dir(new_extensions_dir)
        .map(|mut d| d.next().is_none())
        .unwrap_or(true);

    if new_extensions_is_empty {
        log_line(log, "checking for old code-server extensions to migrate...");
        for old_dir in &old_extensions_candidates {
            if old_dir.is_dir() {
                log_line(log, &format!("found old extensions at {:?}", old_dir));
                match copy_dir_recursive(old_dir, new_extensions_dir, log) {
                    Ok(count) => {
                        log_line(log, &format!("migrated {} extension files from {:?} to {:?}", count, old_dir, new_extensions_dir));
                        break;
                    }
                    Err(e) => {
                        log_line(log, &format!("migration of extensions {:?} failed: {}", old_dir, e));
                    }
                }
            }
        }
    } else {
        log_line(log, "new extensions dir is not empty, skipping migration");
    }
}

/// Resolve where JogiCode keeps all runtime data.
///
/// Portable mode (default): a `data` folder next to the executable, so the app
/// is fully self-contained — nothing is written to AppData. Used when that
/// folder is writable (e.g. extracted from the portable ZIP).
///
/// Installed mode (fallback): `%APPDATA%\JogiCode` — used when the exe lives in
/// a read-only location (Program Files via NSIS), where a sibling `data` folder
/// cannot be created.
fn resolve_data_dir(app: &tauri::App) -> Result<std::path::PathBuf, String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let candidate = exe_dir.join("data");
            if candidate.is_dir() || std::fs::create_dir_all(&candidate).is_ok() {
                return Ok(candidate);
            }
        }
    }
    app.path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve app_data_dir: {}", e))
}

/// Migrate an existing Kilo Code config (`~/.config/kilo`) into
/// `<data>\home\.config\kilo` on the first portable run, so the user keeps
/// their Kilo setup and API key. SSH keys are never copied automatically.
fn migrate_kilo_config(data_dir: &std::path::Path, log: &LogFile) {
    let dest = data_dir.join("home").join(".config").join("kilo");
    if dest.exists() {
        log_line(log, "kilo config already present in data/home, skipping migration");
        return;
    }

    let mut src: Option<std::path::PathBuf> = None;
    if let Ok(up) = std::env::var("USERPROFILE") {
        let p = std::path::PathBuf::from(up).join(".config").join("kilo");
        if p.is_dir() {
            src = Some(p);
        }
    }
    if src.is_none() {
        if let Ok(home) = std::env::var("HOME") {
            let p = std::path::PathBuf::from(home).join(".config").join("kilo");
            if p.is_dir() {
                src = Some(p);
            }
        }
    }

    if let Some(src) = src {
        log_line(log, &format!("found kilocode config at {:?}, migrating to data/home", src));
        match copy_dir_recursive(&src, &dest, log) {
            Ok(n) => log_line(log, &format!("migrated {} kilocode config files to {:?}", n, dest)),
            Err(e) => log_line(log, &format!("kilocode config migration failed: {}", e)),
        }
    } else {
        log_line(log, "no existing kilocode config found to migrate");
    }
}

/// Ensure the JogiCode data directory exists and create a default settings.json
/// that configures VS Code for workspace-local caching and clipboard support.
fn ensure_data_dir(
    data_dir: &std::path::Path,
    log: &LogFile,
) -> Result<(), String> {
    // All JogiCode data lives under the resolved data dir (portable: next to the
    // exe; installed: %APPDATA%\JogiCode\). This keeps everything in one clean
    // place instead of scattered across %LOCALAPPDATA%\code-server\, etc.
    let userdata_dir = data_dir.join("userdata");
    let extensions_dir = data_dir.join("extensions");
    let user_dir = userdata_dir.join("User");

    // Create all required directories
    std::fs::create_dir_all(&userdata_dir)
        .map_err(|e| format!("failed to create userdata dir: {}", e))?;
    std::fs::create_dir_all(&extensions_dir)
        .map_err(|e| format!("failed to create extensions dir: {}", e))?;
    std::fs::create_dir_all(&user_dir)
        .map_err(|e| format!("failed to create User dir: {}", e))?;

    log_line(log, &format!("data_dir: {:?}", data_dir));
    log_line(log, &format!("userdata_dir: {:?}", userdata_dir));
    log_line(log, &format!("extensions_dir: {:?}", extensions_dir));

    // ── Migrate old data BEFORE creating default settings ──
    // This copies old code-server data (including installed extensions
    // like Kilo Code) to the new JogiCode location, and migrates any existing
    // Kilo Code config (~/.config/kilo) into data/home.
    migrate_old_data(&userdata_dir, &extensions_dir, log);
    migrate_kilo_config(data_dir, log);

    // Create default settings.json if it doesn't exist.
    // This configures VS Code for aggressive memory optimization:
    // - <600MB RAM ceiling target
    // - Minimal file watching & indexing
    // - Workspace-local caching
    // - Disabled telemetry/updates/auto-save
    let settings_path = user_dir.join("settings.json");
    if !settings_path.exists() {
        let default_settings = r#"{
    // ════════════════════════════════════════════════════════════════════
    // JogiCode Default Settings — Memory Optimized (<600MB target)
    // ════════════════════════════════════════════════════════════════════

    // ── MEMORY CEILING: V8 / Node.js heap limits ──
    // These are read by code-server's Node.js process to cap V8 heap.
    // 384MB heap + ~100MB native = ~500MB per extension host.
    "terminal.integrated.env.windows": {
        "NODE_OPTIONS": "--max-old-space-size=384"
    },

    // ── MEMORY: Editor & Text Buffer ──
    // Reduce undo stack and render buffer memory
    "editor.maxTokenizationLineLength": 20000,
    "editor.largeFileOptimizations": true,
    "editor.semanticHighlighting.enabled": false,
    "editor.bracketPairColorization.enabled": false,
    "editor.guides.bracketPairs": false,
    "editor.unicodeHighlight.ambiguousCharacters": false,
    "editor.unicodeHighlight.invisibleCharacters": false,
    "editor.unlinkOnSave": false,
    "editor.suggestSelection": "first",
    "editor.wordBasedSuggestions": "off",
    "editor.quickSuggestions": { "other": false, "comments": false, "strings": false },

    // ── MEMORY: Disable expensive features ──
    "editor.minimap.enabled": false,
    "editor.gotoLocation.multiple": "goto",
    "editor.codeLens": false,
    "editor.inlayHints.enabled": "off",
    "editor.stickyScroll.enabled": false,
    "editor.linkedEditing": false,
    "editor.dragAndDrop": false,
    "editor.suggest.showStatusBar": false,
    "editor.suggest.preview": false,
    "editor.suggest.insertMode": "replace",
    "editor.hover.delay": 500,
    "editor.parameterHints.enabled": false,

    // ── FILES: Aggressive watcher exclusion (prevents 35+ process spawn) ──
    "files.watcherExclude": {
        "**/.git/objects/**": true,
        "**/.git/subtree-cache/**": true,
        "**/node_modules/**": true,
        "**/.jogicode/**": true,
        "**/.tmp/**": true,
        "**/.cache/**": true,
        "**/tmp/**": true,
        "**/temp/**": true,
        "**/dist/**": true,
        "**/build/**": true,
        "**/.next/**": true,
        "**/.nuxt/**": true,
        "**/coverage/**": true,
        "**/.vscode/**": true,
        "**/out/**": true,
        "**/__pycache__/**": true,
        "**/.venv/**": true,
        "**/venv/**": true,
        "**/.idea/**": true,
        "**/target/**": true
    },
    "files.exclude": {
        "**/.jogicode": true,
        "**/.tmp": true,
        "**/.cache": true
    },
    "files.hotExit": "off",
    "files.autoSave": "off",

    // ── SEARCH: Limit memory & CPU ──
    "search.exclude": {
        "**/node_modules": true,
        "**/.jogicode": true,
        "**/.tmp": true,
        "**/dist": true,
        "**/build": true,
        "**/.next": true,
        "**/.nuxt": true,
        "**/coverage": true,
        "**/out": true,
        "**/__pycache__": true,
        "**/.venv": true,
        "**/target": true
    },
    "search.followSymlinks": false,
    "search.useReplacePreview": false,
    "search.smartCase": true,
    "search.maxResults": 1000,

    // ── TERMINAL: Minimal memory ──
    "terminal.integrated.scrollback": 1000,
    "terminal.integrated.enablePersistentSessions": false,
    "terminal.integrated.gpuAcceleration": "off",
    "terminal.integrated.windowsEnableConpty": false,

    // ── TYPESCRIPT: Reduce language server memory ──
    "typescript.tsdk": null,
    "typescript.enablePromptUseWorkspaceTsdk": true,
    "typescript.tsserver.log": "off",
    "typescript.tsserver.maxTsServerMemory": 256,
    "typescript.updateImportsOnFileMove.enabled": "never",
    "typescript.suggestionActions.enabled": false,

    // ── GIT: Minimal overhead ──
    "git.enabled": true,
    "git.autorefresh": false,
    "git.confirmSync": false,
    "git.fetchOnPull": false,
    "git.enableSmartCommit": false,
    "git.decorations.enabled": false,
    "git.ignoreMissingGitWarning": true,

    // ── EXTENSIONS: Disable auto-anything ──
    "extensions.autoUpdate": false,
    "extensions.autoCheckUpdates": false,
    "extensions.autoFetch": false,
    "extensions.ignoreRecommendations": true,
    "extensions.showRecommendationsOnlyOnDemand": true,

    // ── TELEMETRY & UPDATES: Off ──
    "telemetry.telemetryLevel": "off",
    "redhat.telemetry.enabled": false,
    "update.mode": "none",
    "update.showReleaseNotes": false,

    // ── WORKBENCH: Minimal UI overhead ──
    "workbench.startupEditor": "none",
    "workbench.colorTheme": "Default Dark+",
    "workbench.iconTheme": "vs-minimal",
    "workbench.activityBar.visible": true,
    "workbench.statusBar.visible": true,
    "workbench.sideBar.location": "left",
    "workbench.editor.enablePreview": true,
    "workbench.editor.enablePreviewFromQuickOpen": true,
    "workbench.editor.closeEmptyGroups": true,
    "workbench.editor.tabCloseButton": "left",
    "workbench.list.openMode": "doubleClick",
    "workbench.tips.enabled": false,
    "workbench.settings.enableNaturalLanguageSearch": false,

    // ── EDITOR: Core settings ──
    "editor.fontSize": 14,
    "editor.tabSize": 2,
    "editor.formatOnSave": false,
    "editor.formatOnPaste": false,
    "editor.formatOnType": false,
    "editor.renderWhitespace": "none",
    "editor.renderControlCharacters": false,
    "editor.renderLineHighlight": "line",
    "editor.cursorBlinking": "smooth",
    "editor.cursorSmoothCaretAnimation": "off",
    "editor.smoothScrolling": false,
    "editor.scrollBeyondLastLine": false,
    "editor.mouseWheelScrollSensitivity": 1,
    "editor.multiCursorModifier": "alt",
    "editor.copyWithSyntaxHighlighting": true,

    // ── WINDOW ──
    "window.menuBarVisibility": "visible",
    "window.title": "${dirty}${activeEditorShort}${separator}${rootName}",
    "window.restoreFullscreen": false,
    "window.newWindowDimensions": "maximized"
}"#;
        std::fs::write(&settings_path, default_settings)
            .map_err(|e| format!("failed to write settings.json: {}", e))?;
        log_line(log, "created default settings.json (memory optimized)");
    }

    // Create a default keybindings.json that adds right-click paste support
    let keybindings_path = user_dir.join("keybindings.json");
    if !keybindings_path.exists() {
        let default_keybindings = r#"[
    // Right-click paste: map middle-click to paste for accessibility
    {
        "key": "ctrl+shift+v",
        "command": "editor.action.clipboardPasteAction"
    }
]"#;
        std::fs::write(&keybindings_path, default_keybindings)
            .map_err(|e| format!("failed to write keybindings.json: {}", e))?;
        log_line(log, "created default keybindings.json");
    }

    Ok(())
}

/// Spawn code-server on the given port.
/// All data (settings, extensions, workspace state) goes to
/// %APPDATA%\JogiCode\ instead of code-server's default locations.
fn spawn_code_server(
    app: &tauri::App,
    log: &LogFile,
    log_path: &std::path::Path,
    port: u16,
    data_dir: &std::path::Path,
) -> Result<Child, String> {
    let (node_exe, cs_entry) = resolve_sidecar_paths(app, log)?;

    // Ensure data directories exist and settings.json is created.
    ensure_data_dir(data_dir, log)?;
    let userdata_dir = data_dir.join("userdata");
    let extensions_dir = data_dir.join("extensions");

    // Open the log file for code-server's stdout.
    // stderr is piped (not redirected to file) so we can capture and log
    // code-server's error messages when it crashes.
    let stdout_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| format!("failed to open log file for code-server stdout: {}", e))?;

    log_line(
        log,
        &format!(
            "spawning: {:?} {:?} --bind-addr 127.0.0.1:{} --auth none --user-data-dir {:?} --extensions-dir {:?}",
            node_exe, cs_entry, port, userdata_dir, extensions_dir
        ),
    );

    let mut cmd = Command::new(&node_exe);
    cmd.arg(&cs_entry)
        .arg("--bind-addr")
        .arg(format!("127.0.0.1:{}", port))
        .arg("--auth")
        .arg("none")
        .arg("--disable-telemetry")
        .arg("--disable-update-check")
        // Store all user data in %APPDATA%\JogiCode\userdata instead of
        // code-server's default %LOCALAPPDATA%\code-server\Data
        .arg("--user-data-dir")
        .arg(&userdata_dir)
        // Store extensions in %APPDATA%\JogiCode\extensions
        .arg("--extensions-dir")
        .arg(&extensions_dir)
        .current_dir(cs_entry.parent().unwrap_or(std::path::Path::new(".")))
        .stdout(Stdio::from(stdout_file))
        // Pipe stderr so we can read code-server's error messages when
        // it crashes. Previously this went to a file, but the output
        // wasn't visible until the process fully exited. With a pipe,
        // we can read stderr in the premature-exit checker thread.
        .stderr(Stdio::piped());

    // ── MEMORY CEILING: Enforce V8 heap limit on code-server process ──
    // NODE_OPTIONS is read by Node.js at startup and caps the V8 old-space
    // heap to 384MB. This prevents code-server + extensions from consuming
    // unbounded memory. Combined with the settings.json terminal env var,
    // this enforces a hard <600MB ceiling per process.
    //
    // We also set UV_THREADPOOL_SIZE=8 (down from default 4, up from 1) to
    // balance I/O throughput without spawning excessive threads.
    //
    // ELECTRON_DISABLE_SECURITY_WARNINGS suppresses console noise.
    cmd.env("NODE_OPTIONS", "--max-old-space-size=384");
    cmd.env("UV_THREADPOOL_SIZE", "8");
    cmd.env("ELECTRON_DISABLE_SECURITY_WARNINGS", "true");

    // ── PORTABLE MODE: Redirect home/AppData into the JogiCode data dir ──
    // Only the code-server child sees these overrides; the rest of the OS is
    // untouched. This keeps Kilo Code config (~/.config/kilo) and any other
    // home-dir-writing tool inside <data>\home so nothing leaks to the real
    // user profile.
    let home_dir = data_dir.join("home");
    let _ = std::fs::create_dir_all(&home_dir);
    cmd.env("USERPROFILE", &home_dir);
    cmd.env("HOME", &home_dir);
    log_line(log, &format!("USERPROFILE/HOME redirected to: {:?}", home_dir));

    if REDIRECT_APPDATA {
        let appdata_dir = data_dir.join("appdata");
        let _ = std::fs::create_dir_all(&appdata_dir);
        cmd.env("APPDATA", &appdata_dir);
        log_line(log, &format!("APPDATA redirected to: {:?}", appdata_dir));
    }

    // ── LOCALIZED TEMP: Redirect temp dirs to JogiCode data dir ──
    // By default, code-server and extensions write temp files to %TEMP%
    // (C:\Users\<user>\AppData\Local\Temp). We redirect to a JogiCode-local
    // temp directory so all temp data is in one place and can be cleaned
    // easily. This also prevents temp file accumulation in the global temp.
    let tmp_dir = data_dir.join("tmp");
    let _ = std::fs::create_dir_all(&tmp_dir);
    cmd.env("TEMP", &tmp_dir);
    cmd.env("TMP", &tmp_dir);
    cmd.env("TMPDIR", &tmp_dir);
    log_line(log, &format!("temp dir redirected to: {:?}", tmp_dir));

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    cmd.spawn()
        .map_err(|e| format!("failed to spawn code-server: {}", e))
}

/// Show an error message on the splash page UI.
fn show_ui_error(window: &tauri::WebviewWindow, message: &str) {
    let escaped = message.replace('\\', "\\\\").replace('\'', "\\'").replace('\n', " ");
    let js = format!(
        "var s = document.getElementById('status'); \
         s.textContent = '{escaped}'; \
         s.classList.add('error'); \
         var sp = document.querySelector('.spinner'); \
         if (sp) sp.style.display = 'none';",
        escaped = escaped
    );
    let _ = window.eval(&js);
}

/// Update the splash page status text.
fn update_ui_status(window: &tauri::WebviewWindow, message: &str) {
    let escaped = message.replace('\'', "\\'");
    let js = format!(
        "var s = document.getElementById('status'); \
         if (!s.classList.contains('error')) {{ s.textContent = '{}'; }}",
        escaped
    );
    let _ = window.eval(&js);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // ── WEBVIEW2 MEMORY CONSTRAINTS ──
    // Set environment variables that WebView2 (Edge/Chromium) reads at startup
    // to enforce memory ceilings and reduce background resource consumption.
    // These must be set BEFORE the webview is created.
    //
    // --memory-pressure-off: Prevents Chromium from running background memory
    //   pressure detection cycles (saves ~2-3% CPU on idle).
    // --disable-gpu-shader-disk-cache: Reduces disk I/O and cache buildup.
    // --disable-features=CalculateNativeWinOcclusion: Disables window
    //   occlusion tracking (saves CPU on Windows).
    // --js-flags=--max-old-space-size=256: Caps V8 heap in WebView2 to 256MB.
    //
    // WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS is the official env var for
    // passing Chromium flags to WebView2.
    #[cfg(windows)]
    {
        let webview_args = "--memory-pressure-off --disable-gpu-shader-disk-cache --disable-features=CalculateNativeWinOcclusion --js-flags=--max-old-space-size=256";
        std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", webview_args);
    }

    tauri::Builder::default()
        .setup(|app| {
            // ── DATA DIRECTORY (portable first, AppData fallback) ──
            // Portable mode: <exe dir>\data so the app is fully self-contained.
            // Installed mode (Program Files via NSIS): %APPDATA%\JogiCode.
            let data_dir = match resolve_data_dir(app) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("[jogicode] FATAL: cannot resolve data dir: {}", e);
                    std::env::temp_dir().join("jogicode")
                }
            };
            if let Err(e) = std::fs::create_dir_all(&data_dir) {
                eprintln!("[jogicode] FATAL: cannot create data dir {:?}: {}", data_dir, e);
            }

            let log_path = data_dir.join("jogicode.log");

            let log_file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&log_path);

            let log: LogFile = match log_file {
                Ok(f) => {
                    let log = Arc::new(Mutex::new(BufWriter::new(Box::new(f) as Box<dyn Write + Send>)));
                    log_line(&log, &format!("JogiCode starting — log file: {:?}", log_path));
                    log
                }
                Err(e) => {
                    eprintln!("[jogicode] FATAL: cannot open log file {:?}: {}", log_path, e);
                    Arc::new(Mutex::new(BufWriter::new(Box::new(std::io::sink()) as Box<dyn Write + Send>)))
                }
            };
            log_line(&log, &format!("data dir: {:?}", data_dir));

            // ── MAIN WINDOW (portable WebView2 profile) ──
            // The window is created here (config has "create": false) so we can
            // point the WebView2 user-data folder at <data>\webview. Without
            // this, WebView2 writes its profile to %LOCALAPPDATA%\<app>\EBWebView.
            let window_config = app
                .config()
                .app
                .windows
                .first()
                .cloned()
                .ok_or_else(|| "no window config in tauri.conf.json".to_string())?;
            tauri::WebviewWindowBuilder::from_config(app, &window_config)
                .data_directory(data_dir.join("webview"))
                .build()
                .map_err(|e| format!("failed to create main window: {}", e))?;
            log_line(&log, "main window created (portable webview profile)");

            // ── Find a free port for code-server ──
            // Instead of hardcoding 8080 (which can become reserved by
            // Windows Hyper-V/WSL2/Docker after a reboot), we find a free
            // port at runtime. Tries 8080-8085 first, then falls back to
            // an OS-assigned random port.
            let port = match find_free_port(&log) {
                Ok(p) => p,
                Err(e) => {
                    log_line(&log, &format!("FATAL: cannot find free port: {}", e));
                    if let Some(window) = app.get_webview_window("main") {
                        show_ui_error(&window, &format!("Cannot find a free network port: {}", e));
                    }
                    return Ok(());
                }
            };

            // Spawn code-server on the dynamic port.
            let child_result = spawn_code_server(app, &log, &log_path, port, &data_dir);
            let child = match child_result {
                Ok(mut child) => {
                    let pid = child.id();
                    log_line(&log, &format!("code-server spawned (pid={}, port={})", pid, port));

                    // Take the stderr pipe so we can read code-server's error
                    // messages if it crashes.
                    let stderr = child.stderr.take();
                    let child_check = Arc::new(Mutex::new(Some(child)));
                    let child_for_check = child_check.clone();
                    let log_for_check = log.clone();

                    std::thread::spawn(move || {
                        // Give code-server 3 seconds to start (or crash).
                        std::thread::sleep(Duration::from_secs(3));

                        if let Ok(mut guard) = child_for_check.lock() {
                            if let Some(ref mut child) = *guard {
                                match child.try_wait() {
                                    Ok(Some(status)) => {
                                        log_line(&log_for_check, &format!(
                                            "code-server exited prematurely with status: {:?}",
                                            status
                                        ));

                                        // Read stderr output to see WHY it crashed.
                                        if let Some(mut stderr) = stderr {
                                            use std::io::Read;
                                            let mut buf = String::new();
                                            let _ = stderr.read_to_string(&mut buf);
                                            if !buf.is_empty() {
                                                // Log each line of stderr separately for clarity.
                                                for line in buf.lines() {
                                                    log_line(&log_for_check, &format!("code-server stderr: {}", line));
                                                }
                                            }
                                        }
                                    }
                                    Ok(None) => {
                                        log_line(&log_for_check, "code-server process is still running after 3s");
                                    }
                                    Err(e) => {
                                        log_line(&log_for_check, &format!(
                                            "failed to check code-server status: {}", e
                                        ));
                                    }
                                }
                            }
                        }
                    });
                    child_check
                }
                Err(e) => {
                    log_line(&log, &format!("FATAL: {}", e));
                    if let Some(window) = app.get_webview_window("main") {
                        show_ui_error(&window, &format!("Startup error: {}", e));
                    }
                    return Ok(());
                }
            };

            app.manage(child.clone());

            let main_window = app
                .get_webview_window("main")
                .expect("[jogicode] main window not found");

            // Spawn a background thread that:
            //   1. Polls the dynamic port until code-server is listening
            //   2. Does an HTTP health check
            //   3. Navigates the webview to code-server
            let log_for_thread = log.clone();
            let window_for_thread = main_window.clone();

            std::thread::spawn(move || {
                update_ui_status(&window_for_thread, "Starting code-server…");

                // Phase 1: TCP port polling.
                log_line(&log_for_thread, &format!("waiting for TCP port {} to open…", port));
                if !wait_for_tcp(port, TCP_POLL_TIMEOUT_SECS, &log_for_thread) {
                    log_line(
                        &log_for_thread,
                        &format!("code-server did not open port {} within {}s", port, TCP_POLL_TIMEOUT_SECS),
                    );
                    show_ui_error(
                        &window_for_thread,
                        &format!(
                            "code-server failed to start within {}s. \
                             Check jogicode.log next to the app.",
                            TCP_POLL_TIMEOUT_SECS
                        ),
                    );
                    return;
                }

                // Phase 2: HTTP health check.
                update_ui_status(&window_for_thread, "Code-server port open, checking HTTP…");
                log_line(&log_for_thread, "TCP port open, starting HTTP health check…");
                if !http_health_check(port, HTTP_HEALTH_TIMEOUT_SECS, &log_for_thread) {
                    log_line(
                        &log_for_thread,
                        "HTTP health check failed — code-server is listening but not responding to HTTP",
                    );
                    show_ui_error(
                        &window_for_thread,
                        "code-server started but HTTP is not responding. Check jogicode.log.",
                    );
                    return;
                }

                // Phase 3: Navigate to code-server.
                update_ui_status(&window_for_thread, "Loading IDE…");
                log_line(&log_for_thread, &format!("navigating webview to code-server on port {}", port));
                let js = format!(
                    "window.location.href = 'http://127.0.0.1:{}';",
                    port
                );
                if let Err(e) = window_for_thread.eval(&js) {
                    log_line(&log_for_thread, &format!("failed to navigate: {}", e));
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                if let Some(state) = window
                    .app_handle()
                    .try_state::<Arc<Mutex<Option<Child>>>>()
                {
                    if let Ok(mut guard) = state.lock() {
                        if let Some(mut child) = guard.take() {
                            println!("[jogicode] killing code-server process");
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
