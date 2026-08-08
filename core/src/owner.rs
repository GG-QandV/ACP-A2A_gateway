//! core/src/owner.rs
//!
//! Владелец разговора и задачи. Вынесен из convert.rs в отдельный
//! модуль, потому что после закрытия аудита P1-2 им пользуется ещё и
//! task_store: владелец должен переживать выселение сессии, иначе
//! проверка «чья задача?» работает только пока разговор жив.
//!
//! Хранится хеш токена, а не сам токен: для ответа на вопрос «тот же
//! клиент?» достаточно равенства, а держать секрет в памяти и на диске
//! дольше необходимого незачем.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Owner {
    /// Вызовы через голый трейт `A2aAgent`, без транспортного контекста.
    /// Отдельная корзина: анонимные вызовы изолированы от токенных.
    Anonymous,
    Token { hash: u64 },
}

impl Owner {
    pub fn from_token(token: &str) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        token.hash(&mut hasher);
        Owner::Token { hash: hasher.finish() }
    }

    /// Хеш не является криптографическим и предназначен только для
    /// сравнения на равенство. Восстановить токен по нему нельзя,
    /// но и полагаться на него как на секрет не следует.
    pub fn is_anonymous(&self) -> bool {
        matches!(self, Owner::Anonymous)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_token_gives_same_owner() {
        assert_eq!(Owner::from_token("t-1"), Owner::from_token("t-1"));
    }

    #[test]
    fn different_tokens_give_different_owners() {
        assert_ne!(Owner::from_token("t-1"), Owner::from_token("t-2"));
    }

    #[test]
    fn anonymous_never_equals_token_owner() {
        assert_ne!(Owner::Anonymous, Owner::from_token("t-1"));
    }

    #[test]
    fn owner_survives_serde_roundtrip() {
        let owner = Owner::from_token("t-1");
        let json = serde_json::to_string(&owner).unwrap();
        let restored: Owner = serde_json::from_str(&json).unwrap();
        assert_eq!(owner, restored);
    }
}
