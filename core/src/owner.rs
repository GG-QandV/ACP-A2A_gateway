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
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;

/// Один seed на весь процесс — иначе from_token("t-1") дважды подряд
/// давал бы разные хеши, и проверка "тот же клиент" всегда бы падала.
/// RandomState вместо DefaultHasher: случайный ключ на каждый старт
/// процесса (Р-23 / TECH_DEBT "хеш токена", частичное закрытие).
static OWNER_HASH_SEED: std::sync::LazyLock<RandomState> =
    std::sync::LazyLock::new(RandomState::new);

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
        // ИСПРАВЛЕНО (TECH_DEBT: хеш токена): RandomState вместо
        // DefaultHasher — тот же нижнеуровневый алгоритм (SipHash), но
        // со случайным ключом на каждый старт процесса. Не заменяет
        // полноценный HMAC (см. TECH_DEBT: "заменить на HMAC при
        // усилении модели угроз" — остаётся будущей работой), но
        // устраняет предвычисляемость коллизий между рестартами без
        // единой новой зависимости и без изменения формата Owner::Token.
        let hash = (*OWNER_HASH_SEED).hash_one(token);
        Owner::Token { hash }
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

    /// Хеш одного и того же токена меняется МЕЖДУ перезапусками процесса
    /// (RandomState, не DefaultHasher) — внутри одного запуска (как в этом
    /// тесте) он стабилен, что и требуется для сравнения владельцев.
    /// Смена между процессами не проверяется юнит-тестом (нужен отдельный
    /// процесс), но задокументирована как ожидаемое поведение.
    #[test]
    fn hash_is_stable_within_process_lifetime() {
        let a = Owner::from_token("t-1");
        let b = Owner::from_token("t-1");
        assert_eq!(
            a, b,
            "внутри одного процесса хеш одного токена должен быть стабилен"
        );
    }
}
