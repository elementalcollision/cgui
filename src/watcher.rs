//! Background tasks that the TUI cares about:
//!
//! * **FSEvents** on `~/.config/cgui/stacks/` — emit `Event::StacksChanged`
//!   when a `*.toml` file is created, modified, or removed so the TUI can
//!   reload the stack list without the user pressing `r`.
//! * **Restart watcher** — every ~10 s, check each running stack's services
//!   against their `restart` policy and re-run any that have stopped.
//! * **Healthcheck loop** — every ~10 s, run each service's healthcheck and
//!   publish the result as `Event::Health`. We intentionally keep the loop
//!   coarse-grained (10 s) and rely on the per-service `interval_s` to
//!   debounce expensive probes.

use crate::stacks::{self, Healthcheck, RestartPolicy, Service, Stack};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone)]
pub enum Event {
    StacksChanged,
    Health {
        stack: String,
        service: String,
        ok: bool,
        message: String,
    },
    Status(String),
    Updates(Vec<crate::update::UpdateInfo>),
}

/// Spawn a one-shot update check at startup. Future iterations will poll on
/// an interval; phase 1 keeps it to a single startup probe so failures are
/// silent and bounded.
pub fn spawn_update_check(tx: UnboundedSender<Event>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut prefs = crate::prefs::Prefs::load();
        let updates = crate::update::check(&mut prefs).await;
        if !updates.is_empty() {
            let _ = tx.send(Event::Updates(updates));
        }
    })
}

/// Spawn the FSEvents watcher in a blocking thread; events flow through `tx`.
/// Returns the watcher handle so it lives at least as long as the channel.
pub fn spawn_fs_watcher(tx: UnboundedSender<Event>) -> Option<notify::RecommendedWatcher> {
    use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
    let dir = stacks::stacks_dir()?;
    let _ = std::fs::create_dir_all(&dir);
    let tx_inner = tx.clone();
    let mut w: RecommendedWatcher = match RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if matches!(
                    ev.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) {
                    let _ = tx_inner.send(Event::StacksChanged);
                }
            }
        },
        Config::default(),
    ) {
        Ok(w) => w,
        Err(_) => return None,
    };
    if w.watch(&dir, RecursiveMode::NonRecursive).is_err() {
        return None;
    }
    Some(w)
}

/// Spawn the periodic restart + healthcheck loop. Runs forever; the caller
/// keeps a JoinHandle if it wants to abort it on shutdown. Loads stack files
/// from disk each tick so reconfiguration is picked up automatically.
pub fn spawn_restart_health(tx: UnboundedSender<Event>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Per (stack, svc): when last health probe fired.
        let mut last_probe: std::collections::HashMap<(String, String), Instant> =
            std::collections::HashMap::new();
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tick.tick().await;
            let stacks_now = stacks::load_all();
            for stack in &stacks_now {
                for svc in &stack.services {
                    // --- restart policy ---
                    if matches!(svc.restart_policy(), RestartPolicy::Always | RestartPolicy::OnFailure) {
                        let name = stacks::container_name(&stack.name, &svc.name);
                        if let Some(state) = container_state(&name).await {
                            let should_restart = match svc.restart_policy() {
                                RestartPolicy::Always => state == "stopped" || state == "exited",
                                RestartPolicy::OnFailure => state == "exited",
                                _ => false,
                            };
                            if should_restart {
                                let _ = tx.send(Event::Status(format!(
                                    "restart: {} ({state}) → start",
                                    name
                                )));
                                let _ = run_start(&name).await;
                            }
                        }
                    }
                    // --- healthcheck ---
                    if let Some(hc) = &svc.healthcheck {
                        let key = (stack.name.clone(), svc.name.clone());
                        let due = match last_probe.get(&key) {
                            Some(t) => t.elapsed() >= Duration::from_secs(hc.interval_s.max(1)),
                            None => true,
                        };
                        if !due {
                            continue;
                        }
                        last_probe.insert(key.clone(), Instant::now());
                        let (ok, message) = probe(stack, svc, hc).await;
                        let _ = tx.send(Event::Health {
                            stack: stack.name.clone(),
                            service: svc.name.clone(),
                            ok,
                            message,
                        });
                    }
                }
            }
        }
    })
}

async fn container_state(name: &str) -> Option<String> {
    // `container inspect` returns an array; we just want top-level "status".
    let out = tokio::process::Command::new(crate::runtime::binary())
        .args(["inspect", name])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let arr = v.as_array()?;
    let first = arr.first()?;
    Some(
        first
            .get("status")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .to_string(),
    )
}

async fn run_start(name: &str) -> std::io::Result<std::process::Output> {
    tokio::process::Command::new(crate::runtime::binary())
        .args(["start", name])
        .output()
        .await
}

async fn probe(stack: &Stack, svc: &Service, hc: &Healthcheck) -> (bool, String) {
    match hc.kind.as_str() {
        "cmd" => probe_cmd(stack, svc, hc).await,
        "http" => probe_http(svc, hc).await,
        _ => probe_tcp(svc, hc).await,
    }
}

/// TCP healthcheck. Looks at the service's `ports` list for an entry like
/// `<host>:<container>` matching `target` on either side, then attempts a
/// 1 s TCP connect to `127.0.0.1:<host>`. If `target` is empty, falls back
/// to the first published port.
async fn probe_tcp(svc: &Service, hc: &Healthcheck) -> (bool, String) {
    let want: Option<&str> = hc.target.as_deref();
    // Collect (host, container) pairs from the published ports list.
    let pairs: Vec<(String, String)> = svc
        .ports
        .iter()
        .filter_map(|p| p.split_once(':').map(|(h, c)| (h.to_string(), c.to_string())))
        .collect();
    let port: Option<String> = match want {
        None => pairs.first().map(|(h, _)| h.clone()),
        Some(t) => pairs
            .iter()
            .find(|(h, c)| h == t || c == t)
            .map(|(h, _)| h.clone())
            .or_else(|| Some(t.to_string())),
    };
    let Some(port) = port else {
        return (false, "no published port for tcp probe".into());
    };
    let addr = format!("127.0.0.1:{port}");
    let timeout = Duration::from_secs(1);
    match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(&addr)).await {
        Ok(Ok(_)) => (true, format!("tcp {addr} ok")),
        Ok(Err(e)) => (false, format!("tcp {addr}: {e}")),
        Err(_) => (false, format!("tcp {addr}: timeout")),
    }
}

/// HTTP / HTTPS healthcheck. `target` accepts:
///   * `"PORT"`              → http://127.0.0.1:PORT/
///   * `"PORT/PATH"`         → http://127.0.0.1:PORT/PATH
///   * `"http://host:port/path"`  → plain HTTP/1.0, hand-rolled client
///   * `"https://host:port/path"` → shell out to `curl --silent --max-time 2 -o /dev/null -w "%{http_code}"`
///
/// Success = response status is in `expect_status[0]..=expect_status[1]`,
/// or 200..399 if not specified. We avoid pulling in a TLS dependency by
/// reusing macOS's built-in `curl` for the HTTPS branch — same approach
/// the update module uses for GitHub API calls.
async fn probe_http(svc: &Service, hc: &Healthcheck) -> (bool, String) {
    let raw = match hc.target.as_deref() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => match svc.ports.first() {
            Some(p) => p.split(':').next().unwrap_or("").to_string(),
            None => return (false, "no http/https target and no published port".into()),
        },
    };
    if raw.starts_with("https://") {
        return probe_https_via_curl(&raw, hc).await;
    }
    let (host, port, path) = parse_http_target(&raw);
    let addr = format!("{host}:{port}");
    let timeout = Duration::from_secs(2);
    let connect = tokio::net::TcpStream::connect(&addr);
    let stream = match tokio::time::timeout(timeout, connect).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return (false, format!("http {addr}{path}: {e}")),
        Err(_) => return (false, format!("http {addr}{path}: connect timeout")),
    };
    let req = format!(
        "GET {path} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: cgui\r\nConnection: close\r\n\r\n"
    );
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = stream;
    if let Err(e) = stream.write_all(req.as_bytes()).await {
        return (false, format!("http {addr}{path}: write {e}"));
    }
    let mut buf = Vec::with_capacity(1024);
    // We only need the first line; cap the read to avoid pulling a huge body.
    let read = tokio::time::timeout(timeout, async {
        let mut tmp = [0u8; 256];
        loop {
            match stream.read(&mut tmp).await {
                Ok(0) => break Ok::<_, std::io::Error>(()),
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.len() > 512 || buf.iter().any(|b| *b == b'\n') {
                        break Ok(());
                    }
                }
                Err(e) => break Err(e),
            }
        }
    })
    .await;
    match read {
        Err(_) => return (false, format!("http {addr}{path}: read timeout")),
        Ok(Err(e)) => return (false, format!("http {addr}{path}: read {e}")),
        Ok(Ok(())) => {}
    }
    let head = String::from_utf8_lossy(&buf);
    let first = head.lines().next().unwrap_or("");
    let status: Option<u16> = first.split_whitespace().nth(1).and_then(|s| s.parse().ok());
    let (lo, hi) = expected_range(hc);
    match status {
        Some(c) if c >= lo && c <= hi => (true, format!("http {addr}{path} → {c}")),
        Some(c) => (false, format!("http {addr}{path} → {c} (expected {lo}-{hi})")),
        None => (false, format!("http {addr}{path}: malformed response: {first}")),
    }
}

/// HTTPS branch via `curl`. Returns just the status code; we don't bother
/// reading the body. `--max-time 2` keeps us bounded; `-o /dev/null` drops
/// the body to avoid slurping large responses into memory.
async fn probe_https_via_curl(url: &str, hc: &Healthcheck) -> (bool, String) {
    let timeout = std::time::Duration::from_secs(3);
    let fut = tokio::process::Command::new("curl")
        .args([
            "-s", "-S", "--max-time", "2", "-o", "/dev/null",
            "-w", "%{http_code}", url,
        ])
        .output();
    let out = match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return (false, format!("https {url}: spawn {e}")),
        Err(_) => return (false, format!("https {url}: outer timeout")),
    };
    let status_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() && status_str.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return (false, format!("https {url}: curl {err}"));
    }
    let code: Option<u16> = status_str.parse().ok();
    let (lo, hi) = expected_range(hc);
    match code {
        Some(c) if c >= lo && c <= hi => (true, format!("https {url} → {c}")),
        Some(c) => (false, format!("https {url} → {c} (expected {lo}-{hi})")),
        None => (false, format!("https {url}: malformed status {status_str:?}")),
    }
}

fn parse_http_target(t: &str) -> (String, u16, String) {
    if let Some(rest) = t.strip_prefix("http://") {
        let (auth, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match auth.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().unwrap_or(80)),
            None => (auth.to_string(), 80),
        };
        return (host, port, path.to_string());
    }
    // PORT or PORT/PATH form.
    let (port_str, path) = match t.find('/') {
        Some(i) => (&t[..i], &t[i..]),
        None => (t, "/"),
    };
    let port: u16 = port_str.parse().unwrap_or(80);
    ("127.0.0.1".to_string(), port, path.to_string())
}

fn expected_range(hc: &Healthcheck) -> (u16, u16) {
    match hc.expect_status.as_slice() {
        [a, b] => (*a, *b),
        [a] => (*a, *a),
        _ => (200, 399),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_target_port_only() {
        assert_eq!(parse_http_target("8080"), ("127.0.0.1".into(), 8080, "/".into()));
    }
    #[test]
    fn parse_target_port_path() {
        assert_eq!(parse_http_target("8080/healthz"), ("127.0.0.1".into(), 8080, "/healthz".into()));
    }
    #[test]
    fn parse_target_full_url() {
        assert_eq!(
            parse_http_target("http://example.com:8080/v1/ping"),
            ("example.com".into(), 8080, "/v1/ping".into())
        );
    }
    #[test]
    fn parse_target_url_no_path() {
        assert_eq!(
            parse_http_target("http://localhost:80"),
            ("localhost".into(), 80, "/".into())
        );
    }
    #[test]
    fn expected_default_range() {
        let hc = crate::stacks::Healthcheck::default();
        assert_eq!(expected_range(&hc), (200, 399));
    }
}

/// Exec a command inside the container; success = exit 0.
async fn probe_cmd(stack: &Stack, svc: &Service, hc: &Healthcheck) -> (bool, String) {
    if hc.command.is_empty() {
        return (false, "healthcheck.command is empty".into());
    }
    let name = stacks::container_name(&stack.name, &svc.name);
    let mut args: Vec<String> = vec!["exec".into(), name.clone()];
    args.extend(hc.command.iter().cloned());
    let out = tokio::process::Command::new(crate::runtime::binary())
        .args(args.iter().map(|s| s.as_str()))
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => (true, format!("exec ok ({})", hc.command.join(" "))),
        Ok(o) => (
            false,
            format!(
                "exec exit {}: {}",
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stderr).trim()
            ),
        ),
        Err(e) => (false, format!("exec spawn: {e}")),
    }
}
