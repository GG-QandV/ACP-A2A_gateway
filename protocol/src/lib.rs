//! protocol/src/lib.rs
//! Типы и (de)serialize для ACP и A2A. Не знает о Reply<T>, о стриминге,
//! о конвертации — только протокольные структуры "как в каноне".

pub mod acp;
pub mod a2a;

pub use acp::*;
pub use a2a::*;
