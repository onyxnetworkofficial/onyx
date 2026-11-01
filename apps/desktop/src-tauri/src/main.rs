//! The Onyx desktop app (CL10, ADR-0061): a window around onyx-contrib's local app.
//!
//! On start it runs the bundled onyx-contrib program (the sidecar, Node's single executable
//! application) as `onyx-contrib ui --desktop`, reads the one line it prints
//! (`ONYX_UI_URL=http://127.0.0.1:<port>/#t=<token>`), checks its shape and opens the window on
//! it. The token travels only through that pipe and the URL fragment. The window may show only the
//! bundled start page and that one local origin; any other address is refused. Quitting closes the
//! sidecar's input (it exits when its input closes) and stops it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder};

/// The sidecar process, kept so quitting can stop it.
struct Sidecar(Mutex<Option<Child>>);

/// How long the sidecar may take to print its address.
const START_TIMEOUT: Duration = Duration::from_secs(45);

/// The bundled program: next to this app's own executable (Tauri's `externalBin`).
fn sidecar_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this app's folder: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "cannot find this app's folder".to_string())?;
    let name = if cfg!(windows) {
        "onyx-contrib.exe"
    } else {
        "onyx-contrib"
    };
    Ok(dir.join(name))
}

/// The address line, checked: http://127.0.0.1:<port>/#t=<64 hex>. Returns the URL and its port.
fn parse_line(line: &str) -> Option<(Url, u16)> {
    let rest = line.trim().strip_prefix("ONYX_UI_URL=")?;
    let url = Url::parse(rest).ok()?;
    let token = url.fragment()?.strip_prefix("t=")?;
    let ok = url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.path() == "/"
        && url.query().is_none()
        && token.len() == 64
        && token.bytes().all(|b| b.is_ascii_hexdigit());
    let port = url.port()?;
    ok.then_some((url, port))
}

/// Start `onyx-contrib ui --desktop` and wait for its address.
fn start_sidecar() -> Result<(Child, Url, u16), String> {
    let path = sidecar_path()?;
    let mut cmd = Command::new(&path);
    cmd.args(["ui", "--desktop"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", path.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "no output pipe from onyx-contrib".to_string())?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut sent = false;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if !sent {
                if let Some(found) = parse_line(&line) {
                    let _ = tx.send(found);
                    sent = true;
                }
            }
            // Keep reading (and dropping) anything else, so the pipe never fills.
        }
    });
    match rx.recv_timeout(START_TIMEOUT) {
        Ok((url, port)) => Ok((child, url, port)),
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err("onyx-contrib did not start in time".to_string())
        }
    }
}

/// Whether the window may show `url`: the bundled start page (`tauri://localhost` on macOS and
/// Linux, `http://tauri.localhost` on Windows, no port), `about:blank`, or the sidecar's own origin.
/// Review CL10-R1-4: exact hosts per system, no port on the bundled origin, no other `about:` page.
fn allowed(url: &Url, port: Option<u16>) -> bool {
    let host = url.host_str();
    match url.scheme() {
        "tauri" => !cfg!(windows) && host == Some("localhost") && url.port().is_none(),
        "http" if host == Some("tauri.localhost") => cfg!(windows) && url.port().is_none(),
        "http" => host == Some("127.0.0.1") && port.is_some() && url.port() == port,
        "about" => url.as_str() == "about:blank",
        _ => false,
    }
}

fn stop(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<Sidecar>() {
        if let Ok(mut guard) = state.0.lock() {
            if let Some(mut child) = guard.take() {
                drop(child.stdin.take()); // its input closes: it exits on its own
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn main() {
    let port: Arc<Mutex<Option<u16>>> = Arc::new(Mutex::new(None));
    let nav_port = Arc::clone(&port);
    let app = tauri::Builder::default()
        .manage(Sidecar(Mutex::new(None)))
        .setup(move |app| {
            let window =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                    .title("Onyx (test network)")
                    .inner_size(1040.0, 820.0)
                    .min_inner_size(380.0, 560.0)
                    .on_navigation(move |url| {
                        let p = nav_port.lock().ok().and_then(|g| *g);
                        allowed(url, p)
                    })
                    .build()?;
            let handle = app.handle().clone();
            let port = Arc::clone(&port);
            std::thread::spawn(move || match start_sidecar() {
                Ok((child, url, p)) => {
                    if let Ok(mut g) = port.lock() {
                        *g = Some(p);
                    }
                    if let Ok(mut g) = handle.state::<Sidecar>().0.lock() {
                        *g = Some(child);
                    }
                    if let Err(e) = window.navigate(url) {
                        let msg =
                            serde_json::to_string(&format!("could not open the app page: {e}"))
                                .unwrap_or_default();
                        let _ = window.eval(format!("window.showError && window.showError({msg})"));
                    }
                }
                Err(e) => {
                    let msg = serde_json::to_string(&e).unwrap_or_default();
                    let _ = window.eval(format!("window.showError && window.showError({msg})"));
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("the Onyx app could not start");
    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            stop(handle);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_line_is_checked() {
        let t = "a".repeat(64);
        let (u, p) = parse_line(&format!("ONYX_UI_URL=http://127.0.0.1:4321/#t={t}\n")).unwrap();
        assert_eq!(p, 4321);
        assert_eq!(u.fragment(), Some(format!("t={t}").as_str()));
        for bad in [
            format!("ONYX_UI_URL=http://evil.example.com:4321/#t={t}"),
            format!("ONYX_UI_URL=https://127.0.0.1:4321/#t={t}"),
            format!("ONYX_UI_URL=http://127.0.0.1:4321/x#t={t}"),
            format!("ONYX_UI_URL=http://127.0.0.1:4321/?a=1#t={t}"),
            "ONYX_UI_URL=http://127.0.0.1:4321/#t=short".to_string(),
            format!("ONYX_UI_URL=http://127.0.0.1/#t={t}"),
            format!("http://127.0.0.1:4321/#t={t}"),
        ] {
            assert!(parse_line(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_window_shows_only_its_own_pages() {
        let ok = |s: &str, p| allowed(&Url::parse(s).unwrap(), p);
        assert_eq!(ok("tauri://localhost/index.html", None), !cfg!(windows));
        assert_eq!(ok("http://tauri.localhost/index.html", None), cfg!(windows));
        assert!(!ok("http://tauri.localhost:8080/index.html", None));
        assert!(!ok("https://tauri.localhost/index.html", None));
        assert!(!ok("tauri://localhost:9/index.html", None));
        assert!(ok("about:blank", None));
        assert!(!ok("about:config", None));
        assert!(ok("http://127.0.0.1:4321/", Some(4321)));
        assert!(!ok("http://127.0.0.1:4322/", Some(4321)));
        assert!(!ok("http://127.0.0.1:4321/", None));
        assert!(!ok("https://evil.example.com/", Some(4321)));
        assert!(!ok("http://localhost:4321/", Some(4321)));
        assert!(!ok("file:///etc/passwd", Some(4321)));
    }
}
