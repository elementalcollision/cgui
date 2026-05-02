//! `cgui doctor` — print a health check of the local environment so a user
//! can quickly tell whether the runtime CLI, profile config, theme, prefs,
//! and stacks dir are in working order.
//!
//! Exit code: 0 if every check passes, 1 if any fail.

use std::path::PathBuf;
use std::process::Command;

pub async fn run() -> i32 {
    let mut all_ok = true;
    let isatty = std::io::IsTerminal::is_terminal(&std::io::stdout());

    println!("{}", section("cgui doctor", isatty));

    // 1. Active runtime profile resolves.
    let profiles = crate::runtime::load_profiles();
    let saved = crate::prefs::Prefs::load().profile;
    let want = saved
        .clone()
        .or_else(crate::runtime::default_name)
        .unwrap_or_else(|| profiles[0].name.clone());
    let resolved = profiles.iter().find(|p| p.name == want);
    match resolved {
        Some(p) => {
            ok(isatty, &format!("active profile: {} → {}", p.name, p.binary));
            crate::runtime::set_active(p);
        }
        None => {
            all_ok = false;
            err(
                isatty,
                &format!("active profile '{want}' not found in profiles.toml"),
            );
        }
    }

    // 2. Runtime binary on PATH.
    let bin = crate::runtime::binary();
    let which = Command::new("which").arg(&bin).output();
    match which {
        Ok(o) if o.status.success() => {
            let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
            ok(isatty, &format!("`{bin}` resolves to {path}"));
        }
        _ => {
            all_ok = false;
            err(isatty, &format!("`{bin}` not on PATH"));
        }
    }

    // 3. Runtime --version.
    match Command::new(&bin).arg("--version").output() {
        Ok(o) if o.status.success() => {
            let v = String::from_utf8_lossy(&o.stdout).trim().to_string();
            ok(isatty, &format!("`{bin} --version` → {v}"));
        }
        _ => {
            all_ok = false;
            err(isatty, &format!("`{bin} --version` failed"));
        }
    }

    // 4. Runtime system status (Apple-container-specific; tolerated absent).
    if bin.ends_with("container") {
        match Command::new(&bin).args(["system", "status"]).output() {
            Ok(o) if o.status.success() => {
                let body = String::from_utf8_lossy(&o.stdout);
                let running = body.lines().any(|l| l.contains("running"));
                if running {
                    ok(isatty, "container system status: running");
                } else {
                    warn(isatty, "container system status not running — try `container system start`");
                }
            }
            _ => warn(isatty, "could not query `container system status` (not Apple container?)"),
        }
    }

    // 5. profiles.toml present + parses.
    let pp = config_path("profiles.toml");
    match pp.as_ref() {
        Some(p) if p.exists() => {
            if profiles.iter().all(|p| !p.name.is_empty()) {
                ok(isatty, &format!("profiles.toml: {} profile(s) at {}", profiles.len(), p.display()));
            } else {
                all_ok = false;
                err(isatty, &format!("profiles.toml at {} parsed but contains malformed entries", p.display()));
            }
        }
        Some(p) => warn(isatty, &format!("no profiles.toml at {} (using built-in default)", p.display())),
        None => warn(isatty, "no $XDG_CONFIG_HOME or $HOME — skipping config checks"),
    }

    // 6. theme.toml parses (optional).
    let tp = config_path("theme.toml");
    if let Some(p) = tp.as_ref() {
        if p.exists() {
            match std::fs::read_to_string(p) {
                Ok(s) => match toml::from_str::<toml::Value>(&s) {
                    Ok(_) => ok(isatty, &format!("theme.toml at {} parses cleanly", p.display())),
                    Err(e) => {
                        all_ok = false;
                        err(isatty, &format!("theme.toml at {} failed to parse: {e}", p.display()));
                    }
                },
                Err(e) => warn(isatty, &format!("could not read theme.toml: {e}")),
            }
        }
    }

    // 7. state.json readable (optional).
    let sp = config_path("state.json");
    if let Some(p) = sp.as_ref() {
        if p.exists() {
            match std::fs::read_to_string(p) {
                Ok(s) => match serde_json::from_str::<serde_json::Value>(&s) {
                    Ok(_) => ok(isatty, &format!("state.json at {} parses cleanly", p.display())),
                    Err(e) => {
                        all_ok = false;
                        err(isatty, &format!("state.json at {} failed to parse: {e}", p.display()));
                    }
                },
                Err(e) => warn(isatty, &format!("could not read state.json: {e}")),
            }
        }
    }

    // 8. stacks dir scan.
    let sd = config_path_dir("stacks");
    if let Some(d) = sd {
        if d.exists() {
            let n = std::fs::read_dir(&d)
                .ok()
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("toml"))
                        .count()
                })
                .unwrap_or(0);
            ok(isatty, &format!("stacks dir at {}: {} stack(s)", d.display(), n));
        }
    }

    // 9. trivy (optional).
    match Command::new("which").arg("trivy").output() {
        Ok(o) if o.status.success() => {
            let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
            ok(isatty, &format!("trivy: {path} (image scan available)"));
        }
        _ => warn(isatty, "trivy not on PATH (image scan disabled — `brew install trivy`)"),
    }

    // 10. update check.
    let mut prefs = crate::prefs::Prefs::load();
    if prefs.auto_update_check == Some(false) {
        warn(isatty, "update checks disabled (auto_update_check = false)");
    } else {
        let updates = crate::update::check(&mut prefs).await;
        if updates.is_empty() {
            ok(isatty, "all components up to date");
        } else {
            for u in &updates {
                warn(
                    isatty,
                    &format!(
                        "{} {} → {}   ({})",
                        u.component.label(),
                        u.installed,
                        u.latest,
                        u.release_url
                    ),
                );
            }
        }
    }

    println!();
    if all_ok {
        println!("{}", section("all checks passed", isatty));
        0
    } else {
        eprintln!("{}", section("one or more checks failed", isatty));
        1
    }
}

fn section(s: &str, color: bool) -> String {
    if color {
        format!("\x1b[1;36m== {s} ==\x1b[0m")
    } else {
        format!("== {s} ==")
    }
}
fn ok(color: bool, s: &str) {
    if color {
        println!("\x1b[32m✓\x1b[0m {s}");
    } else {
        println!("[ok]   {s}");
    }
}
fn warn(color: bool, s: &str) {
    if color {
        println!("\x1b[33m!\x1b[0m {s}");
    } else {
        println!("[warn] {s}");
    }
}
fn err(color: bool, s: &str) {
    if color {
        println!("\x1b[31m✗\x1b[0m {s}");
    } else {
        println!("[err]  {s}");
    }
}

fn config_path(name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("cgui").join(name))
}
fn config_path_dir(name: &str) -> Option<PathBuf> {
    config_path(name)
}
