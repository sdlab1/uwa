//! Dump rendered HTML from a live Chromium over CDP.
//!
//! Used to record the HTML fixtures behind the extraction snapshot tests:
//!
//! ```sh
//! # Main frame only
//! cargo run -p uwa-providers --features snapshot --bin uwa-snapshot -- \
//!     --cdp http://127.0.0.1:9222 --url https://example.com/chat --out fixture.html
//!
//! # Include child frames (same-process iframes)
//! cargo run -p uwa-providers --features snapshot --bin uwa-snapshot -- \
//!     --cdp http://127.0.0.1:9222 --url https://example.com/chat \
//!     --out fixture.html --include-oopifs
//! ```
//!
//! When `--include-oopifs` is set, the binary also dumps the HTML of every
//! child frame reachable from the page's session. Same-origin iframes are
//! fully dumped via `eval_in_frame`. Cross-origin OOPIFs (separate renderer
//! process) are listed with their frame IDs and URLs — their internal HTML
//! requires session-scoped CDP commands (chromiumoxide 0.7.0 limitation;
//! see todo/08.md Part C).

use std::path::PathBuf;
use std::time::Duration;

use uwa_browser::CdpTransport;
use uwa_core::{Page, Transport};

fn usage() -> ! {
    eprintln!(
        "usage: uwa-snapshot --url <url> [--cdp <http://host:port>] \
         [--selector <css>] [--wait-ms <n>] [--out <file>] [--include-oopifs] \
         [--oopif-settle-ms <n>]"
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
    let mut cdp =
        std::env::var("UWA_CDP_URL").unwrap_or_else(|_| "http://127.0.0.1:9222".to_string());
    let mut url: Option<String> = None;
    let mut selector: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut wait_ms: u64 = 20_000;
    let mut include_oopifs = false;
    let mut oopif_settle_ms: u64 = 2_000;

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
            "--include-oopifs" => include_oopifs = true,
            "--oopif-settle-ms" => {
                let v = next(&mut i);
                oopif_settle_ms = v.parse().unwrap_or_else(|_| usage());
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

    let transport = CdpTransport::connect(
        &cdp,
        Duration::from_secs(30),
        Some(uwa_stealth::default_pack()),
    )
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

    // Give OOPIF auto-attach time to register sessions before dumping.
    if include_oopifs {
        eprintln!("uwa-snapshot: settling {oopif_settle_ms}ms for OOPIF auto-attach");
        tokio::time::sleep(Duration::from_millis(oopif_settle_ms)).await;
    }

    let html = page.html().await.unwrap_or_else(|e| fail("html", e));

    match &out {
        Some(path) => {
            std::fs::write(path, &html)
                .unwrap_or_else(|e| fail(&format!("write {}", path.display()), e));
            eprintln!(
                "uwa-snapshot: wrote {} ({} bytes)",
                path.display(),
                html.len()
            );

            // Dump child frames if requested.
            if include_oopifs {
                dump_child_frames(&page, &transport, path).await;
            }
        }
        None => {
            print!("{html}");
            if include_oopifs {
                dump_child_frames(&page, &transport, &PathBuf::from("fixture.html")).await;
            }
        }
    }

    drop(page);
    drop(transport);
}

/// Dump the HTML of every child frame reachable from the page.
///
/// Same-origin iframes: fully dumped via `eval_in_frame`.
/// Cross-origin OOPIFs: frame ID + URL logged; internal HTML requires
/// session-scoped CDP commands (not available in chromiumoxide 0.7.0).
async fn dump_child_frames(page: &Box<dyn Page>, transport: &CdpTransport, main_path: &PathBuf) {
    let frames = match page.frame_tree().await {
        Ok(f) => f,
        Err(e) => {
            eprintln!("uwa-snapshot: frame_tree failed: {e}");
            return;
        }
    };

    // Skip the main frame (index 0); dump the rest.
    let base = main_path.with_extension("");
    let base_str = base.to_string_lossy().to_string();

    for (idx, (frame_id, frame_url)) in frames.iter().enumerate().skip(1) {
        let frame_path = format!("{base_str}.frame_{}.html", idx - 1);

        // Try to evaluate inside this frame to get its HTML.
        match page
            .eval_in_frame(frame_id, "document.documentElement.outerHTML")
            .await
        {
            Ok(v) => {
                if let Some(frame_html) = v.as_str() {
                    let path = PathBuf::from(&frame_path);
                    match std::fs::write(&path, frame_html) {
                        Ok(()) => {
                            eprintln!(
                                "uwa-snapshot: wrote {} ({} bytes, url={})",
                                path.display(),
                                frame_html.len(),
                                frame_url
                            );
                        }
                        Err(e) => {
                            eprintln!("uwa-snapshot: write {} failed: {e}", path.display());
                        }
                    }
                } else {
                    eprintln!("uwa-snapshot: frame {idx} returned non-string HTML");
                }
            }
            Err(e) => {
                // Cross-origin OOPIF: we can see the frame in the tree but
                // can't reach into it from the parent session.
                eprintln!(
                    "uwa-snapshot: frame {idx} (id={frame_id}, url={frame_url}) not reachable: {e}"
                );
                eprintln!(
                    "  → OOPIF HTML requires session-scoped CDP commands; \
                     frame is tracked in OopifRegistry for future use"
                );
            }
        }
    }

    // Report OOPIF sessions from the registry for diagnostics.
    let sessions = transport.oopif().all_sessions().await;
    if !sessions.is_empty() {
        eprintln!("uwa-snapshot: OOPIF sessions in registry:");
        for s in &sessions {
            eprintln!(
                "  target={} type={} frame={:?} url={}",
                s.target_id.inner(),
                s.target_type,
                s.frame_id,
                s.url
            );
        }
    }
}
