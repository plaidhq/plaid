//! Headless modes: no window, no tray.
use crate::driver::Driver;
use crate::{config, secret};
use chrono::{DateTime, Utc};
use plaid_core::dictation::{Controller, Hud, Msg};
use plaid_core::gesture;
use plaid_core::store::Store;
use std::io::Read;
use std::time::Instant;

/// Reads the key from stdin and stores it as this app (no access prompts later). The key is never printed.
pub fn store_key() -> Result<(), String> {
    let mut k = String::new();
    std::io::stdin()
        .read_to_string(&mut k)
        .map_err(|e| e.to_string())?;
    let k = k.trim();
    if k.is_empty() {
        return Err("empty key on stdin".into());
    }
    secret::set("apiKey", k)?;
    eprintln!("stored ({} chars)", k.chars().count());
    Ok(())
}

/// Runs one utterance through the controller with a 16 kHz mono WAV in place of the mic (real time, 100 ms
/// buffers, release at the end of the file). Records land in the data dir like a live run; the paste is printed.
pub fn replay(path: &str) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let pcm = plaid_core::wav::decode(&bytes)?.to_vec();
    eprintln!(
        "replay {path}: {:.1} s → {}",
        pcm.len() as f64 / 32000.,
        config::data_dir().display()
    );
    let (tx, rx) = async_channel::unbounded();
    let mut d = Driver::replay(tx.clone(), pcm);
    let mut c = Controller::new(Store::load(&config::data_dir()));
    let t0 = Instant::now();
    let _ = tx.try_send(Msg::Key {
        action: gesture::Action::StartHold,
        trigger_ms: None,
    });
    let mut end = false;
    loop {
        let m = rx.recv_blocking().map_err(|e| e.to_string())?;
        c.handle(m, &mut d);
        for h in d.hud.drain(..) {
            match h {
                Hud::Level(_) | Hud::Unquiet => {}
                Hud::Text { text, command } => println!(
                    "{:6.2}s {}{text}",
                    t0.elapsed().as_secs_f64(),
                    if command { "[command] " } else { "" }
                ),
                h @ (Hud::Done(_)
                | Hud::Saved(_)
                | Hud::Flash(_)
                | Hud::Failed(_)
                | Hud::Note(_)
                | Hud::Ask(_)
                | Hud::Hide) => {
                    eprintln!("hud {h:?}");
                    end = true;
                }
                h => eprintln!("hud {h:?}"),
            }
        }
        // Stay for the background learning pass (with a learning endpoint configured).
        if end && !c.learning() {
            break;
        }
    }
    if let Some(r) = c.store.all.last() {
        println!(
            "record: outcome={} llm={} asrMs={} totalMs={} audioMs={} ctx={:?} text={} checked={:?}",
            r.outcome.as_deref().unwrap_or("pasted"),
            r.llm,
            r.asr_ms,
            r.total_ms,
            r.audio_ms,
            r.ctx.as_deref().unwrap_or_default(),
            r.text,
            r.checked
        );
    }
    Ok(())
}

/// Recognizes a record's saved audio again (History's retry) and saves the text in place. `at` is the record's
/// RFC 3339 time; default: the newest record with audio.
pub fn recover(at: Option<&str>) -> Result<(), String> {
    let store = Store::load(&config::data_dir());
    let id = match at {
        Some(a) => DateTime::parse_from_rfc3339(a)
            .map_err(|e| format!("{a}: {e}"))?
            .with_timezone(&Utc),
        None => store
            .all
            .iter()
            .rev()
            .find(|r| r.audio.is_some())
            .map(|r| r.at)
            .ok_or("no record with audio")?,
    };
    let before = store
        .all
        .iter()
        .rev()
        .find(|r| r.at == id)
        .ok_or(format!("no record at {id}"))?;
    eprintln!(
        "recover {id}: outcome={} text={}",
        before.outcome.as_deref().unwrap_or("pasted"),
        before.text
    );
    let (tx, rx) = async_channel::unbounded();
    let mut d = Driver::replay(tx.clone(), vec![]);
    let mut c = Controller::new(store);
    let t0 = Instant::now();
    let _ = tx.try_send(Msg::Recover { id });
    loop {
        let m = rx.recv_blocking().map_err(|e| e.to_string())?;
        c.handle(m, &mut d);
        for h in d.hud.drain(..) {
            eprintln!("{:6.2}s hud {h:?}", t0.elapsed().as_secs_f64());
        }
        if c.recovering().is_none() {
            break;
        }
    }
    if let Some(r) = c.store.all.iter().rev().find(|r| r.at == id) {
        println!(
            "record: outcome={} error={:?} text={}",
            r.outcome.as_deref().unwrap_or("pasted"),
            r.error,
            r.text
        );
    }
    Ok(())
}
