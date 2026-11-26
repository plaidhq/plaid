//! `plaid-cloud [serve]` runs the server; `plaid-cloud admin …` manages accounts. Settings come from the
//! environment (docs/cloud.md).
use plaid_cloud::config::{self, Config};
use plaid_cloud::db::Db;
use plaid_cloud::server::{self, App};
use plaid_cloud::{admin, llm, log};
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = std::env::var("PLAID_CLOUD_DB").unwrap_or_else(|_| "plaid-cloud.db".into());
    let db = Db::open(&path).unwrap_or_else(|e| {
        eprintln!("database {path}: {e}");
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
            let cfg = Config::from_env().unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(2)
            });
            let addr =
                std::env::var("PLAID_CLOUD_ADDR").unwrap_or_else(|_| "127.0.0.1:8787".into());
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
                    log!("PLAID_PRICE_* not set: requests cost nothing and credit is never spent");
                }
                log!("listening on {addr}");
                if let Err(e) = server::serve(app, listener).await {
                    eprintln!("{e}");
                    std::process::exit(1)
                }
            });
        }
        _ => {
            eprintln!("usage: plaid-cloud [serve]\n{}", admin::USAGE);
            std::process::exit(2)
        }
    }
}
