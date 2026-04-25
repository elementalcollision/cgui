//! Thin async wrapper around Apple's `container` CLI.
//!
//! We always invoke `--format json` and decode into permissive types, so the
//! rest of the app doesn't have to care about the CLI's exact shape across
//! versions.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;

const BIN: &str = "container";

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

async fn run(args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new(BIN)
        .args(args)
        .output()
        .await
        .with_context(|| format!("failed to spawn `{} {}`", BIN, args.join(" ")))?;
    if !out.status.success() {
        return Err(anyhow!(
            "`{} {}` exited {}: {}",
            BIN,
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

pub async fn logs(id: &str, tail: usize) -> Result<String> {
    // `container logs` doesn't take --tail in all versions; pull then trim.
    let out = Command::new(BIN).args(["logs", id]).output().await?;
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
