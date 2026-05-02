//! Permissive parser for `container image pull` and `container build`
//! progress output.
//!
//! As of Apple `container 0.12.x`, both subcommands accept `--progress=plain`
//! and emit a stable line-based grammar:
//!
//! ```text
//! [STEP/TOTAL] Phase NN% (X of Y blobs, Z UNIT/T UNIT, R UNIT/s) [Ts]
//! ```
//!
//! Concrete examples (captured live against `0.12.3`):
//!
//! ```text
//! [1/2] Fetching image [0s]
//! [1/2] Fetching image (13 of 23 blobs) [1s]
//! [1/2] Fetching image 89% (69 of 71 blobs, 280.4/311.7 MB, 108.3 MB/s) [6s]
//! [2/2] Unpacking image [6s]
//! [2/2] Unpacking image for platform linux/arm64/v8 0% [8s]
//! ```
//!
//! Each line carries up to three signals:
//! 1. a **step ratio** `[N/M]` — coarse phase progress
//! 2. an **inner percent** `NN%` — fine-grained progress within the phase
//! 3. a **byte ratio** `Z/T UNIT` — fine-grained download progress
//!
//! We compose them: when both step and inner percent exist, overall progress
//! is `(step - 1 + inner_pct) / total`. This monotonically advances across
//! phases instead of restarting each phase at 0%. When only one signal is
//! present we fall back gracefully. Lines with only a `[N/M]` step prefix
//! are deliberately *not* used as progress on their own — they're too
//! coarse and used to mis-report 50% for "Fetching image" before any blob
//! had downloaded.
//!
//! Returns a fraction in [0.0, 1.0].

pub fn parse_progress(lines: &[String]) -> Option<f64> {
    for line in lines.iter().rev() {
        if let Some(p) = parse_line(line) {
            return Some(clamp(p));
        }
    }
    None
}

/// All-in-one parser for one line. Combines step ratio + the strongest
/// per-phase signal (inner percent ▷ byte ratio ▷ blob ratio) into one
/// monotonically-increasing fraction.
fn parse_line(line: &str) -> Option<f64> {
    let step = parse_step_prefix(line);
    // Strip the `[N/M] ` prefix so per-phase signals don't pick up the
    // step ratio by accident.
    let body = match step {
        Some((_, _, end)) => &line[end..],
        None => line,
    };

    // Strongest fine-grained signal in the body, in decreasing reliability.
    // Blob ratio (`N of M blobs`) is deliberately excluded — it tracks
    // discovery, not work-done, and would cause the gauge to regress when
    // the next snapshot starts reporting actual byte percentage.
    let inner: Option<f64> = parse_percent(body)
        .or_else(|| parse_byte_ratio(body))
        // Defensive fallback for non-Apple runtimes (docker/podman style
        // "layer 3/8" output).
        .or_else(|| parse_int_ratio(body));

    match (step, inner) {
        (Some((s, t, _)), Some(p)) if t > 0 => {
            // Compose: phase s of t with internal completion p.
            // Treats step 1 of 2 at 0% as 0%, step 2 of 2 at 0% as 50%.
            let s = (s as f64).max(1.0);
            let t = t as f64;
            Some(((s - 1.0) + p) / t)
        }
        (None, Some(p)) => Some(p),
        // Bare step prefix (no inner signal) is intentionally not progress;
        // skip and let an older line with real data win.
        _ => None,
    }
}

/// Recognise a leading `[N/M] ` step prefix. Returns `(N, M, end_byte)` on
/// match. The end is the byte index immediately after the closing `]` and
/// any trailing space, so callers can slice past it.
fn parse_step_prefix(line: &str) -> Option<(u32, u32, usize)> {
    let bytes = line.as_bytes();
    if bytes.first() != Some(&b'[') {
        return None;
    }
    let mut i = 1;
    let n_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == n_start || bytes.get(i) != Some(&b'/') {
        return None;
    }
    let n: u32 = std::str::from_utf8(&bytes[n_start..i]).ok()?.parse().ok()?;
    i += 1;
    let m_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == m_start || bytes.get(i) != Some(&b']') {
        return None;
    }
    let m: u32 = std::str::from_utf8(&bytes[m_start..i]).ok()?.parse().ok()?;
    i += 1;
    // Eat one trailing space if present.
    if bytes.get(i) == Some(&b' ') {
        i += 1;
    }
    Some((n, m, i))
}

/// Find an `(N of M …)` blob-style ratio. Apple's plain output uses
/// `(13 of 23 blobs)` and `(34 of 71 blobs, 55 KB/311.7 MB, 15 KB/s)`.
///
/// Deliberately not in the progress-signal chain (it tracks discovery,
/// not work-done, and would cause the gauge to regress). Kept callable
/// for future status-display features and tested for correctness.
#[allow(dead_code)]
pub fn parse_blob_ratio(line: &str) -> Option<f64> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'(' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            let n_start = j;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > n_start {
                let n: u64 = std::str::from_utf8(&bytes[n_start..j]).ok()?.parse().ok()?;
                // Expect " of "
                let needle = b" of ";
                if bytes.len() >= j + needle.len() && &bytes[j..j + needle.len()] == needle {
                    let mut k = j + needle.len();
                    let m_start = k;
                    while k < bytes.len() && bytes[k].is_ascii_digit() {
                        k += 1;
                    }
                    if k > m_start {
                        let m: u64 =
                            std::str::from_utf8(&bytes[m_start..k]).ok()?.parse().ok()?;
                        if m > 0 && n <= m {
                            return Some(n as f64 / m as f64);
                        }
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// A short status snippet (last non-empty line) suitable for the gauge label.
pub fn status_label(lines: &[String]) -> String {
    lines
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty())
        .cloned()
        .unwrap_or_default()
}

fn clamp(p: f64) -> f64 {
    p.clamp(0.0, 1.0)
}

fn parse_percent(line: &str) -> Option<f64> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            // Skip optional whitespace before %.
            let mut j = i;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'%' {
                if let Ok(n) = std::str::from_utf8(&bytes[start..i]) {
                    if let Ok(v) = n.parse::<f64>() {
                        return Some(v / 100.0);
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Find a `<num><unit>/<num><unit>` ratio anywhere in the line. Returns the
/// resulting fraction. Units: B, KB, MB, GB, TB, KiB, MiB, GiB, TiB.
fn parse_byte_ratio(line: &str) -> Option<f64> {
    // Walk substrings around any '/' character.
    for (slash, _) in line.match_indices('/') {
        let lhs = parse_size_ending_at(&line[..slash])?;
        let rhs = parse_size_starting_at(&line[slash + 1..])?;
        if rhs > 0.0 {
            return Some(lhs / rhs);
        }
    }
    None
}

/// Pull a number+unit ending at the rightmost char of `s` (ignoring trailing
/// whitespace).
fn parse_size_ending_at(s: &str) -> Option<f64> {
    let trimmed = s.trim_end();
    let bytes = trimmed.as_bytes();
    // Consume unit letters from the end.
    let mut end = bytes.len();
    let unit_end = end;
    while end > 0 && bytes[end - 1].is_ascii_alphabetic() {
        end -= 1;
    }
    let unit = &trimmed[end..unit_end];
    if unit.is_empty() {
        return None;
    }
    let mult = unit_multiplier(unit)?;
    // Eat optional whitespace between the number and the unit (Apple's
    // plain output uses `55 KB`, with a space).
    while end > 0 && bytes[end - 1] == b' ' {
        end -= 1;
    }
    let digit_end = end;
    while end > 0 && (bytes[end - 1].is_ascii_digit() || bytes[end - 1] == b'.') {
        end -= 1;
    }
    let num = &trimmed[end..digit_end];
    if num.is_empty() {
        return None;
    }
    num.parse::<f64>().ok().map(|v| v * mult)
}

/// Pull a number+unit starting at the first non-whitespace char of `s`.
fn parse_size_starting_at(s: &str) -> Option<f64> {
    let trimmed = s.trim_start();
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let num: f64 = trimmed[..i].parse().ok()?;
    // Eat optional whitespace between the number and the unit.
    let mut j = i;
    while j < bytes.len() && bytes[j] == b' ' {
        j += 1;
    }
    let unit_start = j;
    while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
        j += 1;
    }
    let unit = &trimmed[unit_start..j];
    let mult = unit_multiplier(unit)?;
    Some(num * mult)
}

fn unit_multiplier(unit: &str) -> Option<f64> {
    Some(match unit {
        "B" => 1.0,
        "KB" => 1_000.0,
        "MB" => 1_000_000.0,
        "GB" => 1_000_000_000.0,
        "TB" => 1_000_000_000_000.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0_f64.powi(3),
        "TiB" => 1024.0_f64.powi(4),
        _ => return None,
    })
}

/// Layer counts: `3/8` (no units, plausibly small).
fn parse_int_ratio(line: &str) -> Option<f64> {
    for (slash, _) in line.match_indices('/') {
        let lhs = trailing_int(&line[..slash])?;
        let rhs = leading_int(&line[slash + 1..])?;
        if rhs > 0 && rhs < 10_000 && lhs <= rhs {
            return Some(lhs as f64 / rhs as f64);
        }
    }
    None
}

fn trailing_int(s: &str) -> Option<u64> {
    let bytes = s.trim_end().as_bytes();
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1].is_ascii_digit() {
        end -= 1;
    }
    if end == bytes.len() {
        return None;
    }
    std::str::from_utf8(&bytes[end..]).ok()?.parse().ok()
}

fn leading_int(s: &str) -> Option<u64> {
    let bytes = s.trim_start().as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    std::str::from_utf8(&bytes[..i]).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }
    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "expected ~{b}, got {a}");
    }

    #[test]
    fn percent() {
        assert_eq!(parse_progress(&lines(&["downloading 42%"])), Some(0.42));
        assert_eq!(parse_progress(&lines(&["downloading 42.5 %"])), Some(0.425));
    }
    #[test]
    fn byte_ratio() {
        let p = parse_progress(&lines(&["pulling 12MB/24MB layer abc"])).unwrap();
        approx(p, 0.5);
        let p = parse_progress(&lines(&["1.5GiB/3GiB"])).unwrap();
        approx(p, 0.5);
    }
    #[test]
    fn int_ratio_fallback() {
        // Non-Apple runtimes (docker/podman) — bare layer ratio without units.
        assert_eq!(parse_progress(&lines(&["layers 3/8"])), Some(3.0 / 8.0));
    }
    #[test]
    fn newest_wins() {
        assert_eq!(
            parse_progress(&lines(&["10%", "50%", "ignore me"])),
            Some(0.5)
        );
    }
    #[test]
    fn nothing() {
        assert_eq!(parse_progress(&lines(&["nothing matches here"])), None);
    }

    // --- Apple `container 0.12.x --progress=plain` format ---

    #[test]
    fn apple_step_prefix() {
        assert_eq!(parse_step_prefix("[1/2] Fetching image"), Some((1, 2, 6)));
        assert_eq!(parse_step_prefix("[2/2] Unpacking [6s]"), Some((2, 2, 6)));
        assert_eq!(parse_step_prefix("[10/12] Foo"), Some((10, 12, 8)));
        assert_eq!(parse_step_prefix("not a prefix"), None);
        assert_eq!(parse_step_prefix("[abc/2]"), None);
    }

    #[test]
    fn apple_blob_ratio() {
        approx(parse_blob_ratio("(13 of 23 blobs)").unwrap(), 13.0 / 23.0);
        approx(
            parse_blob_ratio("(34 of 71 blobs, 55 KB/311.7 MB, 15 KB/s)").unwrap(),
            34.0 / 71.0,
        );
        assert_eq!(parse_blob_ratio("no parens here"), None);
    }

    #[test]
    fn apple_compound_step_one_at_zero() {
        // "[1/2] Fetching image 0% ..." → step 1 of 2 at 0% = 0% overall
        let p =
            parse_progress(&lines(&["[1/2] Fetching image 0% (34 of 71 blobs, 55 KB/311.7 MB, 15 KB/s) [2s]"]))
                .unwrap();
        approx(p, 0.0);
    }

    #[test]
    fn apple_compound_step_one_at_89() {
        // "[1/2] Fetching image 89% ..." → step 1 of 2 at 89% = 44.5% overall
        let p = parse_progress(&lines(&[
            "[1/2] Fetching image 89% (69 of 71 blobs, 280.4/311.7 MB, 108.3 MB/s) [6s]",
        ]))
        .unwrap();
        approx(p, 0.445);
    }

    #[test]
    fn apple_compound_step_two_at_zero() {
        // "[2/2] Unpacking image for platform linux/arm64/v8 0% [8s]"
        //  → step 2 of 2 at 0% = 50% overall
        let p =
            parse_progress(&lines(&["[2/2] Unpacking image for platform linux/arm64/v8 0% [8s]"]))
                .unwrap();
        approx(p, 0.5);
    }

    #[test]
    fn apple_blob_only_no_progress() {
        // Early line with only a blob ratio (no %, no bytes) yields no
        // progress — blob count tracks discovery, not work-done. Letting
        // it through caused the gauge to regress when the next snapshot
        // started reporting actual byte percentage. parse_blob_ratio is
        // still callable for status display, just not in the chain.
        assert_eq!(
            parse_progress(&lines(&["[1/2] Fetching image (13 of 23 blobs) [1s]"])),
            None
        );
        // The function itself still works for callers that want it.
        approx(parse_blob_ratio("(13 of 23 blobs)").unwrap(), 13.0 / 23.0);
    }

    #[test]
    fn apple_bare_step_prefix_skipped() {
        // "[1/2] Fetching image [0s]" has no inner signal at all.
        // We DELIBERATELY do not return 50% from the bare step prefix.
        // (Old parser bug: int_ratio caught the [1/2] and returned 0.5.)
        assert_eq!(
            parse_progress(&lines(&["[1/2] Fetching image [0s]"])),
            None
        );
    }

    #[test]
    fn apple_full_pull_monotonic() {
        // Walking through a real captured pull, parsed newest-first as the
        // gauge would see it. Each successive snapshot should be >= the prior.
        let snapshots = [
            "[1/2] Fetching image [0s]",
            "[1/2] Fetching image (13 of 23 blobs) [1s]",
            "[1/2] Fetching image 0% (34 of 71 blobs, 55 KB/311.7 MB, 15 KB/s) [2s]",
            "[1/2] Fetching image 89% (69 of 71 blobs, 280.4/311.7 MB, 108.3 MB/s) [6s]",
            "[2/2] Unpacking image [6s]",
            "[2/2] Unpacking image for platform linux/arm64/v8 0% [8s]",
        ];
        let mut last: f64 = -1.0;
        for (i, snap) in snapshots.iter().enumerate() {
            // Skip lines that intentionally yield no progress (no inner
            // signal); the gauge in those cases keeps showing the prior
            // value, which is still monotonic.
            if let Some(p) = parse_progress(&lines(&[snap])) {
                assert!(p >= last, "snapshot {i}: {snap:?} → {p} regressed from {last}");
                last = p;
            }
        }
        // Final reachable value should be at least step 2 of 2 (≥ 0.5).
        assert!(last >= 0.5, "final progress {last} should be >= 0.5");
    }

    #[test]
    fn apple_step_with_byte_ratio() {
        // Older Apple lines may emit byte ratio without explicit %.
        let p = parse_progress(&lines(&["[1/2] Fetching image (50 MB/100 MB)"]))
            .unwrap();
        // step 1/2 at 50% inner = 25% overall
        approx(p, 0.25);
    }
}
