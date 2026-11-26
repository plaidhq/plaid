//! Accounts and usage in SQLite. Tokens and invite codes are stored as SHA-256; no text or audio is ever stored.
use chrono::{DateTime, Datelike, SecondsFormat, TimeZone, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::sync::Mutex;

const INVITE_DAYS: i64 = 7;

const MIGRATIONS: &[&str] = &["
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    disabled INTEGER NOT NULL DEFAULT 0,
    created TEXT NOT NULL
);
CREATE TABLE devices (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id),
    token_hash TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    created TEXT NOT NULL,
    last_seen TEXT,
    revoked INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE invites (
    code_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id),
    expires TEXT NOT NULL,
    used_at TEXT
);
CREATE TABLE usage (
    id INTEGER PRIMARY KEY,
    at TEXT NOT NULL,
    user_id INTEGER NOT NULL REFERENCES users(id),
    device_id INTEGER NOT NULL REFERENCES devices(id),
    kind TEXT NOT NULL,
    client TEXT NOT NULL,
    received_ms INTEGER NOT NULL,
    sent_ms INTEGER NOT NULL,
    asr_wait_ms INTEGER,
    llm_model TEXT,
    llm_in INTEGER NOT NULL,
    llm_out INTEGER NOT NULL,
    llm_ms INTEGER,
    refine TEXT NOT NULL,
    cost_micros INTEGER NOT NULL,
    error TEXT
);
CREATE INDEX usage_user_at ON usage(user_id, at);
CREATE TABLE credits (
    id INTEGER PRIMARY KEY,
    at TEXT NOT NULL,
    user_id INTEGER NOT NULL REFERENCES users(id),
    amount_micros INTEGER NOT NULL
);
CREATE INDEX credits_user ON credits(user_id);
"];

pub struct Db(Mutex<Connection>);

/// The caller of an authorized request.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: i64,
    pub user_id: i64,
    pub user: String,
}

/// Why an invite code was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    Unknown,
    Used,
    Expired,
    Disabled,
}

impl Refused {
    /// Shown on the device as is.
    pub fn message(self) -> &'static str {
        match self {
            Refused::Unknown => "邀请码不对",
            Refused::Used => "这个邀请码已经用过了",
            Refused::Expired => "邀请码已过期，请找管理员要新的",
            Refused::Disabled => "这个账号已停用",
        }
    }
}

/// One request, as billed. Never any text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub user_id: i64,
    pub device_id: i64,
    pub kind: String,
    pub client: String,
    pub received_ms: i64,
    pub sent_ms: i64,
    pub asr_wait_ms: Option<i64>,
    pub llm_model: Option<String>,
    pub llm_in: i64,
    pub llm_out: i64,
    pub llm_ms: Option<i64>,
    /// none, skipped, ok, timeout, error.
    pub refine: String,
    pub cost_micros: i64,
    /// auth, quota, upstream, bad_request, disconnected.
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UserRow {
    pub name: String,
    pub balance_micros: i64,
    pub disabled: bool,
    pub devices: i64,
    pub month_micros: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceRow {
    pub id: i64,
    pub user: String,
    pub name: String,
    pub last_seen: Option<String>,
    pub revoked: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MonthRow {
    pub user: String,
    pub requests: i64,
    pub errors: i64,
    pub sent_ms: i64,
    pub llm_in: i64,
    pub llm_out: i64,
    pub cost_micros: i64,
}

/// A user's balance from `?1` = user id.
const BALANCE: &str = "(SELECT COALESCE(SUM(amount_micros), 0) FROM credits WHERE user_id = ?1)
    - (SELECT COALESCE(SUM(cost_micros), 0) FROM usage WHERE user_id = ?1)";

fn add_credit(c: &Connection, user: i64, amount: i64, now: DateTime<Utc>) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO credits (at, user_id, amount_micros) VALUES (?1, ?2, ?3)",
        params![ts(now), user, amount],
    )?;
    Ok(())
}

pub fn hash(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// [start, end) of the UTC month holding `t`, as stored timestamps.
fn month_range(year: i32, month: u32) -> (String, String) {
    let start = Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0).unwrap();
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let end = Utc.with_ymd_and_hms(ny, nm, 1, 0, 0, 0).unwrap();
    (ts(start), ts(end))
}

/// A secret from fresh randomness (UUID v4 is 122 random bits).
fn random_hex(uuids: usize) -> String {
    (0..uuids)
        .map(|_| uuid::Uuid::new_v4().simple().to_string())
        .collect()
}

/// An invite code that survives being read out or typed: 10 characters from an alphabet without 0/O/1/I/L.
fn invite_code() -> String {
    const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
    uuid::Uuid::new_v4().as_bytes()[..10]
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect()
}

/// Codes are compared without case, spaces or dashes.
fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

impl Db {
    pub fn open(path: &str) -> rusqlite::Result<Db> {
        let c = Connection::open(path)?;
        c.pragma_update(None, "journal_mode", "WAL")?;
        c.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(c)
    }

    pub fn memory() -> Db {
        Self::init(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn init(c: Connection) -> rusqlite::Result<Db> {
        c.pragma_update(None, "foreign_keys", "ON")?;
        let version: usize = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, m) in MIGRATIONS.iter().enumerate().skip(version) {
            c.execute_batch(m)?;
            c.pragma_update(None, "user_version", i + 1)?;
        }
        Ok(Db(Mutex::new(c)))
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Creates the user if needed, adds `credit` to their balance when given and returns a new one-time code.
    pub fn invite(
        &self,
        name: &str,
        credit: Option<i64>,
        now: DateTime<Utc>,
    ) -> rusqlite::Result<String> {
        let c = self.conn();
        c.execute(
            "INSERT INTO users (name, created) VALUES (?1, ?2) ON CONFLICT(name) DO NOTHING",
            params![name, ts(now)],
        )?;
        let user: i64 =
            c.query_row("SELECT id FROM users WHERE name = ?1", [name], |r| r.get(0))?;
        if let Some(a) = credit {
            add_credit(&c, user, a, now)?;
        }
        let code = invite_code();
        let expires = now + chrono::Duration::days(INVITE_DAYS);
        c.execute(
            "INSERT INTO invites (code_hash, user_id, expires) VALUES (?1, ?2, ?3)",
            params![hash(&code), user, ts(expires)],
        )?;
        Ok(code)
    }

    /// Spends an unused, unexpired code on a new device token: (token, user name), or why not.
    pub fn login(
        &self,
        code: &str,
        device: &str,
        now: DateTime<Utc>,
    ) -> rusqlite::Result<Result<(String, String), Refused>> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let found: Option<(i64, String, bool, String, bool)> = tx
            .query_row(
                "SELECT i.user_id, u.name, i.used_at IS NOT NULL, i.expires, u.disabled
                 FROM invites i JOIN users u ON u.id = i.user_id WHERE i.code_hash = ?1",
                [hash(&normalize_code(code))],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let (user, name) = match found {
            None => return Ok(Err(Refused::Unknown)),
            Some((.., true, _, _)) => return Ok(Err(Refused::Used)),
            Some((.., expires, _)) if expires <= ts(now) => return Ok(Err(Refused::Expired)),
            Some((.., true)) => return Ok(Err(Refused::Disabled)),
            Some((user, name, ..)) => (user, name),
        };
        tx.execute(
            "UPDATE invites SET used_at = ?1 WHERE code_hash = ?2",
            params![ts(now), hash(&normalize_code(code))],
        )?;
        let token = format!("plaid_{}", random_hex(2));
        let device: String = device.chars().take(80).collect();
        tx.execute(
            "INSERT INTO devices (user_id, token_hash, name, created) VALUES (?1, ?2, ?3, ?4)",
            params![user, hash(&token), device, ts(now)],
        )?;
        tx.commit()?;
        Ok(Ok((token, name)))
    }

    /// The device behind a token, unless revoked or its user disabled; marks it seen.
    pub fn auth(&self, token: &str, now: DateTime<Utc>) -> rusqlite::Result<Option<Device>> {
        let c = self.conn();
        let d = c
            .query_row(
                "SELECT d.id, u.id, u.name FROM devices d JOIN users u ON u.id = d.user_id
                 WHERE d.token_hash = ?1 AND d.revoked = 0 AND u.disabled = 0",
                [hash(token)],
                |r| {
                    Ok(Device {
                        id: r.get(0)?,
                        user_id: r.get(1)?,
                        user: r.get(2)?,
                    })
                },
            )
            .optional()?;
        if let Some(d) = &d {
            c.execute(
                "UPDATE devices SET last_seen = ?1 WHERE id = ?2",
                params![ts(now), d.id],
            )?;
        }
        Ok(d)
    }

    /// The user's spend and audio sent this UTC month.
    pub fn month(&self, user_id: i64, now: DateTime<Utc>) -> rusqlite::Result<(i64, i64)> {
        let (start, end) = month_range(now.year(), now.month());
        self.conn().query_row(
            "SELECT COALESCE(SUM(cost_micros), 0), COALESCE(SUM(sent_ms), 0) FROM usage
             WHERE user_id = ?1 AND at >= ?2 AND at < ?3",
            params![user_id, start, end],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    /// Credit added minus everything spent.
    pub fn balance(&self, user_id: i64) -> rusqlite::Result<i64> {
        self.conn()
            .query_row(&format!("SELECT {BALANCE}"), [user_id], |r| r.get(0))
    }

    /// Adds (or with a negative amount, takes) credit; the new balance, or None without such a user.
    pub fn credit(
        &self,
        name: &str,
        amount: i64,
        now: DateTime<Utc>,
    ) -> rusqlite::Result<Option<i64>> {
        let c = self.conn();
        let Some(user) = c
            .query_row("SELECT id FROM users WHERE name = ?1", [name], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
        else {
            return Ok(None);
        };
        add_credit(&c, user, amount, now)?;
        c.query_row(&format!("SELECT {BALANCE}"), [user], |r| r.get(0))
            .map(Some)
    }

    pub fn add_usage(&self, u: &Usage, now: DateTime<Utc>) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO usage (at, user_id, device_id, kind, client, received_ms, sent_ms, asr_wait_ms, llm_model,
                 llm_in, llm_out, llm_ms, refine, cost_micros, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                ts(now),
                u.user_id,
                u.device_id,
                u.kind,
                u.client,
                u.received_ms,
                u.sent_ms,
                u.asr_wait_ms,
                u.llm_model,
                u.llm_in,
                u.llm_out,
                u.llm_ms,
                u.refine,
                u.cost_micros,
                u.error
            ],
        )?;
        Ok(())
    }

    pub fn users(&self, now: DateTime<Utc>) -> rusqlite::Result<Vec<UserRow>> {
        let (start, end) = month_range(now.year(), now.month());
        let c = self.conn();
        let mut q = c.prepare(
            "SELECT u.name, (SELECT COALESCE(SUM(amount_micros), 0) FROM credits k WHERE k.user_id = u.id)
                     - (SELECT COALESCE(SUM(cost_micros), 0) FROM usage g WHERE g.user_id = u.id),
                 u.disabled,
                 (SELECT COUNT(*) FROM devices d WHERE d.user_id = u.id AND d.revoked = 0),
                 (SELECT COALESCE(SUM(cost_micros), 0) FROM usage g WHERE g.user_id = u.id AND g.at >= ?1 AND g.at < ?2)
             FROM users u ORDER BY u.name",
        )?;
        q.query_map(params![start, end], |r| {
            Ok(UserRow {
                name: r.get(0)?,
                balance_micros: r.get(1)?,
                disabled: r.get(2)?,
                devices: r.get(3)?,
                month_micros: r.get(4)?,
            })
        })?
        .collect()
    }

    pub fn devices(&self) -> rusqlite::Result<Vec<DeviceRow>> {
        let c = self.conn();
        let mut q = c.prepare(
            "SELECT d.id, u.name, d.name, d.last_seen, d.revoked FROM devices d JOIN users u ON u.id = d.user_id
             ORDER BY u.name, d.id",
        )?;
        q.query_map([], |r| {
            Ok(DeviceRow {
                id: r.get(0)?,
                user: r.get(1)?,
                name: r.get(2)?,
                last_seen: r.get(3)?,
                revoked: r.get(4)?,
            })
        })?
        .collect()
    }

    /// Whether the device existed.
    pub fn revoke(&self, device: i64) -> rusqlite::Result<bool> {
        Ok(self
            .conn()
            .execute("UPDATE devices SET revoked = 1 WHERE id = ?1", [device])?
            > 0)
    }

    pub fn set_disabled(&self, name: &str, disabled: bool) -> rusqlite::Result<bool> {
        Ok(self.conn().execute(
            "UPDATE users SET disabled = ?1 WHERE name = ?2",
            params![disabled, name],
        )? > 0)
    }

    /// Per-user totals for a UTC month.
    pub fn report(&self, year: i32, month: u32) -> rusqlite::Result<Vec<MonthRow>> {
        let (start, end) = month_range(year, month);
        let c = self.conn();
        let mut q = c.prepare(
            "SELECT u.name, COUNT(*), SUM(g.error IS NOT NULL), SUM(g.sent_ms), SUM(g.llm_in), SUM(g.llm_out),
                 SUM(g.cost_micros)
             FROM usage g JOIN users u ON u.id = g.user_id
             WHERE g.at >= ?1 AND g.at < ?2 GROUP BY u.name ORDER BY u.name",
        )?;
        q.query_map(params![start, end], |r| {
            Ok(MonthRow {
                user: r.get(0)?,
                requests: r.get(1)?,
                errors: r.get(2)?,
                sent_ms: r.get(3)?,
                llm_in: r.get(4)?,
                llm_out: r.get(5)?,
                cost_micros: r.get(6)?,
            })
        })?
        .collect()
    }

    /// Every text value in the database, for the privacy test.
    #[cfg(test)]
    pub fn all_text(&self) -> Vec<String> {
        let c = self.conn();
        let mut out = vec![];
        for table in ["users", "devices", "invites", "usage", "credits"] {
            let mut q = c.prepare(&format!("SELECT * FROM {table}")).unwrap();
            let n = q.column_count();
            let rows = q
                .query_map([], |r| {
                    Ok((0..n)
                        .filter_map(|i| r.get::<_, Option<String>>(i).ok().flatten())
                        .collect::<Vec<_>>())
                })
                .unwrap();
            for r in rows {
                out.extend(r.unwrap());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn invite_login_auth_revoke() {
        let db = Db::memory();
        let now = at("2026-11-25T10:00:00Z");
        let code = db.invite("amy", Some(5_000_000), now).unwrap();
        assert_eq!(code.len(), 10);
        // Case, spaces and dashes don't matter.
        let typed = format!("{}-{} ", code[..5].to_lowercase(), &code[5..]);
        let (token, name) = db.login(&typed, "Amy's Mac", now).unwrap().unwrap();
        assert_eq!(name, "amy");
        assert_eq!(db.login(&code, "again", now).unwrap(), Err(Refused::Used));
        assert_eq!(
            db.login("ABCDE23456", "d", now).unwrap(),
            Err(Refused::Unknown)
        );
        let d = db.auth(&token, now).unwrap().unwrap();
        assert_eq!(
            (d.user.as_str(), db.balance(d.user_id).unwrap()),
            ("amy", 5_000_000)
        );
        assert!(db.auth("plaid_wrong", now).unwrap().is_none());
        assert!(db.revoke(d.id).unwrap());
        assert!(db.auth(&token, now).unwrap().is_none());
        // The token and code are stored only hashed.
        assert!(
            db.all_text()
                .iter()
                .all(|t| !t.contains(&token) && !t.contains(&code))
        );
    }

    #[test]
    fn invites_expire_after_seven_days() {
        let db = Db::memory();
        let code = db.invite("bo", None, at("2026-11-01T00:00:00Z")).unwrap();
        assert_eq!(
            db.login(&code, "d", at("2026-11-08T00:00:01Z")).unwrap(),
            Err(Refused::Expired)
        );
        let code = db.invite("bo", None, at("2026-11-01T00:00:00Z")).unwrap();
        assert!(
            db.login(&code, "d", at("2026-11-07T23:00:00Z"))
                .unwrap()
                .is_ok()
        );
    }

    #[test]
    fn a_disabled_user_cannot_log_in_or_authenticate() {
        let db = Db::memory();
        let now = at("2026-11-25T10:00:00Z");
        let (token, _) = db
            .login(&db.invite("cy", None, now).unwrap(), "d", now)
            .unwrap()
            .unwrap();
        let code = db.invite("cy", None, now).unwrap();
        db.set_disabled("cy", true).unwrap();
        assert!(db.auth(&token, now).unwrap().is_none());
        assert_eq!(db.login(&code, "d", now).unwrap(), Err(Refused::Disabled));
    }

    #[test]
    fn month_sums_reset_on_the_first_utc() {
        let db = Db::memory();
        let now = at("2026-11-25T10:00:00Z");
        let (token, _) = db
            .login(&db.invite("di", None, now).unwrap(), "d", now)
            .unwrap()
            .unwrap();
        let d = db.auth(&token, now).unwrap().unwrap();
        let u = Usage {
            user_id: d.user_id,
            device_id: d.id,
            kind: "dictate".into(),
            client: "t".into(),
            sent_ms: 1000,
            refine: "ok".into(),
            cost_micros: 300,
            ..Usage::default()
        };
        db.add_usage(&u, at("2026-10-31T23:59:59Z")).unwrap();
        db.add_usage(&u, at("2026-11-01T00:00:00Z")).unwrap();
        db.add_usage(&u, now).unwrap();
        assert_eq!(db.month(d.user_id, now).unwrap(), (600, 2000));
        let r = db.report(2026, 10).unwrap();
        assert_eq!((r[0].requests, r[0].cost_micros), (1, 300));
        assert_eq!(db.users(now).unwrap()[0].month_micros, 600);
        // The balance spans all months.
        assert_eq!(db.balance(d.user_id).unwrap(), -900);
        assert_eq!(db.credit("di", 1000, now).unwrap(), Some(100));
        assert_eq!(db.credit("di", -50, now).unwrap(), Some(50));
        assert_eq!(db.users(now).unwrap()[0].balance_micros, 50);
        assert_eq!(db.credit("nobody", 1, now).unwrap(), None);
    }

    #[test]
    fn migrations_run_once() {
        let path = std::env::temp_dir().join(format!("plaid-cloud-{}.db", uuid::Uuid::new_v4()));
        let p = path.to_str().unwrap();
        Db::open(p).unwrap().invite("x", None, Utc::now()).unwrap();
        assert_eq!(Db::open(p).unwrap().users(Utc::now()).unwrap().len(), 1);
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{p}{ext}"));
        }
    }
}
