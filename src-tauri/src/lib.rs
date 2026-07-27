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

/// Ensure the JogiCode data directory exists and create a default settings.json
/// that configures VS Code for workspace-local caching and clipboard support.
fn ensure_data_dir(
    app: &tauri::App,
    log: &LogFile,
) -> Result<std::path::PathBuf, String> {
    // All JogiCode data lives under %APPDATA%\JogiCode\ (Windows)
    // This keeps everything in one clean place instead of scattered across
    // %LOCALAPPDATA%\code-server\, %APPDATA%\code-server\, etc.
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve app_data_dir: {}", e))?;

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

    // Create default settings.json if it doesn't exist.
    // This configures VS Code to:
    // 1. Use workspace-relative paths for caches where possible
    // 2. Enable clipboard support for right-click paste
    // 3. Keep workspace state clean
    let settings_path = user_dir.join("settings.json");
    if !settings_path.exists() {
        let default_settings = r#"{
    // ════════════════════════════════════════════════════════════════════
    // JogiCode Default Settings
    // ════════════════════════════════════════════════════════════════════

    // ── Clipboard ──
    // Enable syntax highlighting when copying (helps clipboard work better)
    "editor.copyWithSyntaxHighlighting": true,
    // Enable clipboard API for right-click paste
    "editor.multiCursorModifier": "alt",

    // ── Workspace-local caches ──
    // TypeScript: use workspace node_modules if available
    "typescript.tsdk": null,
    "typescript.enablePromptUseWorkspaceTsdk": true,
    // TypeScript cache goes in workspace
    "typescript.tsserver.log": "off",

    // ── Files ──
    // Exclude common junk from file watcher (reduces cache load)
    "files.watcherExclude": {
        "**/.git/objects/**": true,
        "**/.git/subtree-cache/**": true,
        "**/node_modules/**": true,
        "**/.jogicode/**": true
    },
    "files.exclude": {
        "**/.jogicode": true
    },
    "search.exclude": {
        "**/node_modules": true,
        "**/.jogicode": true,
        "**/dist": true,
        "**/build": true
    },

    // ── Terminal ──
    // Keep terminal history in workspace
    "terminal.integrated.scrollback": 5000,
    "terminal.integrated.enablePersistentSessions": false,

    // ── Editor ──
    "editor.fontSize": 14,
    "editor.tabSize": 2,
    "editor.formatOnSave": false,
    "editor.minimap.enabled": false,
    "workbench.startupEditor": "none",
    "workbench.colorTheme": "Default Dark+",
    "window.menuBarVisibility": "visible",

    // ── Extensions ──
    // Auto-install extensions to the JogiCode extensions dir
    "extensions.autoUpdate": false,
    "extensions.autoCheckUpdates": false,
    "extensions.ignoreRecommendations": false,

    // ── Telemetry ──
    "telemetry.telemetryLevel": "off",
    "redhat.telemetry.enabled": false,

    // ── Updates ──
    "update.mode": "none"
}"#;
        std::fs::write(&settings_path, default_settings)
            .map_err(|e| format!("failed to write settings.json: {}", e))?;
        log_line(log, "created default settings.json");
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

    Ok(data_dir)
}

/// Spawn code-server on the given port.
/// All data (settings, extensions, workspace state) goes to
/// %APPDATA%\JogiCode\ instead of code-server's default locations.
fn spawn_code_server(
    app: &tauri::App,
    log: &LogFile,
    log_path: &std::path::Path,
    port: u16,
) -> Result<Child, String> {
    let (node_exe, cs_entry) = resolve_sidecar_paths(app, log)?;

    // Ensure data directories exist and settings.json is created.
    let data_dir = ensure_data_dir(app, log)?;
    let userdata_dir = data_dir.join("userdata");
    let extensions_dir = data_dir.join("extensions");

    let stdout_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| format!("failed to open log file for code-server stdout: {}", e))?;
    let stderr_file = stdout_file
        .try_clone()
        .map_err(|e| format!("failed to clone log file for code-server stderr: {}", e))?;

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
        // Disable session persistence (we manage lifecycle ourselves)
        .arg("--disable-session-restore")
        .current_dir(cs_entry.parent().unwrap_or(std::path::Path::new(".")))
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));

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
    tauri::Builder::default()
        .setup(|app| {
            let log_path = app
                .path()
                .app_data_dir()
                .map(|d| d.join("jogicode.log"))
                .unwrap_or_else(|_| std::env::temp_dir().join("jogicode.log"));

            if let Some(parent) = log_path.parent() {
                if !parent.exists() {
                    let _ = std::fs::create_dir_all(parent);
                }
            }

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
            let child_result = spawn_code_server(app, &log, &log_path, port);
            let child = match child_result {
                Ok(child) => {
                    let pid = child.id();
                    log_line(&log, &format!("code-server spawned (pid={}, port={})", pid, port));
                    let child_check = Arc::new(Mutex::new(Some(child)));
                    let child_for_check = child_check.clone();
                    let log_for_check = log.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(2));
                        if let Ok(mut guard) = child_for_check.lock() {
                            if let Some(ref mut child) = *guard {
                                match child.try_wait() {
                                    Ok(Some(status)) => {
                                        log_line(&log_for_check, &format!(
                                            "code-server exited prematurely with status: {:?}",
                                            status
                                        ));
                                    }
                                    Ok(None) => {
                                        log_line(&log_for_check, "code-server process is still running after 2s");
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
                             Check jogicode.log in %APPDATA%\\com.jogicode.app\\",
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
