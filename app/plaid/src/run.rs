//! Voice command runner (Actions.run/spawn in Actions.swift): one shell line, args on stdin, stdout/stderr
//! captured, killed after `actions::TIMEOUT`. Blocking; call off the controller thread.
use crate::trace;
use plaid_core::actions::{Action, Outcome, TIMEOUT};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex, mpsc};
use std::time::{Duration, Instant};

/// Per stream; the rest is read and dropped so the child never blocks on a full pipe.
const CAP: usize = 256 << 10;
/// After exit, pipes a background child keeps open are abandoned after this.
const PIPE_GRACE: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(10);

/// Names of actions in flight: a second run of one still running is refused.
static RUNNING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

pub fn run(a: &Action, args: &str, env: &[(&'static str, String)]) -> Outcome {
    if !RUNNING.lock().unwrap().insert(a.name.clone()) {
        return Outcome::failed("上一次还在运行");
    }
    let mut cmd = shell(&a.run);
    cmd.envs(env.iter().map(|(k, v)| (k, v)));
    #[cfg(target_os = "macos")]
    cmd.env("PATH", &*LOGIN_PATH);
    let r = spawn(cmd, args.as_bytes(), TIMEOUT);
    RUNNING.lock().unwrap().remove(&a.name);
    let err = if r.ok() {
        String::new()
    } else {
        format!(" err={}", r.failure().chars().take(80).collect::<String>())
    };
    trace::log(&format!(
        "action {} args={} code={}{} ms={} out={}B{err}",
        a.name,
        args.chars().count(),
        r.code.map_or("-".into(), |c| c.to_string()),
        if r.timed_out { " timeout" } else { "" },
        r.ms,
        r.out.len()
    ));
    r
}

#[cfg(unix)]
fn shell(line: &str) -> Command {
    let mut c = Command::new("/bin/zsh");
    c.arg("-c").arg(line);
    c
}

#[cfg(windows)]
fn shell(line: &str) -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut c = Command::new("powershell");
    c.args(["-NoProfile", "-NonInteractive", "-Command", line])
        .creation_flags(CREATE_NO_WINDOW);
    c
}

/// PATH of the user's login shell: a GUI app gets only /usr/bin:/bin:/usr/sbin:/sbin, so Homebrew's tools would be
/// missing. Resolved once (interactive, so .zshrc counts); falls back to adding the Homebrew dirs.
#[cfg(target_os = "macos")]
static LOGIN_PATH: LazyLock<String> = LazyLock::new(|| {
    const MARKER: &str = "__PLAID_PATH__";
    let base = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into());
    let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut c = Command::new(&sh);
    c.arg("-lic")
        .arg(format!("printf '\\n{MARKER}%s\\n' \"$PATH\""));
    let r = spawn(c, b"", Duration::from_secs(5));
    if let Some(p) = r
        .out
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix(MARKER))
        .filter(|p| !p.is_empty())
    {
        trace::log(&format!("login PATH from {sh}"));
        return p.to_owned();
    }
    trace::log(&format!(
        "login PATH unavailable ({}), using defaults",
        r.failure()
    ));
    base + ":/opt/homebrew/bin:/usr/local/bin"
});

/// Resolves the login PATH in the background so the first command doesn't wait ~0.5 s for the shell.
pub fn prewarm() {
    #[cfg(target_os = "macos")]
    std::thread::spawn(|| LazyLock::force(&LOGIN_PATH));
}

/// Reads into `out` as data arrives (what a background child's open pipe held back is still kept), then signals EOF.
fn drain(mut r: impl Read + Send + 'static, out: Arc<Mutex<Vec<u8>>>, eof: mpsc::Sender<()>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 16 << 10];
        while let Ok(n) = r.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut o = out.lock().unwrap();
            let keep = n.min(CAP.saturating_sub(o.len()));
            o.extend_from_slice(&buf[..keep]);
        }
        let _ = eof.send(());
    });
}

/// Unix: own process group, so a timeout takes down the whole pipeline (TERM, then KILL a second later).
fn kill(child: &mut Child) {
    #[cfg(unix)]
    {
        let pg = -(child.id() as i32);
        unsafe { libc::kill(pg, libc::SIGTERM) };
        let until = Instant::now() + Duration::from_secs(1);
        while Instant::now() < until {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(POLL);
        }
        unsafe { libc::kill(pg, libc::SIGKILL) };
    }
    #[cfg(windows)]
    let _ = child.kill();
}

fn spawn(mut cmd: Command, stdin: &[u8], timeout: Duration) -> Outcome {
    let t0 = Instant::now();
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Outcome::failed(e.to_string()),
    };
    // A command that never reads stdin must not block us; Rust ignores SIGPIPE, so the write just fails.
    if let Some(mut w) = child.stdin.take() {
        let input = stdin.to_vec();
        std::thread::spawn(move || {
            let _ = w.write_all(&input);
        });
    }
    let (tx, rx) = mpsc::channel();
    let streams: [Arc<Mutex<Vec<u8>>>; 2] = Default::default();
    let mut open = 0;
    if let Some(o) = child.stdout.take() {
        drain(o, streams[0].clone(), tx.clone());
        open += 1;
    }
    if let Some(e) = child.stderr.take() {
        drain(e, streams[1].clone(), tx);
        open += 1;
    }
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if t0.elapsed() >= timeout => {
                timed_out = true;
                kill(&mut child);
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(_) => break None,
        }
    };
    let until = Instant::now() + PIPE_GRACE;
    for _ in 0..open {
        if rx
            .recv_timeout(until.saturating_duration_since(Instant::now()))
            .is_err()
        {
            break;
        }
    }
    let [out, err] = streams.map(|b| String::from_utf8_lossy(&b.lock().unwrap()).into_owned());
    Outcome {
        code: status.and_then(|s| s.code()),
        timed_out,
        out,
        err,
        ms: t0.elapsed().as_millis() as u64,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(line: &str, stdin: &str, timeout: Duration) -> Outcome {
        spawn(shell(line), stdin.as_bytes(), timeout)
    }

    #[test]
    fn captures_exit_and_stdin() {
        let r = sh(
            "read x; echo \"got $x\"; echo oops >&2; exit 3",
            "hi",
            TIMEOUT,
        );
        assert_eq!(
            (r.code, r.out.as_str(), r.err.as_str()),
            (Some(3), "got hi\n", "oops\n")
        );
        assert_eq!(r.failure(), "oops");
        let r = sh("true", &"x".repeat(1 << 20), TIMEOUT);
        assert!(r.ok(), "unread stdin: {r:?}");
        let r = sh("head -c 400000 /dev/zero", "", TIMEOUT);
        assert_eq!(r.out.len(), CAP);
    }

    #[test]
    fn timeout_kills_the_group_and_background_pipes_dont_hang() {
        let r = sh("sleep 30 | cat", "", Duration::from_millis(200));
        assert!(r.timed_out && r.ms < 2500, "{r:?}");
        assert_eq!(r.failure(), "超时（30 秒）");
        let t = Instant::now();
        let r = sh(
            "(sleep 5 >/dev/null 2>&1 &); sleep 5 & echo done",
            "",
            TIMEOUT,
        );
        assert!(
            r.ok() && r.out == "done\n" && t.elapsed() < Duration::from_secs(3),
            "{r:?}"
        );
    }
}
