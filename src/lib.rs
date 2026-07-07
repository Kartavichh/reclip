//! reclip — менеджер истории буфера обмена для Linux (GNOME/Wayland).
//!
//! Библиотека собирает модули проекта (раскладка — docs/07-code-structure.md).
//! Бинарь `reclip` (см. `main.rs`) разбирает под-команду и вызывает нужный модуль.
//!
//! Модули появляются по мере прохождения этапов из `TODO.md`:
//!   model    — Этап 1 (описание записи истории)
//!   storage  — Этап 1 (SQLite-хранилище)
//!   clipboard — Этап 2
//!   daemon    — Этап 3
//!   picker    — Этап 5

pub mod clipboard;
pub mod daemon;
pub mod model;
pub mod picker;
pub mod storage;
