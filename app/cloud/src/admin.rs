//! `plaid-cloud admin …`: accounts and usage from the shell. There is no web admin.
use crate::config::micros;
use crate::db::Db;
use chrono::{Datelike, Utc};
use plaid_core::cloud::{check_name, credits};

pub const USAGE: &str = "plaid-cloud admin <command>
  invite <name> [--credit N]    create the user if needed, add N credit and print a one-time code (7 days)
  rename <name> <new name>      change a user's name (at most 8 characters, unique ignoring case)
  credit <name> <N|-N>          add (or take) credit and print the balance
  users                         users, devices, balance and this month's spend
  devices                       devices and when they were last seen
  revoke <device id>            end a device's token
  disable <name> | enable <name>
  usage [YYYY-MM]               per-user totals for a UTC month (default: this month)";

/// A signed amount as millionths.
fn signed(s: &str) -> Result<i64, String> {
    match s.strip_prefix('-') {
        Some(n) => micros(n).map(|m| -m),
        None => micros(s.strip_prefix('+').unwrap_or(s)),
    }
}

fn found(ok: bool, what: &str) -> Result<String, String> {
    if ok {
        Ok("ok".into())
    } else {
        Err(format!("no such {what}"))
    }
}

/// The command's output, or an error for stderr.
pub fn run(db: &Db, args: &[String]) -> Result<String, String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let now = Utc::now();
    let db_err = |e: rusqlite::Error| e.to_string();
    match a.as_slice() {
        ["invite", name] => db.invite(&check_name(name)?, None, now).map_err(db_err),
        ["invite", name, "--credit", n] => db
            .invite(&check_name(name)?, Some(micros(n)?), now)
            .map_err(db_err),
        ["rename", name, new] => {
            let new = check_name(new)?;
            let id = db.user_id(name).map_err(db_err)?.ok_or("no such user")?;
            if db.rename(id, &new).map_err(db_err)? {
                Ok("ok".into())
            } else {
                Err(format!("name taken: {new}"))
            }
        }
        ["credit", name, n] => match db.credit(name, signed(n)?, now).map_err(db_err)? {
            Some(b) => Ok(format!("{name}\tbalance={}", credits(b))),
            None => Err("no such user".into()),
        },
        ["users"] => Ok(db
            .users(now)
            .map_err(db_err)?
            .iter()
            .map(|u| {
                format!(
                    "{}\tdevices={}\tbalance={}\tmonth={}{}",
                    u.name,
                    u.devices,
                    credits(u.balance_micros),
                    credits(u.month_micros),
                    if u.disabled { "\tdisabled" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")),
        ["devices"] => Ok(db
            .devices()
            .map_err(db_err)?
            .iter()
            .map(|d| {
                format!(
                    "{}\t{}\t{}\tlast_seen={}{}",
                    d.id,
                    d.user,
                    d.name,
                    d.last_seen.as_deref().unwrap_or("never"),
                    if d.revoked { "\trevoked" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")),
        ["revoke", id] => found(
            db.revoke(id.parse().map_err(|_| "device id is a number")?)
                .map_err(db_err)?,
            "device",
        ),
        ["disable", name] => found(db.set_disabled(name, true).map_err(db_err)?, "user"),
        ["enable", name] => found(db.set_disabled(name, false).map_err(db_err)?, "user"),
        ["usage"] => report(db, now.year(), now.month()),
        ["usage", ym] => {
            let (y, m) = ym
                .split_once('-')
                .and_then(|(y, m)| {
                    Some((
                        y.parse().ok()?,
                        m.parse().ok().filter(|m| (1..=12).contains(m))?,
                    ))
                })
                .ok_or("month is YYYY-MM")?;
            report(db, y, m)
        }
        _ => Err(USAGE.into()),
    }
}

fn report(db: &Db, year: i32, month: u32) -> Result<String, String> {
    let rows = db.report(year, month).map_err(|e| e.to_string())?;
    let mut out = vec![format!("{year}-{month:02}")];
    out.extend(rows.iter().map(|r| {
        format!(
            "{}\trequests={}\terrors={}\taudio={:.1}min\tllm_in={}\tllm_out={}\tcost={}",
            r.user,
            r.requests,
            r.errors,
            r.sent_ms as f64 / 60_000.,
            r.llm_in,
            r.llm_out,
            credits(r.cost_micros)
        )
    }));
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_s(db: &Db, s: &str) -> Result<String, String> {
        run(db, &s.split(' ').map(String::from).collect::<Vec<_>>())
    }

    #[test]
    fn commands() {
        let db = Db::memory();
        let code = run_s(&db, "invite amy --credit 5").unwrap();
        assert_eq!(code.len(), 10);
        assert_eq!(
            run_s(&db, "users").unwrap(),
            "amy\tdevices=0\tbalance=5.00 credit\tmonth=0.00 credit"
        );
        assert_eq!(
            run_s(&db, "credit amy +2.5"),
            Ok("amy\tbalance=7.50 credit".into())
        );
        assert_eq!(
            run_s(&db, "credit amy -10"),
            Ok("amy\tbalance=-2.50 credit".into())
        );
        assert!(run_s(&db, "credit bob 1").is_err());
        assert!(run_s(&db, "credit amy x").is_err());
        assert!(run_s(&db, "revoke 9").is_err());
        assert!(run_s(&db, "usage 2026-13").is_err());
        assert_eq!(run_s(&db, "usage 2026-11").unwrap(), "2026-11");
        assert!(
            run_s(&db, "bogus")
                .unwrap_err()
                .starts_with("plaid-cloud admin")
        );
    }
}
