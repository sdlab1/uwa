//! Per-conversation pinning state.
//!
//! Tracks what the *tab* has already seen, so a repeat turn can skip the
//! system prompt (tool schemas) and any messages the browser already holds.
//! The state is keyed by [`ConversationId`] and reset whenever the pinned
//! tab changes — the browser-side history is bound to a specific tab, not
//! to a conversation name.
//!
//! The two decisions the store supports:
//!
//! * **Full** — send everything, exactly as before. Chosen on the first
//!   turn, on any sign of divergence (edited history, changed tool set,
//!   different tab), or when pinning is disabled.
//! * **Delta** — send only `messages[messages_seen..]`, no system prompt.
//!   The browser's own turn-composer already strips assistant messages
//!   (see `uwa-tools::compose_browser_turn`), so this collapses to "the
//!   new user turn" in practice.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use uwa_core::types::openai::ChatMessage;
use uwa_core::{ConversationId, TabId};

use crate::hash::{fnv_byte, fnv_bytes, role_tag, FNV_OFFSET};

/// Everything we need to know about a conversation to decide what to send.
#[derive(Debug, Clone)]
pub struct ConversationState {
    /// The tab this state refers to. If the session manager reattaches
    /// the conversation to a different tab, the state is discarded.
    pub tab: TabId,
    /// Hash of `messages[..messages_seen]` — the client's history at the
    /// time of the last successful send.
    pub prefix_hash: u64,
    /// Number of client messages seen at the last send.
    pub messages_seen: usize,
    /// Hash of the system prompt (tool schemas) injected at the last send.
    /// A changed tool set forces a Full send — the browser must see the
    /// new schemas.
    pub system_hash: u64,
    pub last_updated: Instant,
}

/// What to send this turn.
#[derive(Debug)]
pub enum PinDecision {
    /// Send full history with system prompt.
    Full,
    /// Send only these messages. The caller must **not** prepend the
    /// system prompt — the browser already has it from an earlier turn.
    Delta { messages: Vec<ChatMessage> },
}

/// Manual impl: `ChatMessage` doesn't derive `PartialEq`, but tests need
/// to compare decisions. Equality compares role, content text, and
/// tool-call ids — enough to distinguish Full from a specific Delta.
impl PartialEq for PinDecision {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (PinDecision::Full, PinDecision::Full) => true,
            (PinDecision::Delta { messages: a }, PinDecision::Delta { messages: b }) => {
                a.len() == b.len()
                    && a.iter().zip(b.iter()).all(|(x, y)| {
                        x.role == y.role
                            && x.content_text() == y.content_text()
                            && x.tool_call_id == y.tool_call_id
                            && match (&x.tool_calls, &y.tool_calls) {
                                (None, None) => true,
                                (Some(x), Some(y)) => {
                                    x.len() == y.len()
                                        && x.iter().zip(y.iter()).all(|(p, q)| {
                                            p.id == q.id && p.function.name == q.function.name
                                        })
                                }
                                _ => false,
                            }
                    })
            }
            _ => false,
        }
    }
}

/// Decide what to send given the stored state (if any), the current tab,
/// the client's full history, and the current system-prompt hash.
///
/// The decision is deliberately conservative: any uncertainty falls back
/// to `Full`. A spurious Delta risks duplicating a turn in the browser;
/// a spurious Full only costs a few hundred characters of injection.
pub fn decide(
    state: Option<&ConversationState>,
    tab: &TabId,
    messages: &[ChatMessage],
    system_hash: u64,
) -> PinDecision {
    let Some(state) = state else {
        return PinDecision::Full;
    };
    if state.tab != *tab {
        return PinDecision::Full;
    }
    if state.system_hash != system_hash {
        return PinDecision::Full;
    }
    // No new messages since the last send — a client retry, or an
    // edit-and-resend. Both are safer to replay in full.
    if messages.len() <= state.messages_seen {
        return PinDecision::Full;
    }
    // The client's history must extend what we saw before, byte for byte.
    // Any divergence (edited message, truncated history, new conversation
    // reusing the same tab) forces a full replay.
    // Slicing a Vec by a runtime index is a forbid-level lint; `get()`
    // returning None (seen-count past the current history) is by definition
    // a divergence → full replay.
    let Some(prefix) = messages.get(..state.messages_seen) else {
        return PinDecision::Full;
    };
    if hash_messages(prefix) != state.prefix_hash {
        return PinDecision::Full;
    }
    match messages.get(state.messages_seen..) {
        Some(rest) => PinDecision::Delta {
            messages: rest.to_vec(),
        },
        None => PinDecision::Full,
    }
}

/// In-memory map with a TTL sweep. Bounded implicitly by the number of
/// active conversations; the sweep drops entries whose last update is
/// older than `ttl`.
pub struct PinningStore {
    inner: RwLock<HashMap<ConversationId, ConversationState>>,
    ttl: Duration,
}

impl PinningStore {
    /// Default TTL: 30 minutes. Long enough to cover a normal coding
    /// session's pauses; short enough that an abandoned conversation
    /// doesn't hold state overnight.
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            ttl,
        }
    }

    pub async fn get(&self, cid: &ConversationId) -> Option<ConversationState> {
        self.inner.read().await.get(cid).cloned()
    }

    pub async fn put(&self, cid: ConversationId, state: ConversationState) {
        self.inner.write().await.insert(cid, state);
    }

    /// Evict entries older than `ttl`. Returns how many were dropped.
    pub async fn sweep(&self) -> usize {
        let now = Instant::now();
        let ttl = self.ttl;
        let mut g = self.inner.write().await;
        let before = g.len();
        g.retain(|_, s| now.duration_since(s.last_updated) < ttl);
        before.saturating_sub(g.len())
    }

    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Hash functions
// ---------------------------------------------------------------------------

/// Hash a message sequence. Two sequences with the same roles, contents,
/// and tool calls produce the same hash; a single byte changed in any
/// message flips it. Fields the browser can't observe (name, tool_call_id
/// on the tool message) are folded in anyway — a diff that flips them
/// safely forces a Full decision.
pub fn hash_messages(messages: &[ChatMessage]) -> u64 {
    let mut h = FNV_OFFSET;
    for m in messages {
        h = fnv_byte(h, role_tag(m.role));
        h = fnv_byte(h, 0); // separator
        h = fnv_bytes(h, m.content_text().as_bytes());
        h = fnv_byte(h, 0xff); // hard separator between messages
        if let Some(tcs) = &m.tool_calls {
            for tc in tcs {
                h = fnv_bytes(h, tc.function.name.as_bytes());
                h = fnv_byte(h, b'=');
                h = fnv_bytes(h, tc.function.arguments.as_bytes());
                h = fnv_byte(h, 0xfe);
            }
        }
        if let Some(id) = &m.tool_call_id {
            h = fnv_bytes(h, id.as_bytes());
            h = fnv_byte(h, 0xfd);
        }
    }
    h
}

/// Hash a system-prompt string. `None` hashes to a stable sentinel.
pub fn hash_system(system: Option<&str>) -> u64 {
    let mut h = FNV_OFFSET;
    match system {
        None => h = fnv_byte(h, 0x00),
        Some(s) => {
            h = fnv_byte(h, 0x01);
            h = fnv_bytes(h, s.as_bytes());
        }
    }
    h
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use uwa_core::types::openai::{FunctionCall, MessageContent, ToolCallRef};
    use uwa_core::types::Role;

    fn user(s: &str) -> ChatMessage {
        ChatMessage::text(Role::User, s)
    }
    fn assistant(s: &str) -> ChatMessage {
        ChatMessage::text(Role::Assistant, s)
    }

    fn state(tab: &str, seen: usize, prefix_hash: u64, sys_hash: u64) -> ConversationState {
        ConversationState {
            tab: TabId::from_raw(tab),
            prefix_hash,
            messages_seen: seen,
            system_hash: sys_hash,
            last_updated: Instant::now(),
        }
    }

    #[test]
    fn no_state_is_full() {
        let tab = TabId::from_raw("t1");
        let msgs = vec![user("hi")];
        assert_eq!(decide(None, &tab, &msgs, 0), PinDecision::Full);
    }

    #[test]
    fn different_tab_is_full() {
        let current_tab = TabId::from_raw("t2");
        let msgs = vec![user("hi")];
        let st = state("t1", 1, hash_messages(&[user("hi")]), 0);
        assert_eq!(decide(Some(&st), &current_tab, &msgs, 0), PinDecision::Full);
    }

    #[test]
    fn changed_system_hash_is_full() {
        let tab = TabId::from_raw("t1");
        let msgs = vec![user("hi"), assistant("yo"), user("again")];
        let st = state("t1", 1, hash_messages(&[user("hi")]), 100);
        assert_eq!(decide(Some(&st), &tab, &msgs, 200), PinDecision::Full);
    }

    #[test]
    fn same_length_is_full() {
        let tab = TabId::from_raw("t1");
        let msgs = vec![user("hi")];
        let st = state("t1", 1, hash_messages(&[user("hi")]), 0);
        assert_eq!(decide(Some(&st), &tab, &msgs, 0), PinDecision::Full);
    }

    #[test]
    fn truncated_history_is_full() {
        let tab = TabId::from_raw("t1");
        let msgs = vec![user("hi")];
        let st = state("t1", 5, 0, 0);
        assert_eq!(decide(Some(&st), &tab, &msgs, 0), PinDecision::Full);
    }

    #[test]
    fn edited_prefix_is_full() {
        let tab = TabId::from_raw("t1");
        let old_prefix = vec![user("hi")];
        let msgs = vec![user("hi-edited"), assistant("yo"), user("next")];
        let st = state("t1", 1, hash_messages(&old_prefix), 0);
        assert_eq!(decide(Some(&st), &tab, &msgs, 0), PinDecision::Full);
    }

    #[test]
    fn happy_path_is_delta() {
        let tab = TabId::from_raw("t1");
        let seen = vec![user("hi"), assistant("yo"), user("how are you")];
        let msgs = vec![
            user("hi"),
            assistant("yo"),
            user("how are you"),
            assistant("fine"),
            user("great"),
        ];
        let st = state("t1", 3, hash_messages(&seen), 0);
        match decide(Some(&st), &tab, &msgs, 0) {
            PinDecision::Delta { messages } => {
                assert_eq!(messages.len(), 2);
                assert!(
                    matches!(&messages[0].content, Some(MessageContent::Text(s)) if s == "fine")
                );
                assert!(
                    matches!(&messages[1].content, Some(MessageContent::Text(s)) if s == "great")
                );
            }
            other => panic!("expected Delta, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_history_also_deltas() {
        let tab = TabId::from_raw("t1");
        let seen = vec![user("weather?")];
        let msgs = vec![
            user("weather?"),
            ChatMessage {
                role: Role::Assistant,
                content: None,
                name: None,
                tool_call_id: None,
                tool_calls: Some(vec![ToolCallRef {
                    id: "call_1".into(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: "get_weather".into(),
                        arguments: "{\"city\":\"NYC\"}".into(),
                    },
                }]),
                uwa_media: Vec::new(),
            },
            ChatMessage {
                role: Role::Tool,
                content: Some(MessageContent::Text("22C".into())),
                name: None,
                tool_call_id: Some("call_1".into()),
                tool_calls: None,
                uwa_media: Vec::new(),
            },
        ];
        let st = state("t1", 1, hash_messages(&seen), 0);
        match decide(Some(&st), &tab, &msgs, 0) {
            PinDecision::Delta { messages } => assert_eq!(messages.len(), 2),
            other => panic!("expected Delta, got {other:?}"),
        }
    }

    #[test]
    fn hash_is_stable_across_calls() {
        let m = vec![user("hi"), assistant("yo")];
        assert_eq!(hash_messages(&m), hash_messages(&m));
    }

    #[test]
    fn hash_changes_with_content() {
        assert_ne!(hash_messages(&[user("hi")]), hash_messages(&[user("hi!")]));
    }

    #[test]
    fn hash_changes_with_role() {
        assert_ne!(
            hash_messages(&[ChatMessage::text(Role::User, "x")]),
            hash_messages(&[ChatMessage::text(Role::Assistant, "x")]),
        );
    }

    #[test]
    fn hash_system_distinguishes_none_from_empty() {
        assert_ne!(hash_system(None), hash_system(Some("")));
        assert_ne!(hash_system(Some("a")), hash_system(Some("b")));
    }

    #[tokio::test]
    async fn store_get_put_sweep() {
        let store = PinningStore::new(Duration::from_millis(20));
        let cid = ConversationId::from_raw("c1");
        let st = state("t1", 1, 0, 0);
        store.put(cid.clone(), st.clone()).await;
        assert!(store.get(&cid).await.is_some());
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(store.sweep().await, 1);
        assert!(store.get(&cid).await.is_none());
    }
}
