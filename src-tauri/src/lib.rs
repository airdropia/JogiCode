use std::fs::OpenOptions;
use std::io::{BufWriter, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Manager;

const CODE_SERVER_PORT: u16 = 8080;
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
/// Used as a fallback if the expected entry.js path doesn't exist.
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

/// Resolve the path to the bundled node.exe and code-server entry point.
/// Uses canonicalize() for absolute Windows paths, and falls back to
/// recursive search if the expected entry.js path doesn't exist.
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

    // List the contents of binaries/ for debugging.
    if let Ok(entries) = std::fs::read_dir(&binaries_dir) {
        let contents: Vec<String> = entries
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        log_line(log, &format!("binaries/ contents: {:?}", contents));
    }

    // node.exe
    let node_exe = binaries_dir.join("node.exe");
    log_line(log, &format!("node.exe path: {:?}", node_exe));
    if !node_exe.exists() {
        return Err(format!("node.exe not found at {:?}", node_exe));
    }
    let node_exe = node_exe
        .canonicalize()
        .map_err(|e| format!("failed to canonicalize node.exe path: {}", e))?;
    log_line(log, &format!("node.exe canonical: {:?}", node_exe));

    // code-server entry.js — try multiple known locations.
    let cs_base = binaries_dir.join("code-server");
    let candidate_paths = [
        // Standard npm install location:
        // binaries/code-server/node_modules/code-server/out/node/entry.js
        cs_base
            .join("node_modules")
            .join("code-server")
            .join("out")
            .join("node")
            .join("entry.js"),
        // Alternative: if code-server was installed globally or differently
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

    // Fallback: recursive search for entry.js under binaries/code-server/
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
    log_line(log, &format!("entry.js canonical: {:?}", cs_entry));

    Ok((node_exe, cs_entry))
}

/// Spawn code-server: node.exe entry.js --bind-addr 127.0.0.1:8080 --auth none ...
/// code-server stdout/stderr are piped to the log file.
fn spawn_code_server(
    app: &tauri::App,
    log: &LogFile,
    log_path: &std::path::Path,
) -> Result<Child, String> {
    let (node_exe, cs_entry) = resolve_sidecar_paths(app, log)?;

    // Open the log file separately for code-server's stdout/stderr.
    // We can't clone the Box<dyn Write> inside the LogFile, so we reopen
    // the same file path. code-server's output will be interleaved with
    // our Rust logs in the same file.
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
            "spawning: {:?} {:?} --bind-addr 127.0.0.1:{} --auth none --disable-telemetry --disable-update-check",
            node_exe, cs_entry, CODE_SERVER_PORT
        ),
    );

    Command::new(&node_exe)
        .arg(&cs_entry)
        .arg("--bind-addr")
        .arg(format!("127.0.0.1:{}", CODE_SERVER_PORT))
        .arg("--auth")
        .arg("none")
        .arg("--disable-telemetry")
        .arg("--disable-update-check")
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .spawn()
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
            // Open a log file in the per-user app data directory.
            // Windows: %APPDATA%\com.jogicode.app\jogicode.log
            let log_path = app
                .path()
                .app_data_dir()
                .map(|d| d.join("jogicode.log"))
                .unwrap_or_else(|_| std::env::temp_dir().join("jogicode.log"));

            // Ensure parent dir exists.
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
                    // Fall back to a no-op logger (println only).
                    Arc::new(Mutex::new(BufWriter::new(Box::new(std::io::sink()) as Box<dyn Write + Send>)))
                }
            };

            // Spawn code-server.
            let child_result = spawn_code_server(app, &log, &log_path);
            let child = match child_result {
                Ok(child) => {
                    let pid = child.id();
                    log_line(&log, &format!("code-server spawned (pid={})", pid));
                    // Check if the process is still alive after 2 seconds.
                    // If it exited immediately, the entry path or args are wrong.
                    let child_check = Arc::new(Mutex::new(Some(child)));
                    let child_for_check = child_check.clone();
                    let log_for_check = log.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(2));
                        // Try to get a lock on the child — if we can, check if
                        // it's still running by attempting wait(non-blocking).
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

            // Store the child handle so we can kill it when the app closes.
            app.manage(child.clone());

            // Get the main window reference for navigation.
            let main_window = app
                .get_webview_window("main")
                .expect("[jogicode] main window not found");

            // Spawn a background thread that:
            //   1. Polls TCP port 8080 until code-server is listening
            //   2. Does an HTTP health check to verify the server is responding
            //   3. Navigates the webview from the splash page to code-server
            let log_for_thread = log.clone();
            let window_for_thread = main_window.clone();

            std::thread::spawn(move || {
                update_ui_status(&window_for_thread, "Starting code-server…");

                // Phase 1: TCP port polling.
                log_line(&log_for_thread, "waiting for TCP port 8080 to open…");
                if !wait_for_tcp(CODE_SERVER_PORT, TCP_POLL_TIMEOUT_SECS, &log_for_thread) {
                    log_line(
                        &log_for_thread,
                        &format!("code-server did not open port {} within {}s", CODE_SERVER_PORT, TCP_POLL_TIMEOUT_SECS),
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
                if !http_health_check(CODE_SERVER_PORT, HTTP_HEALTH_TIMEOUT_SECS, &log_for_thread) {
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
                log_line(&log_for_thread, "navigating webview to code-server");
                let js = format!(
                    "window.location.href = 'http://127.0.0.1:{}';",
                    CODE_SERVER_PORT
                );
                if let Err(e) = window_for_thread.eval(&js) {
                    log_line(&log_for_thread, &format!("failed to navigate: {}", e));
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // Kill code-server when the main window is destroyed.
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
