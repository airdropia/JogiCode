use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::Manager;

/// The port code-server listens on.
const CODE_SERVER_PORT: u16 = 8080;
/// How long to wait for code-server to start (seconds).
const STARTUP_TIMEOUT_SECS: u64 = 120;
/// Extra delay after port opens to let HTTP server fully initialize.
const HTTP_READY_DELAY_SECS: u64 = 3;

/// Poll 127.0.0.1:PORT until a TCP connection succeeds or timeout.
fn wait_for_port(port: u16, timeout_secs: u64) -> bool {
    let addr = format!("127.0.0.1:{}", port);
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    while start.elapsed() < timeout {
        match TcpStream::connect_timeout(
            &addr.parse().expect("invalid addr"),
            Duration::from_secs(1),
        ) {
            Ok(_) => return true,
            Err(_) => std::thread::sleep(Duration::from_millis(500)),
        }
    }
    false
}

/// Resolve the path to the bundled node.exe and code-server entry point.
/// Both live under the Tauri resource directory in `binaries/`.
fn resolve_sidecar_paths(app: &tauri::App) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let resource_dir = app
        .path()
        .resource_dir()
        .map_err(|e| format!("failed to resolve resource dir: {}", e))?;

    let node_exe = resource_dir.join("binaries").join("node.exe");
    let cs_entry = resource_dir
        .join("binaries")
        .join("code-server")
        .join("out")
        .join("node")
        .join("entry.js");

    if !node_exe.exists() {
        return Err(format!("node.exe not found at {:?}", node_exe));
    }
    if !cs_entry.exists() {
        return Err(format!("code-server entry.js not found at {:?}", cs_entry));
    }

    Ok((node_exe, cs_entry))
}

/// Spawn code-server: node.exe binaries/code-server/out/node/entry.js --bind-addr 127.0.0.1:8080 --auth none ...
fn spawn_code_server(app: &tauri::App) -> Result<Child, String> {
    let (node_exe, cs_entry) = resolve_sidecar_paths(app)?;

    println!("[jogicode] spawning code-server: {:?} {:?} --bind-addr 127.0.0.1:{} --auth none", node_exe, cs_entry, CODE_SERVER_PORT);

    Command::new(&node_exe)
        .arg(&cs_entry)
        .arg("--bind-addr")
        .arg(format!("127.0.0.1:{}", CODE_SERVER_PORT))
        .arg("--auth")
        .arg("none")
        .arg("--disable-telemetry")
        .arg("--disable-update-check")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn code-server: {}", e))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // Spawn code-server.
            match spawn_code_server(app) {
                Ok(child) => {
                    let pid = child.id();
                    println!("[jogicode] code-server spawned (pid={})", pid);
                    // Store the child handle so we can kill it when the app closes.
                    app.manage(Mutex::new(Some(child)));
                }
                Err(e) => {
                    eprintln!("[jogicode] FATAL: {}", e);
                    // Show error on the splash page.
                    if let Some(window) = app.get_webview_window("main") {
                        let msg = format!(
                            "document.getElementById('status').textContent = 'Error: {}'; \
                             document.getElementById('status').classList.add('error'); \
                             document.querySelector('.spinner').style.display = 'none';",
                            e.replace('\'', "\\'")
                        );
                        let _ = window.eval(&msg);
                    }
                    return Ok(());
                }
            }

            // Get the main window reference for navigation.
            let main_window = app
                .get_webview_window("main")
                .expect("[jogicode] main window not found");

            // Spawn a background thread that:
            //   1. Polls port 8080 until code-server is listening
            //   2. Waits a brief moment for the HTTP server to fully initialize
            //   3. Navigates the webview from the splash page to code-server
            //
            // We use window.eval() instead of fetch() from JS because the
            // Tauri webview runs in a secure context (https://tauri.localhost)
            // which blocks fetch() to http://127.0.0.1 as mixed content.
            // Top-level navigation via window.location.href is NOT blocked.
            std::thread::spawn(move || {
                if wait_for_port(CODE_SERVER_PORT, STARTUP_TIMEOUT_SECS) {
                    println!("[jogicode] code-server is listening, navigating webview");
                    // Give the HTTP server a moment to fully initialize.
                    std::thread::sleep(Duration::from_secs(HTTP_READY_DELAY_SECS));
                    // Navigate the webview from splash page to code-server.
                    let js = format!(
                        "window.location.href = 'http://127.0.0.1:{}';",
                        CODE_SERVER_PORT
                    );
                    if let Err(e) = main_window.eval(&js) {
                        eprintln!("[jogicode] failed to navigate: {}", e);
                    }
                } else {
                    eprintln!(
                        "[jogicode] code-server failed to start within {}s",
                        STARTUP_TIMEOUT_SECS
                    );
                    let _ = main_window.eval(
                        "document.getElementById('status').textContent = \
                         'Failed to start code-server. Please restart JogiCode.';\
                         document.getElementById('status').classList.add('error');\
                         document.querySelector('.spinner').style.display = 'none';"
                    );
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            // Kill code-server when the main window is destroyed.
            if let tauri::WindowEvent::Destroyed = event {
                if let Some(state) = window
                    .app_handle()
                    .try_state::<Mutex<Option<Child>>>()
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
