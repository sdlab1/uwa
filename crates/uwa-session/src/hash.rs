//! Conversation id from prefix of history, plus the FNV-1a helpers shared with
//! the pinning hashes.
use uwa_core::types::openai::ChatMessage;
use uwa_core::types::Role;
use uwa_core::ConversationId;

/// FNV-1a 64-bit. Deterministic across runs *and* Rust versions (unlike
/// `DefaultHasher`, whose algorithm std may change), so ids and hashes can be
/// compared across restarts and quoted in logs.
pub(crate) const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[inline]
pub(crate) fn fnv_byte(h: u64, b: u8) -> u64 {
    (h ^ u64::from(b)).wrapping_mul(FNV_PRIME)
}

#[inline]
pub(crate) fn fnv_bytes(h: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(h, |acc, &b| fnv_byte(acc, b))
}

pub(crate) fn role_tag(role: Role) -> u8 {
    match role {
        Role::System => 1,
        Role::User => 2,
        Role::Assistant => 3,
        Role::Tool => 4,
    }
}

/// Identify a conversation by its stable *opening*: every leading system
/// message plus the first non-system message (the user's first turn).
///
/// The anchor must not depend on how far the conversation has progressed.
/// Hashing a fixed "first N messages" window broke that for clients without
/// a system prompt: turn 1 is `[user]` but turn 2 is `[user, assistant, user]`,
/// so a `take(2)` window hashed `[user]` and then `[user, assistant]` - two
/// different ids for one conversation, which moved it to a fresh tab and
/// defeated pinning.
///
/// An optional `provider` parameter scopes the conversation to a specific
/// provider (e.g. "chatgpt", "claude"), so the same user opening routed
/// to different providers gets distinct conversation ids and separate
/// pinning state.
pub fn conversation_id(messages: &[ChatMessage], provider: Option<&str>) -> ConversationId {
    let mut h = FNV_OFFSET;
    // Hash the provider first (if given) so it scopes the entire id.
    if let Some(p) = provider {
        h = fnv_bytes(h, p.as_bytes());
        h = fnv_byte(h, 0x00); // provider separator
    }
    for m in messages {
        h = fnv_byte(h, role_tag(m.role));
        h = fnv_byte(h, 0); // role/content separator
        h = fnv_bytes(h, m.content_text().as_bytes());
        h = fnv_byte(h, 0xff); // message terminator (never valid UTF-8)
        if !matches!(m.role, Role::System) {
            break;
        }
    }
    ConversationId::from_raw(format!("conv_{h:016x}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uwa_core::types::Role;

    #[test]
    fn same_prefix_same_id() {
        let a = vec![
            ChatMessage::text(Role::System, "s"),
            ChatMessage::text(Role::User, "q"),
            ChatMessage::text(Role::Assistant, "a1"),
        ];
        let b = vec![
            ChatMessage::text(Role::System, "s"),
            ChatMessage::text(Role::User, "q"),
            ChatMessage::text(Role::Assistant, "different later round"),
        ];
        assert_eq!(conversation_id(&a, None), conversation_id(&b, None));
    }

    #[test]
    fn different_prefix_different_id() {
        let a = vec![ChatMessage::text(Role::User, "q1")];
        let b = vec![ChatMessage::text(Role::User, "q2")];
        assert_ne!(conversation_id(&a, None), conversation_id(&b, None));
    }

    #[test]
    fn fnv_matches_the_published_test_vectors() {
        assert_eq!(fnv_bytes(FNV_OFFSET, b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv_bytes(FNV_OFFSET, b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv_bytes(FNV_OFFSET, b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn id_is_a_pinned_golden_value() {
        let msgs = vec![
            ChatMessage::text(Role::System, "sys"),
            ChatMessage::text(Role::User, "q"),
        ];
        assert_eq!(
            conversation_id(&msgs, None).as_str(),
            "conv_1393131233e47b38"
        );
    }

    #[test]
    fn leading_system_only_history_does_not_panic() {
        let msgs = vec![
            ChatMessage::text(Role::System, "a"),
            ChatMessage::text(Role::System, "b"),
        ];
        assert!(conversation_id(&msgs, None).as_str().starts_with("conv_"));
        assert!(conversation_id(&[], None).as_str().starts_with("conv_"));
    }

    #[test]
    fn identical_openings_share_an_id() {
        let a = vec![
            ChatMessage::text(Role::User, "hi"),
            ChatMessage::text(Role::Assistant, "one"),
        ];
        let b = vec![
            ChatMessage::text(Role::User, "hi"),
            ChatMessage::text(Role::Assistant, "two"),
        ];
        assert_eq!(conversation_id(&a, None), conversation_id(&b, None));
    }
}
