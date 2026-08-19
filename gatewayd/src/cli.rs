//! gatewayd/src/cli.rs
//!
//! User-facing CLI commands (Phase 7). So far one command:
//!
//! ```text
//! gatewayd --journal [--db PATH] [--limit N] [--level info|warn|error]
//!                   [--category NAME] [--since PERIOD]
//! ```
//!
//! Shows the durable journal as a terminal table, newest first. `--since`
//! accepts `10m`, `6h`, `1d`, `2w`, `1mo` (month = 30 days). The CLI opens
//! the journal database read-only, so it works whether the gateway is
//! running or stopped.
//!
//! Next step (approved by the user): the same CLI will list agents waiting
//! for approval and accept/reject them — one window for viewing and deciding.

use std::path::PathBuf;

use gatewayd::approvals::{ApprovalRecord, ApprovalStore, Status};
use gatewayd::journal::{query_recent, JournalFilter, JournalRecord};

/// Entry point for `gatewayd --journal ...`. Returns Ok(()) after printing.
pub fn run_journal(args: &[String]) -> anyhow::Result<()> {
    let mut db = PathBuf::from("/tmp/gateway/journal.db");
    let mut limit: Option<u64> = None;
    let mut level: Option<String> = None;
    let mut category: Option<String> = None;
    let mut since: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let value = args.get(i + 1).map(|s| s.as_str());
        match arg.as_str() {
            "--db" => {
                let v = value.ok_or_else(|| anyhow::anyhow!("--db: expected a path"))?;
                db = PathBuf::from(v);
                i += 1;
            }
            "--limit" => {
                let v = value.ok_or_else(|| anyhow::anyhow!("--limit: expected a number"))?;
                limit = Some(
                    v.parse::<u64>()
                        .map_err(|_| anyhow::anyhow!("--limit: expected a number, got {v}"))?,
                );
                i += 1;
            }
            "--level" => {
                let v = value.ok_or_else(|| anyhow::anyhow!("--level: expected info|warn|error"))?;
                if !matches!(v, "info" | "warn" | "error") {
                    anyhow::bail!("--level: expected info|warn|error, got {v}");
                }
                level = Some(v.to_string());
                i += 1;
            }
            "--category" => {
                let v = value.ok_or_else(|| anyhow::anyhow!("--category: expected a name"))?;
                category = Some(v.to_string());
                i += 1;
            }
            "--since" => {
                let v = value.ok_or_else(|| anyhow::anyhow!("--since: expected a period"))?;
                since = Some(v.to_string());
                i += 1;
            }
            other => anyhow::bail!("--journal: unknown argument {other}"),
        }
        i += 1;
    }

    let since_secs = match &since {
        Some(s) => Some(parse_period(s)?),
        None => None,
    };

    if !db.exists() {
        anyhow::bail!(
            "journal not found: {} (is --db pointing at the right file?)",
            db.display()
        );
    }

    let filter = JournalFilter {
        limit,
        level,
        category,
        since_secs,
    };
    let records = query_recent(&db, &filter)?;
    print_table(&records);
    Ok(())
}

/// Entry point for the approvals CLI:
/// `gatewayd --approvals [--db PATH]`, `gatewayd --approve <name> [--db PATH]`,
/// `gatewayd --reject <name> [--db PATH]`.
pub fn run_approvals(command: &str, args: &[String]) -> anyhow::Result<()> {
    let mut db = PathBuf::from("/tmp/gateway/approvals.db");
    let mut name: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--db" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow::anyhow!("--db: expected a path"))?;
                db = PathBuf::from(v);
                i += 1;
            }
            other if other.starts_with("--") => {
                anyhow::bail!("{command}: unknown argument {other}")
            }
            other => {
                name = Some(other.to_string());
            }
        }
        i += 1;
    }

    let store = ApprovalStore::open(&db)?;
    match command {
        "--approvals" => {
            let list = store.list()?;
            print_approvals(&list);
            Ok(())
        }
        "--approve" | "--reject" => {
            let agent = name
                .ok_or_else(|| anyhow::anyhow!("{command}: expected agent name"))?;
            let status = if command == "--approve" {
                Status::Approved
            } else {
                Status::Rejected
            };
            store.set(&agent, status)?;
            let verb = status.as_str();
            println!("{verb}: {agent}");
            Ok(())
        }
        _ => unreachable!("run_approvals called with {command}"),
    }
}

fn print_approvals(list: &[ApprovalRecord]) {
    if list.is_empty() {
        println!("(no approval records yet — agents appear here on first gateway start)");
        return;
    }
    let mut id_w = 9usize;
    let mut st_w = 8usize;
    for r in list {
        id_w = id_w.max(r.agent_id.len());
        st_w = st_w.max(r.status.as_str().len());
    }
    let fp_w = list.iter().map(|r| r.fingerprint.len().max(11)).max().unwrap_or(11);
    let time_w = "YYYY-MM-DD HH:MM:SS UTC".len();

    let sep = |w: usize| "-".repeat(w);
    println!(
        " {:<id$} | {:<st$} | {:<fp$} | {:<tw$} | {:<tw$} ",
        "AGENT_ID",
        "STATUS",
        "FINGERPRINT",
        "DECIDED (UTC)",
        "LAST SEEN (UTC)",
        id = id_w,
        st = st_w,
        fp = fp_w,
        tw = time_w,
    );
    println!(
        " {}-+-{}-+-{}-+-{}-+-{}- ",
        sep(id_w),
        sep(st_w),
        sep(fp_w),
        sep(time_w),
        sep(time_w),
    );
    for r in list {
        let decided = r
            .decided_at
            .as_deref()
            .map(|s| unix_to_utc(s.parse::<u64>().unwrap_or(0)))
            .unwrap_or_else(|| "—".to_string());
        let last_seen = unix_to_utc(r.last_seen_at.parse::<u64>().unwrap_or(0));
        println!(
            " {:<id$} | {:<st$} | {:<fp$} | {:<tw$} | {:<tw$} ",
            r.agent_id,
            r.status.as_str(),
            r.fingerprint,
            decided,
            last_seen,
            id = id_w,
            st = st_w,
            fp = fp_w,
            tw = time_w,
        );
    }
    println!("{} record(s)", list.len());
}

/// Prints records as a table, newest first. TIME is readable UTC.
fn print_table(records: &[JournalRecord]) {
    if records.is_empty() {
        println!("(journal is empty)");
        return;
    }
    let mut id_w = 2usize;
    let mut level_w = 5usize;
    let mut cat_w = 8usize;
    let mut msg_w = 7usize;
    for r in records {
        id_w = id_w.max(format!("{}", r.seq).len());
        level_w = level_w.max(r.level.len());
        cat_w = cat_w.max(r.category.len());
        let msg = &r.message;
        msg_w = msg_w.max(msg.len().min(TRUNCATE));
    }
    let time_w = "YYYY-MM-DD HH:MM:SS UTC".len();

    let sep = |w: usize| "-".repeat(w);
    println!(
        " {:>id$} | {:<tw$} | {:<lw$} | {:<cw$} | {:<mw$} ",
        "ID",
        "TIME (UTC)",
        "LEVEL",
        "CATEGORY",
        "MESSAGE",
        id = id_w,
        tw = time_w,
        lw = level_w,
        cw = cat_w,
        mw = msg_w,
    );
    println!(
        " {}-+-{}-+-{}-+-{}-+-{}- ",
        sep(id_w),
        sep(time_w),
        sep(level_w),
        sep(cat_w),
        sep(msg_w),
    );
    for r in records {
        let msg: String = if r.message.len() > TRUNCATE {
            format!("{}…", &r.message[..TRUNCATE])
        } else {
            r.message.clone()
        };
        println!(
            " {:>id$} | {:<tw$} | {:<lw$} | {:<cw$} | {:<mw$} ",
            r.seq,
            unix_to_utc(r.ts.parse::<u64>().unwrap_or(0)),
            r.level,
            r.category,
            msg,
            id = id_w,
            tw = time_w,
            lw = level_w,
            cw = cat_w,
            mw = msg_w,
        );
    }
    println!(
        " {}-+-{}-+-{}-+-{}-+-{}- ",
        sep(id_w),
        sep(time_w),
        sep(level_w),
        sep(cat_w),
        sep(msg_w),
    );
    println!("{} rows", records.len());
}

const TRUNCATE: usize = 120;

/// Parses a period like `10m`, `6h`, `1d`, `2w`, `1mo` into seconds.
fn parse_period(s: &str) -> anyhow::Result<u64> {
    let s = s.trim();
    let (num, unit) = s
        .find(|c: char| !c.is_ascii_digit())
        .map(|idx| (&s[..idx], &s[idx..]))
        .ok_or_else(|| anyhow::anyhow!("--since: expected number+unit, e.g. 10m/6h/1d"))?;
    let n: u64 = num
        .parse()
        .map_err(|_| anyhow::anyhow!("--since: not a number: {num}"))?;
    if n == 0 {
        anyhow::bail!("--since: period must be greater than zero");
    }
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 24 * 3600,
        "w" => 7 * 24 * 3600,
        "mo" => 30 * 24 * 3600,
        _ => anyhow::bail!("--since: unknown unit {unit} (s/m/h/d/w/mo)"),
    };
    Ok(n * mult)
}

/// Converts unix seconds to readable UTC time without external crates
/// (civil-from-days, Howard Hinnant algorithm).
fn unix_to_utc(secs: u64) -> String {
    let days = secs / 86400;
    let rem = secs % 86400;
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;
    let ss = rem % 60;

    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y0 = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y0 + 1 } else { y0 };

    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02} UTC")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_period_units() {
        assert_eq!(parse_period("30s").unwrap(), 30);
        assert_eq!(parse_period("10m").unwrap(), 600);
        assert_eq!(parse_period("6h").unwrap(), 21600);
        assert_eq!(parse_period("1d").unwrap(), 86400);
        assert_eq!(parse_period("2w").unwrap(), 1209600);
        assert_eq!(parse_period("1mo").unwrap(), 2592000);
    }

    #[test]
    fn parse_period_rejects_garbage() {
        assert!(parse_period("abc").is_err());
        assert!(parse_period("5x").is_err());
        assert!(parse_period("0h").is_err());
        assert!(parse_period("").is_err());
    }

    #[test]
    fn unix_to_utc_known_values() {
        assert_eq!(unix_to_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(unix_to_utc(1_700_000_000), "2023-11-14 22:13:20 UTC");
        assert_eq!(unix_to_utc(1_787_097_498), "2026-08-18 23:58:18 UTC");
    }

    #[test]
    fn run_journal_missing_db_is_error() {
        let err = run_journal(&["--db".to_string(), "/nonexistent/j.db".to_string()]);
        assert!(err.is_err());
    }

    #[test]
    fn run_journal_unknown_flag_is_error() {
        let err = run_journal(&["--nope".to_string()]);
        assert!(err.is_err());
    }
}