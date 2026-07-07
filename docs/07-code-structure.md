# 07. Структура кода

Решения по группе 7 из `TODO.md`.

## 7.1 Раскладка проекта — ✅ один crate, бинарь с под-командами
Один crate (не workspace — для нашего размера это переусложнение).

```
reclip/
├── Cargo.toml
└── src/
    ├── main.rs        # точка входа: clap разбирает под-команду и вызывает модуль
    ├── lib.rs         # собирает модули ниже
    ├── model.rs       # ClipItem, enum типа содержимого
    ├── storage.rs     # SQLite: открыть, добавить, поднять наверх, список, обрезка
    ├── clipboard.rs   # trait Clipboard + реализация на arboard
    ├── daemon.rs      # цикл поллинга
    └── picker.rs      # окно GTK4
```

Под-команды: `reclip daemon`, `reclip show`, `reclip list`.
Каждый модуль отвечает за одно — легко тестировать по кускам.

## 7.2 Trait-абстракция буфера — ✅ `Clipboard` (get_text/set_text)
Минимальная форма под текст в MVP, с прицелом на будущее:

```rust
/// «Переходник» к системному буферу обмена.
/// Прячет различия X11 / Wayland / Windows за одним интерфейсом.
pub trait Clipboard {
    /// Прочитать текущий текст из буфера (None — если там не текст/пусто).
    fn get_text(&mut self) -> anyhow::Result<Option<String>>;
    /// Положить текст в буфер.
    fn set_text(&mut self, text: &str) -> anyhow::Result<()>;
}
```

- MVP: одна реализация `ArboardClipboard` (обёртка над `arboard`).
- Будущее (картинки): добавим `get_image`/`set_image` или обобщим до
  `get_content`/`set_content` на базе enum из `model.rs`. Компилятор подсветит
  все места, где надо дописать ветку.
- План Б (3.4): если `arboard` подведёт на GNOME — пишем другую реализацию
  того же trait, остальной код не трогаем.
