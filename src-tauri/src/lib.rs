use std::net::TcpStream;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::Manager;
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;

/// The port code-server listens on.
const CODE_SERVER_PORT: u16 = 8080;
/// How long to wait for code-server to start (seconds).
const STARTUP_TIMEOUT_SECS: u64 = 120;
/// Extra delay after port opens to let HTTP server fully initialize.
const HTTP_READY_DELAY_SECS: u64 = 2;

/// Poll 127.0.0.1:PORT until a TCP connection succeeds or timeout.
/// Returns true if the port is open, false on timeout.
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            println!("[jogicode] starting code-server sidecar on port {}", CODE_SERVER_PORT);

            // Spawn code-server as a Tauri sidecar.
            // The binary is resolved from bundle.externalBin — Tauri appends
            // the target triple automatically (e.g. code-server-x86_64-pc-windows-msvc.exe).
            let sidecar = app
                .shell()
                .sidecar("code-server")
                .expect("[jogicode] failed to resolve code-server sidecar binary");

            let (mut rx, child) = sidecar
                .args([
                    "--bind-addr", &format!("127.0.0.1:{}", CODE_SERVER_PORT),
                    "--auth", "none",
                    "--disable-telemetry",
                    "--disable-update-check",
                ])
                .spawn()
                .expect("[jogicode] failed to spawn code-server");

            println!("[jogicode] code-server spawned (pid={:?})", child.pid());

            // Store the child handle so we can kill it when the app closes.
            app.manage(Mutex::new(Some(child)));

            // Spawn a background thread that:
            //   1. Polls port 8080 until code-server is listening
            //   2. Waits a brief moment for the HTTP server to fully initialize
            //   3. Navigates the webview from the splash page to code-server
            //
            // We use window.eval() instead of fetch() from JS because the
            // Tauri webview runs in a secure context (https://tauri.localhost)
            // which blocks fetch() to http://127.0.0.1 as mixed content.
            // Top-level navigation via window.location.href is NOT blocked.
            let main_window = app
                .get_webview_window("main")
                .expect("[jogicode] main window not found");

            std::thread::spawn(move || {
                // Log code-server stdout/stderr (non-blocking).
                tauri::async_runtime::spawn(async move {
                    while let Some(event) = rx.recv().await {
                        match event {
                            tauri_plugin_shell::process::CommandEvent::Stdout(line) => {
                                println!("[code-server] {}", String::from_utf8_lossy(&line));
                            }
                            tauri_plugin_shell::process::CommandEvent::Stderr(line) => {
                                eprintln!("[code-server] {}", String::from_utf8_lossy(&line));
                            }
                            tauri_plugin_shell::process::CommandEvent::Error(err) => {
                                eprintln!("[code-server] error: {}", err);
                            }
                            tauri_plugin_shell::process::CommandEvent::Terminated(payload) => {
                                println!("[code-server] terminated: {:?}", payload);
                            }
                            _ => {}
                        }
                    }
                });

                // Wait for code-server to start listening.
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
                    eprintln!("[jogicode] code-server failed to start within {}s", STARTUP_TIMEOUT_SECS);
                    let _ = main_window.eval(
                        "document.getElementById('status').textContent = \
                         'Failed to start code-server. Please restart JogiCode.';\
                         document.getElementById('status').style.color = '#ef4444';\
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
                    .try_state::<Mutex<Option<CommandChild>>>()
                {
                    if let Ok(mut guard) = state.lock() {
                        if let Some(child) = guard.take() {
                            println!("[jogicode] killing code-server process");
                            let _ = child.kill();
                        }
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
