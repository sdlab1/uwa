//! Dump rendered HTML from a live Chromium over CDP.
//!
//! Used to record the HTML fixtures behind the extraction snapshot tests:
//!
//! ```sh
//! cargo run -p uwa-providers --features snapshot --bin uwa-snapshot -- \
//!     --cdp http://127.0.0.1:9222 --url https://example.com/chat --out fixture.html
//! ```
//!
//! It only *connects* — the Chromium itself is started/owned by the caller
//! (see `UWA_CDP_URL`).

use std::path::PathBuf;
use std::time::Duration;

use uwa_browser::CdpTransport;
use uwa_core::Transport;

fn usage() -> ! {
    eprintln!(
        "usage: uwa-snapshot --url <url> [--cdp <http://host:port>] \
         [--selector <css>] [--wait-ms <n>] [--out <file>]"
    );
    std::process::exit(2);
}

fn fail(what: &str, err: impl std::fmt::Display) -> ! {
    eprintln!("uwa-snapshot: {what}: {err}");
    std::process::exit(1);
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cdp = std::env::var("UWA_CDP_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9222".to_string());
    let mut url: Option<String> = None;
    let mut selector: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut wait_ms: u64 = 20_000;

    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let next = |i: &mut usize| -> String {
            *i += 1;
            args.get(*i).cloned().unwrap_or_else(|| usage())
        };
        match flag {
            "--cdp" => cdp = next(&mut i),
            "--url" => url = Some(next(&mut i)),
            "--selector" => selector = Some(next(&mut i)),
            "--out" => out = Some(PathBuf::from(next(&mut i))),
            "--wait-ms" => {
                let v = next(&mut i);
                wait_ms = v.parse().unwrap_or_else(|_| usage());
            }
            "-h" | "--help" => usage(),
            other => {
                eprintln!("uwa-snapshot: unknown flag {other}");
                usage();
            }
        }
        i += 1;
    }
    let url = url.unwrap_or_else(|| usage());
    let target = url::Url::parse(&url).unwrap_or_else(|e| fail("bad --url", e));

    let transport =
        CdpTransport::connect(&cdp, Duration::from_secs(30), Some(uwa_stealth::default_pack()))
            .await
            .unwrap_or_else(|e| fail(&format!("connect {cdp}"), e));

    let tabs = transport
        .list_tabs()
        .await
        .unwrap_or_else(|e| fail("list_tabs", e));
    let Some(tab) = tabs.first().cloned() else {
        eprintln!("uwa-snapshot: no tabs on {cdp}");
        std::process::exit(1);
    };

    let page = transport
        .page(&tab)
        .await
        .unwrap_or_else(|e| fail("page", e));
    page.goto(&target)
        .await
        .unwrap_or_else(|e| fail(&format!("goto {target}"), e));
    if let Some(sel) = &selector {
        page.wait_for_selector(sel, Duration::from_millis(wait_ms))
            .await
            .unwrap_or_else(|e| fail(&format!("wait_for_selector {sel}"), e));
    }
    let html = page.html().await.unwrap_or_else(|e| fail("html", e));

    match out {
        Some(path) => {
            std::fs::write(&path, &html).unwrap_or_else(|e| fail(&format!("write {}", path.display()), e));
            eprintln!("uwa-snapshot: wrote {} ({} bytes)", path.display(), html.len());
        }
        None => print!("{html}"),
    }

    drop(page);
    drop(transport);
}
