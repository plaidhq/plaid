//! In-app updates. A check runs shortly after launch and every 6 h (failures only logged), or from 设置 › 通用
//! (failures shown). On macOS a newer release is downloaded, verified against GitHub's digest and unpacked in
//! the background; the user applies it from the tray or settings, which swaps the bundle in place and
//! relaunches, never during a dictation. Where the app can't replace itself (Windows for now, a bare binary, a
//! translocated or read-only bundle) the release page is opened instead.
use crate::{os, trace};
use gpui_kit::*;
use plaid_core::update::{self, Release};
use std::path::{Path, PathBuf};
use std::time::Duration;

const FIRST_CHECK: Duration = Duration::from_secs(30);
const EVERY: Duration = Duration::from_secs(6 * 3600);

#[derive(Clone, Debug, Default)]
pub enum State {
    #[default]
    Idle,
    Checking,
    /// A manual check found nothing newer.
    Latest,
    /// Newer, but installed by hand from the release page.
    Available(Release),
    Downloading(Release),
    /// Unpacked and verified; applying swaps this bundle in.
    Ready(Release, PathBuf),
    /// A manual check or a download failed.
    Failed(String),
}

#[derive(Default)]
pub struct Updater {
    pub state: State,
    /// Set when "restart to update" was clicked during a dictation.
    pub busy_note: bool,
}

impl Global for Updater {}

impl Updater {
    /// The version an update is offered for, if any.
    pub fn offer(&self) -> Option<&str> {
        match &self.state {
            State::Available(r) | State::Ready(r, _) => Some(&r.version),
            _ => None,
        }
    }
}

/// Starts the periodic check.
pub fn start(cx: &mut App) {
    cx.set_global(Updater::default());
    cx.spawn(async move |cx| {
        cx.background_executor().timer(FIRST_CHECK).await;
        loop {
            cx.update(|cx| check(false, cx));
            cx.background_executor().timer(EVERY).await;
        }
    })
    .detach();
}

fn set(state: State, cx: &mut App) {
    let u = cx.default_global::<Updater>();
    u.state = state;
    u.busy_note = false;
    let offer = u.offer().map(str::to_owned);
    let ready = matches!(u.state, State::Ready(..));
    crate::tray::offer_update(offer.as_deref(), ready, cx);
    cx.refresh_windows();
}

/// Looks for a newer release; `manual` shows "已是最新" and failures. A found update is kept across checks.
pub fn check(manual: bool, cx: &mut App) {
    let prior = cx.default_global::<Updater>().state.clone();
    match &prior {
        State::Checking | State::Downloading(_) | State::Ready(..) => return,
        _ => {}
    }
    if manual {
        set(State::Checking, cx);
    }
    let task = cx
        .background_executor()
        .spawn(async move { update::check(os::proxy().as_ref()) });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok(r) if update::newer(&r.version, env!("CARGO_PKG_VERSION")) => {
                trace::log(&format!("update: {} available", r.version));
                fetch(r, cx);
            }
            Ok(_) if manual => set(State::Latest, cx),
            Ok(_) => {}
            Err(e) => {
                trace::log(&format!("update check: {e}"));
                if manual {
                    set(State::Failed(format!("检查失败：{e}")), cx);
                } else if matches!(prior, State::Failed(_) | State::Latest) {
                    set(State::Idle, cx);
                }
            }
        });
    })
    .detach();
}

/// Downloads and unpacks `r` when this app can replace itself, else offers the release page.
fn fetch(r: Release, cx: &mut App) {
    let (Some(app), Some(asset)) = (bundle(), r.asset.clone()) else {
        return set(State::Available(r), cx);
    };
    set(State::Downloading(r.clone()), cx);
    let version = r.version.clone();
    let task = cx.background_executor().spawn(async move {
        let dir = std::env::temp_dir().join("plaid-update");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let zip = dir.join("update.zip");
        update::download(&asset, &zip, os::proxy().as_ref())?;
        let staged = stage(&zip, &dir.join("unpacked"), &app, &version);
        let _ = std::fs::remove_file(&zip);
        staged
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok(staged) => {
                trace::log(&format!("update: {} ready", r.version));
                set(State::Ready(r, staged), cx);
            }
            Err(e) => {
                trace::log(&format!("update download: {e}"));
                set(State::Failed(format!("下载 {} 失败：{e}", r.version)), cx);
            }
        });
    })
    .detach();
}

/// The tray / settings action: restart into the staged update, or open the release page.
pub fn apply(cx: &mut App) {
    let state = cx.default_global::<Updater>().state.clone();
    match state {
        State::Available(r) => crate::ui::open_url(&r.page),
        State::Ready(r, staged) => {
            // The controller's flag for "esc would cancel something": recording, finishing, refining, or a
            // command confirmation on screen.
            if cx
                .try_global::<crate::ui::Live>()
                .is_some_and(|l| l.keys.busy())
            {
                cx.default_global::<Updater>().busy_note = true;
                cx.refresh_windows();
                return;
            }
            let Some(app) = bundle() else {
                return crate::ui::open_url(&r.page);
            };
            match swap(&app, &staged) {
                Ok(old) => {
                    trace::log(&format!("update: installing {}", r.version));
                    if let Err(e) = relaunch(&app, &old) {
                        trace::log(&format!("update relaunch: {e}"));
                    }
                    cx.quit();
                }
                Err(e) => {
                    trace::log(&format!("update install: {e}"));
                    set(State::Failed(format!("安装失败：{e}")), cx);
                }
            }
        }
        _ => {}
    }
}

/// `plaid --update`: the same check, download and swap without the UI or a relaunch.
pub fn cli() -> Result<(), String> {
    let r = update::check(os::proxy().as_ref())?;
    let current = env!("CARGO_PKG_VERSION");
    if !update::newer(&r.version, current) {
        println!("up to date: {current} (latest {})", r.version);
        return Ok(());
    }
    let app = bundle().ok_or("not a replaceable app bundle")?;
    let asset = r
        .asset
        .as_ref()
        .ok_or("release has no asset with a digest for this platform")?;
    let dir = std::env::temp_dir().join("plaid-update-cli");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let zip = dir.join("update.zip");
    update::download(asset, &zip, os::proxy().as_ref())?;
    let staged = stage(&zip, &dir.join("unpacked"), &app, &r.version)?;
    let old = swap(&app, &staged)?;
    let _ = std::fs::remove_dir_all(&old);
    let _ = std::fs::remove_dir_all(&dir);
    println!("updated {} {current} -> {}", app.display(), r.version);
    Ok(())
}

/// The running app bundle, when it can be replaced in place: not translocated, in a writable folder.
#[cfg(target_os = "macos")]
pub fn bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let app = exe.parent()?.parent()?.parent()?;
    if app.extension()? != "app" || app.to_string_lossy().contains("/AppTranslocation/") {
        return None;
    }
    let probe = app
        .parent()?
        .join(format!(".plaid-write-{}", std::process::id()));
    std::fs::write(&probe, b"").ok()?;
    let _ = std::fs::remove_file(&probe);
    Some(app.to_owned())
}

#[cfg(not(target_os = "macos"))]
pub fn bundle() -> Option<PathBuf> {
    None
}

fn plist(app: &Path, key: &str) -> Option<String> {
    let out = std::process::Command::new("/usr/libexec/PlistBuddy")
        .arg("-c")
        .arg(format!("Print :{key}"))
        .arg(app.join("Contents/Info.plist"))
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Unzips into `dir` and returns the one .app inside, which must carry `app`'s bundle id and `version`.
pub fn stage(zip: &Path, dir: &Path, app: &Path, version: &str) -> Result<PathBuf, String> {
    let _ = std::fs::remove_dir_all(dir);
    let ok = std::process::Command::new("ditto")
        .args(["-x", "-k"])
        .arg(zip)
        .arg(dir)
        .status()
        .map_err(|e| e.to_string())?
        .success();
    if !ok {
        return Err("解压失败".into());
    }
    let mut apps = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "app"));
    let (Some(staged), None) = (apps.next(), apps.next()) else {
        return Err("压缩包里不是单个 .app".into());
    };
    let id = plist(&staged, "CFBundleIdentifier");
    if id.is_none() || id != plist(app, "CFBundleIdentifier") {
        return Err(format!("bundle id 不符：{id:?}"));
    }
    let v = plist(&staged, "CFBundleShortVersionString");
    if v.as_deref() != Some(version) {
        return Err(format!("版本不符：{v:?}"));
    }
    Ok(staged)
}

/// Moves `app` aside and copies `staged` in its place; on failure puts `app` back. Returns the moved-aside path.
pub fn swap(app: &Path, staged: &Path) -> Result<PathBuf, String> {
    let name = app.file_name().ok_or("no name")?.to_string_lossy();
    let old = app.with_file_name(format!(".{name}.old"));
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(app, &old).map_err(|e| e.to_string())?;
    let copied = std::process::Command::new("ditto")
        .arg(staged)
        .arg(app)
        .status()
        .map(|s| s.success());
    if copied.as_ref().is_ok_and(|&ok| ok) {
        let _ = std::fs::remove_dir_all(staged);
        return Ok(old);
    }
    let _ = std::fs::remove_dir_all(app);
    std::fs::rename(&old, app).map_err(|e| format!("恢复旧版本失败：{e}"))?;
    Err(format!("复制失败：{copied:?}"))
}

/// Once this process has exited: delete the old bundle, open the new one.
fn relaunch(app: &Path, old: &Path) -> std::io::Result<()> {
    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; rm -rf "$2"; open "$3""#)
        .arg("sh")
        .arg(std::process::id().to_string())
        .arg(old)
        .arg(app)
        .spawn()
        .map(drop)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    // Not `super::*`: gpui_kit's glob brings its own `test` attribute.
    use super::{stage, swap};
    use std::path::{Path, PathBuf};

    fn fake_app(dir: &Path, name: &str, id: &str, version: &str) -> PathBuf {
        let app = dir.join(name);
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/MacOS/plaid"), version).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{id}</string>
<key>CFBundleShortVersionString</key><string>{version}</string>
</dict></plist>"#
            ),
        )
        .unwrap();
        app
    }

    fn zip(app: &Path, to: &Path) {
        assert!(
            std::process::Command::new("ditto")
                .args(["-c", "-k", "--keepParent"])
                .arg(app)
                .arg(to)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn stage_and_swap() {
        let root = std::env::temp_dir().join(format!("plaid-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (apps, build) = (root.join("Applications"), root.join("build"));
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::create_dir_all(&build).unwrap();
        let installed = fake_app(&apps, "Plaid.app", "x.plaid", "1.0.0");
        let next = fake_app(&build, "Plaid.app", "x.plaid", "1.0.1");
        let z = root.join("u.zip");
        zip(&next, &z);

        assert!(
            stage(&z, &root.join("s"), &installed, "1.0.2")
                .unwrap_err()
                .contains("版本")
        );
        let other = fake_app(&root.join("o"), "Other.app", "x.other", "1.0.1");
        let zo = root.join("o.zip");
        zip(&other, &zo);
        assert!(
            stage(&zo, &root.join("s"), &installed, "1.0.1")
                .unwrap_err()
                .contains("bundle id")
        );

        let staged = stage(&z, &root.join("s"), &installed, "1.0.1").unwrap();
        let old = swap(&installed, &staged).unwrap();
        assert_eq!(
            std::fs::read_to_string(installed.join("Contents/MacOS/plaid")).unwrap(),
            "1.0.1"
        );
        assert_eq!(
            std::fs::read_to_string(old.join("Contents/MacOS/plaid")).unwrap(),
            "1.0.0"
        );
        assert_eq!(old.file_name().unwrap(), ".Plaid.app.old");

        // A failed copy puts the old bundle back.
        let err = swap(&installed, &root.join("missing.app")).unwrap_err();
        assert!(err.contains("复制失败"), "{err}");
        assert_eq!(
            std::fs::read_to_string(installed.join("Contents/MacOS/plaid")).unwrap(),
            "1.0.1"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
