//! Thin async wrapper around Apple's `container` CLI.
//!
//! We always invoke `--format json` and decode into permissive types, so the
//! rest of the app doesn't have to care about the CLI's exact shape across
//! versions.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Resolved at call-time from the active runtime profile.
fn bin() -> String {
    crate::runtime::binary()
}

/// True when the active runtime is Apple's `container` CLI (vs docker /
/// podman / nerdctl, where flags like `--progress=plain` aren't supported
/// or take different forms). We detect by basename so an absolute path
/// like `/usr/local/bin/container` still matches.
fn is_apple_container() -> bool {
    let b = bin();
    let basename = std::path::Path::new(&b)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&b);
    basename == "container"
}

#[derive(Debug, Clone)]
pub struct Container {
    pub id: String,
    pub image: String,
    pub status: String,
    pub cpus: u64,
    pub memory_bytes: u64,
    pub ports: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Image {
    pub reference: String,
    pub size: String,
    pub digest: String,
}

#[derive(Debug, Clone)]
pub struct Volume {
    pub name: String,
    pub driver: String,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct Network {
    pub id: String,
    pub mode: String,
    pub state: String,
    pub subnet: String,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct StatRow {
    pub id: String,
    pub name: String,
    #[serde(rename = "cpuPercent", alias = "cpu_percent", alias = "CPU")]
    pub cpu_percent: f64,
    #[serde(rename = "memoryUsage", alias = "memory_usage", alias = "MemUsage")]
    pub memory_usage: u64,
    #[serde(rename = "memoryLimit", alias = "memory_limit", alias = "MemLimit")]
    pub memory_limit: u64,
}

/// Default timeout for one-shot CLI calls. Stats can legitimately take ~2s
/// even with `--no-stream` (the runtime waits a sample window), so the
/// timeout has to be generous.
const RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

async fn run(args: &[&str]) -> Result<Vec<u8>> {
    let fut = Command::new(bin()).args(args).output();
    let out = match tokio::time::timeout(RUN_TIMEOUT, fut).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return Err(anyhow!(e)).with_context(|| {
                format!("failed to spawn `{} {}`", bin(), args.join(" "))
            });
        }
        Err(_) => {
            return Err(anyhow!(
                "`{} {}` timed out after {}s",
                bin(),
                args.join(" "),
                RUN_TIMEOUT.as_secs()
            ));
        }
    };
    if !out.status.success() {
        return Err(anyhow!(
            "`{} {}` exited {}: {}",
            bin(),
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

pub async fn list_containers(all: bool) -> Result<Vec<Container>> {
    let mut args = vec!["ls", "--format", "json"];
    if all {
        args.push("--all");
    }
    let bytes = run(&args).await?;
    let raw: Vec<Value> = serde_json::from_slice(&bytes).context("parse container ls json")?;
    Ok(raw.into_iter().map(parse_container).collect())
}

fn parse_container(v: Value) -> Container {
    let cfg = v.get("configuration").cloned().unwrap_or(Value::Null);
    let id = cfg
        .get("id")
        .and_then(|x| x.as_str())
        .unwrap_or("?")
        .to_string();
    let image = cfg
        .get("image")
        .and_then(|i| i.get("reference"))
        .and_then(|x| x.as_str())
        .unwrap_or("?")
        .to_string();
    let status = v
        .get("status")
        .and_then(|x| x.as_str())
        .unwrap_or("unknown")
        .to_string();
    let cpus = cfg
        .get("resources")
        .and_then(|r| r.get("cpus"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let memory_bytes = cfg
        .get("resources")
        .and_then(|r| r.get("memoryInBytes"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let ports = cfg
        .get("publishedPorts")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|p| {
                    let host = p.get("hostPort").and_then(|x| x.as_u64()).unwrap_or(0);
                    let cont = p.get("containerPort").and_then(|x| x.as_u64()).unwrap_or(0);
                    let proto = p.get("proto").and_then(|x| x.as_str()).unwrap_or("tcp");
                    format!("{host}:{cont}/{proto}")
                })
                .collect()
        })
        .unwrap_or_default();
    Container {
        id,
        image,
        status,
        cpus,
        memory_bytes,
        ports,
    }
}

pub async fn list_images() -> Result<Vec<Image>> {
    let bytes = run(&["image", "ls", "--format", "json"]).await?;
    let raw: Vec<Value> = serde_json::from_slice(&bytes).context("parse image ls json")?;
    Ok(raw
        .into_iter()
        .map(|v| Image {
            reference: v
                .get("reference")
                .and_then(|x| x.as_str())
                .unwrap_or("?")
                .to_string(),
            size: v
                .get("fullSize")
                .and_then(|x| x.as_str())
                .unwrap_or("?")
                .to_string(),
            digest: v
                .get("descriptor")
                .and_then(|d| d.get("digest"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .collect())
}

pub async fn list_volumes() -> Result<Vec<Volume>> {
    let bytes = run(&["volume", "ls", "--format", "json"]).await?;
    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(vec![]);
    }
    let raw: Vec<Value> = serde_json::from_slice(&bytes).unwrap_or_default();
    Ok(raw
        .into_iter()
        .map(|v| Volume {
            name: v
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("?")
                .to_string(),
            driver: v
                .get("driver")
                .and_then(|x| x.as_str())
                .unwrap_or("local")
                .to_string(),
            source: v
                .get("source")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .collect())
}

pub async fn list_networks() -> Result<Vec<Network>> {
    let bytes = run(&["network", "ls", "--format", "json"]).await?;
    let raw: Vec<Value> = serde_json::from_slice(&bytes).unwrap_or_default();
    Ok(raw
        .into_iter()
        .map(|v| {
            let cfg = v.get("config").cloned().unwrap_or(Value::Null);
            Network {
                id: v
                    .get("id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("?")
                    .to_string(),
                mode: cfg
                    .get("mode")
                    .and_then(|x| x.as_str())
                    .unwrap_or("?")
                    .to_string(),
                state: v
                    .get("state")
                    .and_then(|x| x.as_str())
                    .unwrap_or("?")
                    .to_string(),
                subnet: v
                    .get("status")
                    .and_then(|s| s.get("ipv4Subnet").or_else(|| s.get("ipv6Subnet")))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            }
        })
        .collect())
}

pub async fn stats_snapshot() -> Result<Vec<StatRow>> {
    let bytes = run(&["stats", "--no-stream", "--format", "json"]).await?;
    let raw: Vec<Value> = serde_json::from_slice(&bytes).unwrap_or_default();
    Ok(raw
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
}

/// Spawn `container logs -f <id>` and stream both stdout and stderr,
/// line-by-line, into the shared sink. Mirrors `spawn_pull` so the modal/
/// log infrastructure can stay shared.
///
/// The returned JoinHandle's `.abort()` kills the streaming task; ratatui's
/// process will exit when the shell does on its own (Ctrl-C from us would
/// require a more involved process-group setup).
pub fn spawn_log_follow(
    id: String,
    sink: Arc<Mutex<Vec<String>>>,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        push(&sink, format!("$ container logs -f {id}"));
        let mut child = Command::new(bin())
            .args(["logs", "-f", &id])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn logs -f {id}"))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let s1 = sink.clone();
        let s2 = sink.clone();
        let t_out = tokio::spawn(async move {
            if let Some(out) = stdout {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    push(&s1, line);
                }
            }
        });
        let t_err = tokio::spawn(async move {
            if let Some(err) = stderr {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    push(&s2, line);
                }
            }
        });
        let _ = child.wait().await;
        let _ = t_out.await;
        let _ = t_err.await;
        push(&sink, "[follow ended]".into());
        Ok(())
    })
}

/// Multi-container follow. Spawns one `container logs -f <id>` per target,
/// tags every line with `[label] `, and merges them into the shared sink.
/// Aborting the returned handle drops the children — `kill_on_drop(true)`
/// ensures their `container` processes get SIGKILLed too.
pub fn spawn_logs_multi(
    targets: Vec<(String, String)>, // (label, container id)
    sink: Arc<Mutex<Vec<String>>>,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        if targets.is_empty() {
            push(&sink, "[multi] no services to follow".into());
            return Ok(());
        }
        push(
            &sink,
            format!(
                "$ container logs -f (multi: {})",
                targets
                    .iter()
                    .map(|(l, _)| l.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::with_capacity(targets.len());
        for (label, id) in targets {
            let s = sink.clone();
            handles.push(tokio::spawn(async move {
                let spawn = Command::new(bin())
                    .args(["logs", "-f", &id])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn();
                let mut child = match spawn {
                    Ok(c) => c,
                    Err(e) => {
                        push(&s, format!("[{label}] spawn error: {e}"));
                        return;
                    }
                };
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let s_out = s.clone();
                let s_err = s.clone();
                let l_out = label.clone();
                let l_err = label.clone();
                let t_out = tokio::spawn(async move {
                    if let Some(out) = stdout {
                        let mut lines = BufReader::new(out).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            push(&s_out, format!("[{l_out}] {line}"));
                        }
                    }
                });
                let t_err = tokio::spawn(async move {
                    if let Some(err) = stderr {
                        let mut lines = BufReader::new(err).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            push(&s_err, format!("[{l_err}] {line}"));
                        }
                    }
                });
                let _ = child.wait().await;
                let _ = t_out.await;
                let _ = t_err.await;
                push(&s, format!("[{label}] follow ended"));
            }));
        }
        for h in handles {
            let _ = h.await;
        }
        push(&sink, "[multi] all follows ended".into());
        Ok(())
    })
}

pub async fn logs(id: &str, tail: usize) -> Result<String> {
    // `container logs` doesn't take --tail in all versions; pull then trim.
    let out = Command::new(bin()).args(["logs", id]).output().await?;
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    let lines: Vec<&str> = s.lines().rev().take(tail).collect();
    Ok(lines.into_iter().rev().collect::<Vec<_>>().join("\n"))
}

pub async fn start(id: &str) -> Result<()> {
    run(&["start", id]).await.map(|_| ())
}
pub async fn stop(id: &str) -> Result<()> {
    run(&["stop", id]).await.map(|_| ())
}
pub async fn kill(id: &str) -> Result<()> {
    run(&["kill", id]).await.map(|_| ())
}
pub async fn delete(id: &str) -> Result<()> {
    run(&["delete", id]).await.map(|_| ())
}

/// Volume detail: pretty-printed `container volume inspect <name>` JSON
/// plus a header derived from filesystem stat of the backing image: capacity
/// from the CLI, actual on-disk usage from `metadata().len()`, fill ratio,
/// and a unicode bar gauge.
pub async fn volume_detail(name: &str) -> Result<String> {
    let bytes = run(&["volume", "inspect", name]).await?;
    let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    // `inspect` returns an array; take the first entry.
    let v = parsed
        .get(0)
        .cloned()
        .unwrap_or_else(|| parsed.clone());
    let pretty = serde_json::to_string_pretty(&v).unwrap_or_else(|_| {
        String::from_utf8_lossy(&bytes).into_owned()
    });

    let source = v
        .get("source")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let capacity = v
        .get("sizeInBytes")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let driver = v.get("driver").and_then(|x| x.as_str()).unwrap_or("?");
    let format = v.get("format").and_then(|x| x.as_str()).unwrap_or("?");
    let created = v.get("createdAt").and_then(|x| x.as_str()).unwrap_or("?");

    let on_disk = if source.is_empty() {
        None
    } else {
        std::fs::metadata(&source).ok().map(|m| m.len())
    };

    let mut header = String::new();
    use std::fmt::Write as _;
    let _ = writeln!(header, "== Volume: {name} ==");
    let _ = writeln!(header, "Driver:    {driver}");
    let _ = writeln!(header, "Format:    {format}");
    let _ = writeln!(header, "Created:   {created}");
    let _ = writeln!(header, "Source:    {source}");
    if capacity > 0 {
        let _ = writeln!(header, "Capacity:  {} ({} bytes)", human_bytes(capacity), capacity);
    } else {
        let _ = writeln!(header, "Capacity:  unknown");
    }
    match on_disk {
        Some(used) => {
            let pct = if capacity > 0 {
                (used as f64 / capacity as f64) * 100.0
            } else {
                0.0
            };
            let _ = writeln!(
                header,
                "On disk:   {} ({} bytes) — sparse {:.3}% of capacity",
                human_bytes(used),
                used,
                pct
            );
            let _ = writeln!(header, "Fill:      [{}] {:>5.1}%", bar_gauge(pct, 30), pct);
        }
        None => {
            let _ = writeln!(header, "On disk:   unavailable (cannot stat source)");
        }
    }
    let _ = writeln!(header, "\n== Inspect ==");

    Ok(format!("{header}{pretty}"))
}

fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} {}", UNITS[0])
    } else {
        format!("{v:.2} {}", UNITS[i])
    }
}

fn bar_gauge(pct: f64, width: usize) -> String {
    let pct = pct.clamp(0.0, 100.0);
    let filled = ((pct / 100.0) * width as f64).round() as usize;
    let mut s = String::with_capacity(width);
    for _ in 0..filled {
        s.push('█');
    }
    for _ in filled..width {
        s.push('░');
    }
    s
}

/// Network detail: pretty-printed `container network inspect <id>` plus a
/// header derived from the parsed config (mode, state, subnet, gateway,
/// nameservers).
pub async fn network_detail(id: &str) -> Result<String> {
    let bytes = run(&["network", "inspect", id]).await?;
    let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let v = parsed.get(0).cloned().unwrap_or_else(|| parsed.clone());
    let pretty = serde_json::to_string_pretty(&v)
        .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).into_owned());

    let cfg = v.get("config").cloned().unwrap_or(Value::Null);
    let status = v.get("status").cloned().unwrap_or(Value::Null);
    let mode = cfg.get("mode").and_then(|x| x.as_str()).unwrap_or("?");
    let state = v.get("state").and_then(|x| x.as_str()).unwrap_or("?");
    let plugin = cfg
        .get("pluginInfo")
        .and_then(|p| p.get("plugin"))
        .and_then(|x| x.as_str())
        .unwrap_or("?");
    let variant = cfg
        .get("pluginInfo")
        .and_then(|p| p.get("variant"))
        .and_then(|x| x.as_str())
        .unwrap_or("?");
    let v4_subnet = status
        .get("ipv4Subnet")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let v4_gw = status
        .get("ipv4Gateway")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let v6_subnet = status
        .get("ipv6Subnet")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let v6_gw = status
        .get("ipv6Gateway")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let nameservers: Vec<String> = status
        .get("nameservers")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let mut header = String::new();
    use std::fmt::Write as _;
    let _ = writeln!(header, "== Network: {id} ==");
    let _ = writeln!(header, "Mode:        {mode}");
    let _ = writeln!(header, "State:       {state}");
    let _ = writeln!(header, "Plugin:      {plugin} ({variant})");
    if !v4_subnet.is_empty() || !v4_gw.is_empty() {
        let _ = writeln!(
            header,
            "IPv4:        subnet {} · gateway {}",
            if v4_subnet.is_empty() { "—" } else { v4_subnet },
            if v4_gw.is_empty() { "—" } else { v4_gw }
        );
    }
    if !v6_subnet.is_empty() || !v6_gw.is_empty() {
        let _ = writeln!(
            header,
            "IPv6:        subnet {} · gateway {}",
            if v6_subnet.is_empty() { "—" } else { v6_subnet },
            if v6_gw.is_empty() { "—" } else { v6_gw }
        );
    }
    if !nameservers.is_empty() {
        let _ = writeln!(header, "Nameservers: {}", nameservers.join(", "));
    }
    let _ = writeln!(header, "\n== Inspect ==");

    Ok(format!("{header}{pretty}"))
}

/// Streaming trivy image scan. Captures the JSON report into `json_sink`
/// (one entry containing the full body) while echoing progress lines from
/// stderr into the visible op `sink`. The caller parses `json_sink` after
/// the task completes.
///
/// Requires `trivy` on PATH; the spawn fails cleanly if not.
pub fn spawn_trivy(
    image: String,
    sink: Arc<Mutex<Vec<String>>>,
    json_sink: Arc<Mutex<String>>,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        push(
            &sink,
            format!("$ trivy image --format json --severity HIGH,CRITICAL {image}"),
        );
        let mut child = match Command::new("trivy")
            .args(["image", "--format", "json", "--severity", "HIGH,CRITICAL", &image])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                push(&sink, format!("✗ failed to spawn trivy: {e}"));
                push(&sink, "  install with `brew install trivy`".into());
                return Err(anyhow!(e));
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let s_err = sink.clone();
        let json_sink_inner = json_sink.clone();

        // stdout is the JSON body — read it to a single String.
        let t_out = tokio::spawn(async move {
            if let Some(mut out) = stdout {
                use tokio::io::AsyncReadExt;
                let mut buf = Vec::new();
                let _ = out.read_to_end(&mut buf).await;
                if let Ok(mut g) = json_sink_inner.lock() {
                    *g = String::from_utf8_lossy(&buf).into_owned();
                }
            }
        });
        // stderr is human-readable progress — line-buffer it into the op log.
        let t_err = tokio::spawn(async move {
            if let Some(err) = stderr {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    push(&s_err, line);
                }
            }
        });
        let status = child.wait().await?;
        let _ = t_out.await;
        let _ = t_err.await;
        if status.success() {
            push(&sink, format!("✓ scanned {image}"));
            Ok(())
        } else {
            let msg = format!("✗ trivy exited {status}");
            push(&sink, msg.clone());
            Err(anyhow!(msg))
        }
    })
}

/// Pretty-printed `container inspect <id>` JSON. Falls back to raw stdout if
/// the response isn't valid JSON for any reason.
pub async fn inspect(id: &str) -> Result<String> {
    let bytes = run(&["inspect", id]).await?;
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) => Ok(serde_json::to_string_pretty(&v).unwrap_or_else(|_| {
            String::from_utf8_lossy(&bytes).into_owned()
        })),
        Err(_) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
    }
}

/// Streaming pull. Spawns `container image pull <reference>`, appends each
/// stdout/stderr line to `sink`, and reports completion via the returned
/// JoinHandle (Ok = success, Err = non-zero exit + last line).
pub fn spawn_pull(
    reference: String,
    sink: Arc<Mutex<Vec<String>>>,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        // `--progress=plain` was added in Apple container 0.12.0 and gives
        // us a stable line-based grammar parseable by `pullprog`. Other
        // runtimes (docker pull, podman pull, nerdctl pull) don't support
        // it and would error, so we gate by binary basename. The parser
        // falls back to its bare-`N/M` heuristics on those runtimes.
        let mut argv: Vec<&str> = vec!["image", "pull"];
        if is_apple_container() {
            argv.push("--progress=plain");
        }
        argv.push(&reference);
        push(
            &sink,
            format!("$ {} {}", bin(), argv.join(" ")),
        );
        let mut child = Command::new(bin())
            .args(&argv)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn pull {reference}"))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let s1 = sink.clone();
        let s2 = sink.clone();

        let t_out = tokio::spawn(async move {
            if let Some(out) = stdout {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    push(&s1, line);
                }
            }
        });
        let t_err = tokio::spawn(async move {
            if let Some(err) = stderr {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    push(&s2, line);
                }
            }
        });

        let status = child.wait().await?;
        let _ = t_out.await;
        let _ = t_err.await;

        if status.success() {
            push(&sink, format!("✓ pulled {reference}"));
            Ok(())
        } else {
            let msg = format!("✗ pull failed ({status})");
            push(&sink, msg.clone());
            Err(anyhow!(msg))
        }
    })
}

/// Streaming build. Runs `container build [-t <tag>] <context_path>` and
/// pipes both stdout and stderr line-by-line into the shared sink. Mirrors
/// `spawn_pull` so both can share the same modal renderer.
pub fn spawn_build(
    context_path: String,
    tag: Option<String>,
    sink: Arc<Mutex<Vec<String>>>,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        // `--progress=plain` works for Apple container 0.12+ and for
        // `docker build` (buildx default), but not for podman or nerdctl
        // build. Gate to keep things portable across the runtime profile.
        let mut args: Vec<String> = vec!["build".into()];
        if is_apple_container() {
            args.push("--progress=plain".into());
        }
        if let Some(ref t) = tag {
            args.push("-t".into());
            args.push(t.clone());
        }
        args.push(context_path.clone());
        push(
            &sink,
            format!("$ container {}", args.join(" ")),
        );
        let mut child = Command::new(bin())
            .args(args.iter().map(|s| s.as_str()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn build {context_path}"))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let s1 = sink.clone();
        let s2 = sink.clone();
        let t_out = tokio::spawn(async move {
            if let Some(out) = stdout {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    push(&s1, line);
                }
            }
        });
        let t_err = tokio::spawn(async move {
            if let Some(err) = stderr {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    push(&s2, line);
                }
            }
        });
        let status = child.wait().await?;
        let _ = t_out.await;
        let _ = t_err.await;
        if status.success() {
            push(&sink, format!("✓ built {}", tag.as_deref().unwrap_or(&context_path)));
            Ok(())
        } else {
            let msg = format!("✗ build failed ({status})");
            push(&sink, msg.clone());
            Err(anyhow!(msg))
        }
    })
}

fn push(sink: &Arc<Mutex<Vec<String>>>, line: String) {
    if let Ok(mut v) = sink.lock() {
        // Cap to avoid unbounded growth on a runaway pull.
        if v.len() >= 2000 {
            v.drain(0..1000);
        }
        v.push(line);
    }
}
