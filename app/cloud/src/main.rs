//! `plaid-cloud [--config FILE] [serve]` runs the server; `plaid-cloud [--config FILE] admin …` manages
//! accounts. Settings come from the config file (default plaid-cloud.toml; docs/cloud.md).
use plaid_cloud::config::{self, Config, Settings};
use plaid_cloud::db::Db;
use plaid_cloud::server::{self, App};
use plaid_cloud::{admin, llm, log};
use std::sync::Arc;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let file = match args.first().map(String::as_str) {
        Some("--config" | "-c") if args.len() > 1 => {
            let f = args.remove(1);
            args.remove(0);
            f
        }
        _ => "plaid-cloud.toml".into(),
    };
    let settings = Settings::load(std::path::Path::new(&file)).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2)
    });
    warn_if_readable(&file);
    let path = settings.db();
    let db = Db::open(&path.to_string_lossy()).unwrap_or_else(|e| {
        eprintln!("database {}: {e}", path.display());
        std::process::exit(1)
    });
    match args.first().map(String::as_str) {
        Some("admin") => match admin::run(&db, &args[1..]) {
            Ok(out) => println!("{out}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2)
            }
        },
        None | Some("serve") => {
            let cfg = Config::from(&settings.file).unwrap_or_else(|e| {
                eprintln!("config {file}: {e}");
                std::process::exit(2)
            });
            let addr = settings.listen();
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind(&addr)
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("bind {addr}: {e}");
                        std::process::exit(1)
                    });
                let http = llm::client(cfg.llm.as_ref().and_then(|l| l.proxy.as_ref()));
                let app = Arc::new(App { db, cfg, http });
                if let Some(l) = &app.cfg.llm {
                    match &l.proxy {
                        Some(p) => log!(
                            "llm {} {} via {}",
                            l.endpoint.base,
                            l.endpoint.model,
                            config::proxy_display(p)
                        ),
                        None => log!("llm {} {}", l.endpoint.base, l.endpoint.model),
                    }
                    let (http, l) = (app.http.clone(), l.clone());
                    tokio::spawn(async move { llm::warm(&http, &l).await });
                } else {
                    log!("no LLM configured: dictations are not refined");
                }
                let p = &app.cfg.prices;
                if p.asr_hour == 0. && p.llm_in == 0. && p.llm_out == 0. {
                    log!("[prices] not set: requests cost nothing and credit is never spent");
                }
                log!("listening on {addr}");
                if let Err(e) = server::serve(app, listener).await {
                    eprintln!("{e}");
                    std::process::exit(1)
                }
            });
        }
        _ => {
            eprintln!(
                "usage: plaid-cloud [--config FILE] [serve]\n{}",
                admin::USAGE
            );
            std::process::exit(2)
        }
    }
}

/// The file holds the Volc and LLM keys; say so when other users can read it.
fn warn_if_readable(file: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = std::fs::metadata(file)
            && m.permissions().mode() & 0o077 != 0
        {
            eprintln!("warning: {file} holds keys and is readable by others (chmod 600)");
        }
    }
    #[cfg(not(unix))]
    let _ = file;
}
