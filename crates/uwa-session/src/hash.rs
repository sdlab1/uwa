//! Conversation id from prefix of history. Same id across tool-loop rounds.
use std::hash::{Hash, Hasher};
use uwa_core::types::openai::ChatMessage;
use uwa_core::ConversationId;

const PREFIX: usize = 2;

pub fn conversation_id(messages: &[ChatMessage]) -> ConversationId {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for m in messages.iter().take(PREFIX) {
        role_str(m.role).hash(&mut h);
        m.content_text().hash(&mut h);
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
}
