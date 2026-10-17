//! Pagination and filters over `/admin/history`.

use serde_json::{json, Value};
use std::sync::Arc;
use uwa_history::{HistoryCfg, HistoryStore};
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

async fn app() -> axum_test::TestServer {
    // Buffer big enough that none of these records are evicted.
    let store = Arc::new(
        HistoryStore::new(
            HistoryCfg {
                buffer_size: 500,
                ..Default::default()
            },
            false,
        )
        .await
        .unwrap_or_else(|e| unreachable!("history store: {e}")),
    );
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .with_history(store)
        .build()
        .server
}

async fn chat(s: &axum_test::TestServer, user: &str) {
    let r = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role":"user","content": user}]
        }))
        .await;
    r.assert_status_ok();
}

async fn get(s: &axum_test::TestServer, q: &str) -> Value {
    let r = s
        .get(&format!("/admin/history{q}"))
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
    r.json()
}

fn ids_of(v: &Value) -> Vec<String> {
    v.get("records")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .map(|r| {
                    r.get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn pagination_walks_the_whole_buffer() {
    let s = app().await;
    for i in 0..7 {
        chat(&s, &format!("msg {i}")).await;
    }

    let page1 = get(&s, "?limit=3").await;
    assert_eq!(page1.get("count").and_then(|c| c.as_u64()), Some(3));
    assert_eq!(
        page1.get("total_matching").and_then(|c| c.as_u64()),
        Some(7)
    );
    let next = page1
        .get("next_before_id")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(!next.is_empty(), "page1 must carry a cursor: {page1}");
    let ids1 = ids_of(&page1);

    let page2 = get(&s, &format!("?limit=3&before_id={next}")).await;
    assert_eq!(page2.get("count").and_then(|c| c.as_u64()), Some(3));
    let next2 = page2
        .get("next_before_id")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();
    let ids2 = ids_of(&page2);

    let page3 = get(&s, &format!("?limit=3&before_id={next2}")).await;
    assert_eq!(page3.get("count").and_then(|c| c.as_u64()), Some(1));
    assert!(page3
        .get("next_before_id")
        .and_then(|c| c.as_null())
        .is_some());
    let ids3 = ids_of(&page3);

    // No overlap across pages, all 7 unique.
    let all: Vec<String> = ids1.into_iter().chain(ids2).chain(ids3).collect();
    let unique: std::collections::HashSet<_> = all.iter().collect();
    assert_eq!(all.len(), 7);
    assert_eq!(unique.len(), 7, "pagination repeated an id");
}

#[tokio::test]
async fn filter_by_profile_narrows_results() {
    let s = app().await;
    // The testkit config has no profiles; filtering by `default` should
    // match everything; filtering by a non-default name matches nothing.
    for _ in 0..3 {
        chat(&s, "hi").await;
    }
    let default = get(&s, "?profile=default").await;
    assert_eq!(
        default.get("total_matching").and_then(|c| c.as_u64()),
        Some(3)
    );
    let work = get(&s, "?profile=work").await;
    assert_eq!(work.get("total_matching").and_then(|c| c.as_u64()), Some(0));
    assert_eq!(work.get("count").and_then(|c| c.as_u64()), Some(0));
}

#[tokio::test]
async fn filter_by_model_and_provider_compose() {
    let s = app().await;
    for _ in 0..2 {
        chat(&s, "a").await;
    }
    // The testkit config aliases `gpt-4o` → `chatgpt`.
    let hit = get(&s, "?model=gpt-4o&provider=chatgpt").await;
    assert_eq!(hit.get("total_matching").and_then(|c| c.as_u64()), Some(2));

    let miss = get(&s, "?model=claude-3-5-sonnet").await;
    assert_eq!(miss.get("total_matching").and_then(|c| c.as_u64()), Some(0));

    // The handler returns 400 for an unknown status.
    let r = s
        .get("/admin/history?status=nonsense")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_bad_request();
}

#[tokio::test]
async fn unknown_before_id_returns_empty_page() {
    let s = app().await;
    chat(&s, "a").await;
    let v = get(&s, "?limit=10&before_id=does-not-exist").await;
    assert_eq!(v.get("count").and_then(|c| c.as_u64()), Some(0));
    assert_eq!(v.get("total_matching").and_then(|c| c.as_u64()), Some(0));
    assert!(v.get("next_before_id").and_then(|c| c.as_null()).is_some());
}

#[tokio::test]
async fn empty_result_has_no_cursor() {
    let s = app().await;
    let v = get(&s, "?limit=10").await;
    assert_eq!(v.get("count").and_then(|c| c.as_u64()), Some(0));
    assert_eq!(v.get("total_matching").and_then(|c| c.as_u64()), Some(0));
    assert!(v.get("next_before_id").and_then(|c| c.as_null()).is_some());
}

#[tokio::test]
async fn limit_is_capped() {
    // `limit=99999` should not crash; the store clamps it. We can't
    // assert the exact clamp value from outside, only that the request
    // succeeds and returns at most what we produced.
    let s = app().await;
    for _ in 0..5 {
        chat(&s, "x").await;
    }
    let v = get(&s, "?limit=99999").await;
    assert_eq!(v.get("count").and_then(|c| c.as_u64()), Some(5));
    assert!(v.get("next_before_id").and_then(|c| c.as_null()).is_some());
}
