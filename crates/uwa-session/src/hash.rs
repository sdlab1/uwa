//! Conversation id from prefix of history. Same id across tool-loop rounds.
use std::hash::{Hash, Hasher};
use uwa_core::types::openai::ChatMessage;
use uwa_core::ConversationId;

/// Identify a conversation by its stable *opening*: every leading system
/// message plus the first non-system message (the user's first turn).
///
/// The anchor must not depend on how far the conversation has progressed.
/// Hashing a fixed "first N messages" window broke that for clients without
/// a system prompt: turn 1 is `[user]` but turn 2 is `[user, assistant, user]`,
/// so a `take(2)` window hashed `[user]` and then `[user, assistant]` - two
/// different ids for one conversation, which moved it to a fresh tab and
/// defeated pinning.
pub fn conversation_id(messages: &[ChatMessage]) -> ConversationId {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for m in messages {
        role_str(m.role).hash(&mut h);
        m.content_text().hash(&mut h);
        if !matches!(m.role, uwa_core::types::Role::System) {
            break;
        }
    }
    ConversationId::from_raw(format!("conv_{:x}", h.finish()))
}

fn role_str(r: uwa_core::types::Role) -> u8 {
    match r {
        uwa_core::types::Role::System => 0,
        uwa_core::types::Role::User => 1,
        uwa_core::types::Role::Assistant => 2,
        uwa_core::types::Role::Tool => 3,
    }
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
        assert_eq!(conversation_id(&a), conversation_id(&b));
    }

    #[test]
    fn different_prefix_different_id() {
        let a = vec![ChatMessage::text(Role::User, "q1")];
        let b = vec![ChatMessage::text(Role::User, "q2")];
        assert_ne!(conversation_id(&a), conversation_id(&b));
    }

    #[test]
    fn id_is_stable_as_the_conversation_grows_without_a_system_prompt() {
        // Regression: turn 2 used to hash `[user, assistant]` while turn 1
        // hashed `[user]`, so every follow-up looked like a new conversation.
        let turn1 = vec![ChatMessage::text(Role::User, "q")];
        let turn2 = vec![
            ChatMessage::text(Role::User, "q"),
            ChatMessage::text(Role::Assistant, "a"),
            ChatMessage::text(Role::User, "q2"),
        ];
        let turn3 = vec![
            ChatMessage::text(Role::User, "q"),
            ChatMessage::text(Role::Assistant, "a"),
            ChatMessage::text(Role::User, "q2"),
            ChatMessage::text(Role::Assistant, "a2"),
            ChatMessage::text(Role::User, "q3"),
        ];
        assert_eq!(conversation_id(&turn1), conversation_id(&turn2));
        assert_eq!(conversation_id(&turn2), conversation_id(&turn3));
    }

    #[test]
    fn id_is_stable_with_system_prompt_and_tool_rounds() {
        let first = vec![
            ChatMessage::text(Role::System, "sys"),
            ChatMessage::text(Role::User, "q"),
        ];
        let later = vec![
            ChatMessage::text(Role::System, "sys"),
            ChatMessage::text(Role::User, "q"),
            ChatMessage::text(Role::Assistant, "calling"),
            ChatMessage::text(Role::Tool, "result"),
        ];
        assert_eq!(conversation_id(&first), conversation_id(&later));
    }

    #[test]
    fn id_distinguishes_system_prompt_and_first_user_turn() {
        let base = vec![
            ChatMessage::text(Role::System, "sys"),
            ChatMessage::text(Role::User, "q"),
        ];
        let other_sys = vec![
            ChatMessage::text(Role::System, "SYS"),
            ChatMessage::text(Role::User, "q"),
        ];
        let other_user = vec![
            ChatMessage::text(Role::System, "sys"),
            ChatMessage::text(Role::User, "Q"),
        ];
        assert_ne!(conversation_id(&base), conversation_id(&other_sys));
        assert_ne!(conversation_id(&base), conversation_id(&other_user));
    }

    #[test]
    fn id_does_not_confuse_role_or_message_boundaries() {
        // ("ab","c") vs ("a","bc") and system-vs-user with equal text.
        let a = vec![
            ChatMessage::text(Role::System, "ab"),
            ChatMessage::text(Role::User, "c"),
        ];
        let b = vec![
            ChatMessage::text(Role::System, "a"),
            ChatMessage::text(Role::User, "bc"),
        ];
        let c = vec![ChatMessage::text(Role::User, "x")];
        let d = vec![ChatMessage::text(Role::System, "x")];
        assert_ne!(conversation_id(&a), conversation_id(&b));
        assert_ne!(conversation_id(&c), conversation_id(&d));
    }

    #[test]
    fn id_of_empty_history_is_well_defined() {
        assert_eq!(conversation_id(&[]), conversation_id(&[]));
    }
}
