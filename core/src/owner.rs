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

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Дефолтный ключ — ТОЛЬКО для локальной разработки. В проде
/// обязательно задать GATEWAY_HMAC_KEY через окружение.
const DEFAULT_DEV_KEY: &str = "default-dev-key-do-not-use-in-prod";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Owner {
    /// Вызовы через голый трейт `A2aAgent`, без транспортного контекста.
    /// Отдельная корзина: анонимные вызовы изолированы от токенных.
    Anonymous,
    Token {
        hash: u64,
    },
}

impl Owner {
    pub fn from_token(token: &str) -> Self {
        // ИСПРАВЛЕНО (TECH_DEBT: хеш токена — HMAC): RandomState заменён
        // на HMAC-SHA256 с ключом из {env:GATEWAY_HMAC_KEY}. Это
        // криптографический хеш, а не просто SipHash с случайным seed.
        // Первые 8 байт HMAC идут в hash: u64 — формат Owner::Token не
        // изменился, StoredTask без миграции.
        let key = std::env::var("GATEWAY_HMAC_KEY").unwrap_or_else(|_| DEFAULT_DEV_KEY.to_string());
        let mut mac =
            HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC accepts any key length");
        mac.update(token.as_bytes());
        let result = mac.finalize();
        let hash_bytes: [u8; 8] = result.into_bytes()[..8].try_into().unwrap();
        Owner::Token {
            hash: u64::from_le_bytes(hash_bytes),
        }
    }

    /// Хеш не является криптографическим и предназначен только для
    /// сравнения на равенство. Восстановить токен по нему нельзя,
    /// но и полагаться на него как на секрет не следует.
    pub fn is_anonymous(&self) -> bool {
        matches!(self, Owner::Anonymous)
    }

    /// Возвращает true, если токен даёт тот же Owner, что и этот.
    /// Нужно для тестов, чтобы проверить, что HMAC детерминирован.
    #[cfg(test)]
    pub fn same_token_as(&self, token: &str) -> bool {
        *self == Owner::from_token(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_token_gives_same_owner() {
        assert_eq!(Owner::from_token("t-1"), Owner::from_token("t-1"));
        // С HMAC это остаётся верным: один токен → один хеш (детерминизм).
    }

    #[test]
    fn different_tokens_give_different_owners() {
        assert_ne!(Owner::from_token("t-1"), Owner::from_token("t-2"));
        // С HMAC это тоже остаётся верным: разные токены → разные хеши.
    }

    #[test]
    fn hmac_is_deterministic_for_same_token() {
        // Проверяем, что from_token детерминирован (один токен → один Owner)
        // даже с HMAC. Это не тест на криптографическую стойкость — только
        // на корректность реализации.
        let owner = Owner::from_token("test-token");
        assert!(owner.same_token_as("test-token"));
        assert!(!owner.same_token_as("other-token"));
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
