//! gatewayd/src/lib.rs
//! Библиотечная часть бинаря gatewayd. Модули вынесены сюда, чтобы
//! интеграционные тесты (gatewayd/tests/) могли собирать Router и
//! Registry без запуска полного процесса: lib не зависит от main() и
//! от чтения конфига, поэтому тест-харнес подключает свой Registry.

pub mod registry;
pub mod transport_a2a_passthrough;
pub mod transport_http;
pub mod transport_tcp;
