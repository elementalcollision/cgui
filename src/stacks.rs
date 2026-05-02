//! Lightweight compose-style stacks. Each stack is a TOML file in
//! `$XDG_CONFIG_HOME/cgui/stacks/<name>.toml` describing 1..N services.
//! Bringing a stack up runs `container run -d --name <stack>_<svc> ...` per
//! service; bringing it down stops + deletes those containers.
//!
//! Schema:
//!
//! ```toml
//! name = "myapp"
//!
//! [[service]]
//! name = "db"
//! image = "docker.io/pgvector/pgvector:pg16"
//! env = { POSTGRES_USER = "test", POSTGRES_PASSWORD = "test" }
//! ports = ["15432:5432"]
//! volumes = ["dbdata:/var/lib/postgresql/data"]
//! network = "default"
//!
//! [[service]]
//! name = "api"
//! image = "myapp/api:latest"
//! depends_on = ["db"]
//! ```

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Deserialize)]
pub struct Stack {
    pub name: String,
    #[serde(rename = "service", default)]
    pub services: Vec<Service>,
    /// Path the stack was loaded from (None for synthesized stacks).
    #[serde(skip)]
    pub source: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Service {
    pub name: String,
    pub image: String,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(default)]
    pub volumes: Vec<String>,
    pub network: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// `"no"` (default), `"always"`, or `"on-failure"`. Anything else is
    /// treated as `"no"`.
    #[serde(default)]
    pub restart: Option<String>,
    pub healthcheck: Option<Healthcheck>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Healthcheck {
    /// `"tcp"` (default), `"http"`, or `"cmd"`.
    /// * `tcp` opens a TCP connection to the resolved port.
    /// * `http` issues `GET` over plain HTTP/1.0; success = status in
    ///   `expect_status` (default 200..399).
    /// * `cmd` runs `container exec <name> <command…>`; success = exit 0.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// For tcp/http: a port number, or `PORT/PATH`, or a full
    /// `http://host:port/path` URL. For cmd: ignored.
    #[serde(default)]
    pub target: Option<String>,
    /// For cmd: argv passed to `container exec`.
    #[serde(default)]
    pub command: Vec<String>,
    /// HTTP only: status range. `[200, 299]` or `[201]` etc. Default 200-399.
    #[serde(default)]
    pub expect_status: Vec<u16>,
    /// Seconds between checks. Default 30.
    #[serde(default = "default_interval")]
    pub interval_s: u64,
}

fn default_kind() -> String { "tcp".into() }
fn default_interval() -> u64 { 30 }

impl Service {
    pub fn restart_policy(&self) -> RestartPolicy {
        match self.restart.as_deref().unwrap_or("no") {
            "always" => RestartPolicy::Always,
            "on-failure" => RestartPolicy::OnFailure,
            _ => RestartPolicy::No,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RestartPolicy {
    No,
    Always,
    OnFailure,
}

/// Load every `*.toml` in the stacks dir.
pub fn load_all() -> Vec<Stack> {
    let Some(dir) = stacks_dir() else { return vec![] };
    let Ok(rd) = std::fs::read_dir(&dir) else { return vec![] };
    let mut out: Vec<Stack> = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|x| x.to_str()) != Some("toml") {
            continue;
        }
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Ok(mut stack) = toml::from_str::<Stack>(&s) {
                stack.source = Some(p);
                out.push(stack);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn stacks_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("cgui").join("stacks"))
}

/// Container name prefix: `<stack>_<service>`.
pub fn container_name(stack: &str, service: &str) -> String {
    format!("{stack}_{service}")
}

/// Build the `container run` argv for a single service.
pub fn run_args(stack: &str, svc: &Service) -> Vec<String> {
    let mut a: Vec<String> = vec!["run".into(), "-d".into(), "--name".into(), container_name(stack, &svc.name)];
    for (k, v) in &svc.env {
        a.push("-e".into());
        a.push(format!("{k}={v}"));
    }
    for p in &svc.ports {
        a.push("-p".into());
        a.push(p.clone());
    }
    for v in &svc.volumes {
        a.push("-v".into());
        a.push(v.clone());
    }
    if let Some(n) = &svc.network {
        a.push("--network".into());
        a.push(n.clone());
    }
    a.push(svc.image.clone());
    a.extend(svc.args.iter().cloned());
    a
}

/// Order services so `depends_on` comes before dependents (stable topo sort).
/// Cycles fall back to source order, since the operation will surface its
/// own error from `container run` anyway.
pub fn topo_order(stack: &Stack) -> Vec<&Service> {
    use std::collections::{HashMap, HashSet};
    let by_name: HashMap<&str, &Service> = stack
        .services
        .iter()
        .map(|s| (s.name.as_str(), s))
        .collect();
    let mut visited: HashSet<&str> = HashSet::new();
    let mut order: Vec<&Service> = Vec::new();
    fn visit<'a>(
        name: &'a str,
        by_name: &HashMap<&'a str, &'a Service>,
        visited: &mut HashSet<&'a str>,
        order: &mut Vec<&'a Service>,
    ) {
        if visited.contains(name) {
            return;
        }
        visited.insert(name);
        if let Some(s) = by_name.get(name) {
            for dep in &s.depends_on {
                visit(dep.as_str(), by_name, visited, order);
            }
            order.push(s);
        }
    }
    for s in &stack.services {
        visit(s.name.as_str(), &by_name, &mut visited, &mut order);
    }
    order
}

/// Spawn an "up" pipeline: run each service in dependency order, streaming
/// status lines into `sink`. Errors from any one service are surfaced but
/// the rest still run (mirrors compose's default behavior).
pub fn spawn_up(stack: Stack, sink: Arc<Mutex<Vec<String>>>) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        push(&sink, format!("$ stack up: {}", stack.name));
        let mut errors: Vec<String> = Vec::new();
        for svc in topo_order(&stack) {
            let args = run_args(&stack.name, svc);
            push(
                &sink,
                format!("→ {} ({}): container {}", svc.name, svc.image, args.join(" ")),
            );
            let bin = crate::runtime::binary();
            match tokio::process::Command::new(&bin)
                .args(args.iter().map(|s| s.as_str()))
                .output()
                .await
            {
                Ok(o) if o.status.success() => {
                    let id = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    push(&sink, format!("  ✓ {}", id));
                }
                Ok(o) => {
                    let msg = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    push(&sink, format!("  ✗ {}: {}", svc.name, msg));
                    errors.push(svc.name.clone());
                }
                Err(e) => {
                    push(&sink, format!("  ✗ {}: spawn error: {}", svc.name, e));
                    errors.push(svc.name.clone());
                }
            }
        }
        if errors.is_empty() {
            push(&sink, format!("✓ stack up: {} ({} services)", stack.name, stack.services.len()));
            Ok(())
        } else {
            let msg = format!("✗ stack up partial: {} failed", errors.join(", "));
            push(&sink, msg.clone());
            Err(anyhow!(msg))
        }
    })
}

/// Spawn a "down" pipeline: stop+delete every service container in reverse
/// dependency order. Missing containers are not errors (down is idempotent).
pub fn spawn_down(stack: Stack, sink: Arc<Mutex<Vec<String>>>) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        push(&sink, format!("$ stack down: {}", stack.name));
        let bin = crate::runtime::binary();
        let order: Vec<&Service> = topo_order(&stack);
        for svc in order.into_iter().rev() {
            let name = container_name(&stack.name, &svc.name);
            // Stop (ignore errors).
            let _ = tokio::process::Command::new(&bin)
                .args(["stop", &name])
                .output()
                .await;
            // Delete (ignore "not found").
            match tokio::process::Command::new(&bin)
                .args(["delete", &name])
                .output()
                .await
            {
                Ok(o) if o.status.success() => {
                    push(&sink, format!("  ✓ rm {name}"));
                }
                Ok(_) => push(&sink, format!("  · {name} already gone")),
                Err(e) => push(&sink, format!("  ✗ {name}: {e}")),
            }
        }
        push(&sink, format!("✓ stack down: {}", stack.name));
        Ok(())
    })
}

fn push(sink: &Arc<Mutex<Vec<String>>>, line: String) {
    if let Ok(mut v) = sink.lock() {
        if v.len() >= 2000 {
            v.drain(0..1000);
        }
        v.push(line);
    }
}

/// Path of the stack file `<dir>/<name>.toml`. Returns None if no config dir.
pub fn path_for(name: &str) -> Option<PathBuf> {
    stacks_dir().map(|d| d.join(format!("{name}.toml")))
}

/// Create a new stack file with a starter template. Returns the path on
/// success; errors if the file already exists or no config dir is available.
pub fn create_template(name: &str) -> Result<PathBuf> {
    let Some(dir) = stacks_dir() else {
        return Err(anyhow!("no XDG_CONFIG_HOME or HOME — can't write stack"));
    };
    std::fs::create_dir_all(&dir).context("create stacks dir")?;
    let p = dir.join(format!("{name}.toml"));
    if p.exists() {
        return Err(anyhow!("stack '{name}' already exists at {}", p.display()));
    }
    let body = format!(
        r#"# cgui stack — bring up with `u`, tear down with `D`.
name = "{name}"

[[service]]
name = "app"
image = "docker.io/library/alpine:latest"
# env = {{ KEY = "value" }}
# ports = ["8080:80"]
# volumes = ["mydata:/data"]
# network = "default"
# depends_on = []
# args = ["sh", "-c", "while true; do date; sleep 5; done"]
"#
    );
    std::fs::write(&p, body).context("write stack template")?;
    Ok(p)
}

// --- Stack templates: scaffolds for `cgui new <name> --template <kind>`.
//
// Each template is a TOML body string with a `{name}` placeholder for the
// stack name. We deliberately don't ship a templating engine — these are
// short, hand-tuned starting points the user is expected to edit.

pub struct Template {
    pub name: &'static str,
    pub description: &'static str,
    pub body: &'static str,
}

pub const TEMPLATES: &[Template] = &[
    Template {
        name: "blank",
        description: "Single Alpine service skeleton",
        body: r#"# {name}: blank single-service template
name = "{name}"

[[service]]
name = "app"
image = "docker.io/library/alpine:latest"
# args = ["sh", "-c", "while true; do date; sleep 5; done"]
# env = { KEY = "value" }
# ports = ["8080:80"]
# restart = "always"
"#,
    },
    Template {
        name: "postgres",
        description: "Postgres + pgvector with healthcheck",
        body: r#"# {name}: postgres single-service stack
name = "{name}"

[[service]]
name = "db"
image = "docker.io/pgvector/pgvector:pg16"
env = { POSTGRES_USER = "app", POSTGRES_PASSWORD = "app", POSTGRES_DB = "app" }
ports = ["15432:5432"]
volumes = ["{name}-pgdata:/var/lib/postgresql/data"]
restart = "always"

[service.healthcheck]
kind = "tcp"
target = "15432"
interval_s = 30
"#,
    },
    Template {
        name: "postgres+api",
        description: "Postgres + an API skeleton with depends_on",
        body: r#"# {name}: postgres + api stack — replace api image with your build
name = "{name}"

[[service]]
name = "db"
image = "docker.io/pgvector/pgvector:pg16"
env = { POSTGRES_USER = "app", POSTGRES_PASSWORD = "app", POSTGRES_DB = "app" }
ports = ["15432:5432"]
volumes = ["{name}-pgdata:/var/lib/postgresql/data"]
restart = "always"

[service.healthcheck]
kind = "tcp"
target = "15432"

[[service]]
name = "api"
image = "docker.io/library/alpine:latest"   # replace with your image
depends_on = ["db"]
network = "default"
ports = ["8080:8080"]
env = { DATABASE_URL = "postgres://app:app@host.docker.internal:15432/app" }
restart = "on-failure"

[service.healthcheck]
kind = "http"
target = "8080/healthz"
interval_s = 30
"#,
    },
    Template {
        name: "redis",
        description: "Redis cache",
        body: r#"# {name}: redis stack
name = "{name}"

[[service]]
name = "cache"
image = "docker.io/library/redis:7-alpine"
ports = ["16379:6379"]
volumes = ["{name}-redis:/data"]
restart = "always"

[service.healthcheck]
kind = "cmd"
command = ["redis-cli", "ping"]
interval_s = 30
"#,
    },
    Template {
        name: "nginx",
        description: "Nginx web server with bind-mountable conf",
        body: r#"# {name}: nginx stack
name = "{name}"

[[service]]
name = "web"
image = "docker.io/library/nginx:1.27-alpine"
ports = ["8080:80"]
# volumes = ["./nginx.conf:/etc/nginx/conf.d/default.conf:ro"]
restart = "always"

[service.healthcheck]
kind = "http"
target = "8080/"
expect_status = [200, 404]
"#,
    },
];

pub fn template_by_name(s: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.name == s)
}

// --- Live diff: stack TOML vs running container ---

/// One row in the diff: a single (kind, expected, actual) triple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffRow {
    /// Container exists, field matches the stack file.
    Match { service: String, field: String, value: String },
    /// Container exists, field differs.
    Differ {
        service: String,
        field: String,
        expected: String,
        actual: String,
    },
    /// Container doesn't exist for this service at all.
    Missing { service: String, expected_image: String },
    /// Container exists but isn't running.
    NotRunning { service: String, status: String },
}

/// Compare what a stack file declares against what the runtime reports for
/// each `<stack>_<service>` container. Async because each service triggers
/// a `container inspect` call. Errors during inspect become Missing rows.
pub async fn diff_against_runtime(stack: &Stack) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for svc in &stack.services {
        let cname = container_name(&stack.name, &svc.name);
        let actual = match inspect_container(&cname).await {
            Some(v) => v,
            None => {
                rows.push(DiffRow::Missing {
                    service: svc.name.clone(),
                    expected_image: svc.image.clone(),
                });
                continue;
            }
        };

        // Status row first — sets context for the rest.
        let status = actual
            .get("status")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .to_string();
        if status != "running" {
            rows.push(DiffRow::NotRunning {
                service: svc.name.clone(),
                status: status.clone(),
            });
        }

        let cfg = actual.get("configuration").cloned().unwrap_or_default();

        // image
        let actual_image = cfg
            .get("image")
            .and_then(|i| i.get("reference"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        push_compare(
            &mut rows,
            &svc.name,
            "image",
            &svc.image,
            &actual_image,
        );

        // ports — sort both sides and compare as lists
        let actual_ports = cfg
            .get("publishedPorts")
            .and_then(|x| x.as_array())
            .map(|arr| {
                let mut v: Vec<String> = arr
                    .iter()
                    .map(|p| {
                        let host = p.get("hostPort").and_then(|x| x.as_u64()).unwrap_or(0);
                        let cont = p.get("containerPort").and_then(|x| x.as_u64()).unwrap_or(0);
                        format!("{host}:{cont}")
                    })
                    .collect();
                v.sort();
                v
            })
            .unwrap_or_default();
        let mut expected_ports = svc.ports.clone();
        expected_ports.sort();
        push_compare(
            &mut rows,
            &svc.name,
            "ports",
            &expected_ports.join(", "),
            &actual_ports.join(", "),
        );

        // env — extract KEY=VALUE pairs from initProcess.environment, intersect on KEY
        let actual_env: std::collections::BTreeMap<String, String> = cfg
            .get("initProcess")
            .and_then(|p| p.get("environment"))
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .filter_map(|s| s.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        for (k, expected) in &svc.env {
            let actual = actual_env.get(k).cloned().unwrap_or_default();
            push_compare(
                &mut rows,
                &svc.name,
                &format!("env[{k}]"),
                expected,
                &actual,
            );
        }

        // network — first attached
        let actual_net = cfg
            .get("networks")
            .and_then(|x| x.as_array())
            .and_then(|arr| arr.first())
            .and_then(|n| n.get("network"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(expected_net) = &svc.network {
            push_compare(&mut rows, &svc.name, "network", expected_net, &actual_net);
        }
    }
    rows
}

fn push_compare(
    rows: &mut Vec<DiffRow>,
    service: &str,
    field: &str,
    expected: &str,
    actual: &str,
) {
    if expected == actual {
        rows.push(DiffRow::Match {
            service: service.to_string(),
            field: field.to_string(),
            value: expected.to_string(),
        });
    } else {
        rows.push(DiffRow::Differ {
            service: service.to_string(),
            field: field.to_string(),
            expected: expected.to_string(),
            actual: actual.to_string(),
        });
    }
}

async fn inspect_container(name: &str) -> Option<serde_json::Value> {
    let bin = crate::runtime::binary();
    let out = tokio::process::Command::new(bin)
        .args(["inspect", name])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let arr = v.as_array()?;
    arr.first().cloned()
}

/// Render the chosen template with `{name}` interpolated.
pub fn render_template(t: &Template, name: &str) -> String {
    t.body.replace("{name}", name)
}

/// Create a new stack file from a named template (or "blank" if unspecified).
/// Errors if the destination already exists or if `name` contains characters
/// not safe for a filename (alnum / `-` / `_`).
pub fn create_from_template(name: &str, template_name: Option<&str>) -> Result<PathBuf> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(anyhow!(
            "stack name must be ASCII alphanumeric, '-', or '_' (got: {name:?})"
        ));
    }
    let t_name = template_name.unwrap_or("blank");
    let template = template_by_name(t_name).ok_or_else(|| {
        anyhow!(
            "unknown template: {t_name}. Known: {}",
            TEMPLATES.iter().map(|x| x.name).collect::<Vec<_>>().join(", ")
        )
    })?;
    let Some(dir) = stacks_dir() else {
        return Err(anyhow!("no XDG_CONFIG_HOME or HOME — can't write stack"));
    };
    std::fs::create_dir_all(&dir).context("create stacks dir")?;
    let p = dir.join(format!("{name}.toml"));
    if p.exists() {
        return Err(anyhow!("stack '{name}' already exists at {}", p.display()));
    }
    std::fs::write(&p, render_template(template, name)).context("write stack from template")?;
    Ok(p)
}

/// Best-effort: write a sample stack file the first time the user opens the
/// Stacks tab and it's empty, so they have something to play with.
pub fn ensure_sample() -> Result<Option<PathBuf>> {
    let Some(dir) = stacks_dir() else { return Ok(None) };
    std::fs::create_dir_all(&dir).context("create stacks dir")?;
    let p = dir.join("example.toml");
    if p.exists() {
        return Ok(None);
    }
    let body = r#"# cgui example stack — bring up with `u`, tear down with `D`.
name = "example"

[[service]]
name = "db"
image = "docker.io/pgvector/pgvector:pg16"
env = { POSTGRES_USER = "test", POSTGRES_PASSWORD = "test" }
ports = ["15432:5432"]
"#;
    std::fs::write(&p, body).context("write example stack")?;
    Ok(Some(p))
}

