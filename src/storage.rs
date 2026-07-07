//! Хранилище истории — SQLite (docs/02-storage.md).
//!
//! Единственный писатель — демон; пикер только читает (docs/05). SQLite в
//! режиме WAL сам разруливает одновременный доступ (2.4). Файл лежит в
//! `~/.local/share/reclip/history.db` (2.1) и переживает перезагрузки (2.3).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::model::{ClipItem, Content};

/// Максимум записей в истории; лишние (самые старые) вытесняются (1.6).
pub const MAX_ITEMS: usize = 100;

/// Порог размера одной записи в байтах (UTF-8). Крупнее — не сохраняем (1.5).
pub const MAX_TEXT_BYTES: usize = 1024 * 1024; // ~1 МБ

/// Текущая версия схемы (пишется в `PRAGMA user_version`, 2.5).
const SCHEMA_VERSION: i64 = 1;

/// Что произошло при попытке добавить запись — удобно для тестов и логов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    /// Новая запись добавлена.
    Added,
    /// Такой текст уже был — подняли наверх (дедуп, 1.3).
    Bumped,
    /// Запись отброшена фильтром (пустое/пробелы — 1.4, или слишком крупное — 1.5).
    Ignored,
}

/// Хранилище истории поверх одного SQLite-соединения.
pub struct Storage {
    conn: Connection,
}

impl Storage {
    /// Открыть (при необходимости — создать) базу по указанному пути.
    /// Создаёт родительские каталоги, включает WAL и применяет миграции.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("не удалось создать каталог {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("не удалось открыть базу {}", path.display()))?;
        // WAL — параллельное чтение пикером во время записи демоном (2.4).
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // Если база занята другим процессом — подождать, а не падать сразу.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let storage = Self { conn };
        storage.migrate()?;
        Ok(storage)
    }

    /// База в оперативной памяти — только для тестов.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let storage = Self { conn };
        storage.migrate()?;
        Ok(storage)
    }

    /// Применить миграции схемы, ориентируясь на `PRAGMA user_version` (2.5).
    fn migrate(&self) -> Result<()> {
        let version: i64 =
            self.conn
                .pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version < 1 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS items (
                     id         INTEGER PRIMARY KEY AUTOINCREMENT,
                     kind       TEXT    NOT NULL DEFAULT 'text',
                     text       TEXT    NOT NULL,
                     created_at INTEGER NOT NULL
                 );",
            )?;
        }
        // Будущие версии: `if version < 2 { ... }` и т.д.
        self.conn
            .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    /// Добавить текст в историю с учётом фильтров и дедупа.
    ///
    /// - пустое/из одних пробелов — игнор (1.4);
    /// - крупнее порога — игнор (1.5);
    /// - уже есть такой текст — поднимаем наверх (1.3) через «удалить+вставить»,
    ///   чтобы запись получила свежий `id` и оказалась первой в списке;
    /// - после вставки обрезаем историю до `MAX_ITEMS` (1.6).
    pub fn add_text(&self, text: &str) -> Result<AddOutcome> {
        if text.trim().is_empty() {
            return Ok(AddOutcome::Ignored);
        }
        if text.len() > MAX_TEXT_BYTES {
            return Ok(AddOutcome::Ignored);
        }

        // Дедуп: убираем прежнюю такую же запись (если была), затем вставляем
        // заново — новый id поставит её наверх.
        let removed = self
            .conn
            .execute("DELETE FROM items WHERE text = ?1", params![text])?;
        self.conn.execute(
            "INSERT INTO items (kind, text, created_at) VALUES ('text', ?1, ?2)",
            params![text, now_millis()],
        )?;

        self.trim()?;

        Ok(if removed > 0 {
            AddOutcome::Bumped
        } else {
            AddOutcome::Added
        })
    }

    /// Оставить только `MAX_ITEMS` самых новых записей, остальные удалить (1.6).
    fn trim(&self) -> Result<()> {
        self.conn.execute(
            "DELETE FROM items
             WHERE id NOT IN (
                 SELECT id FROM items ORDER BY id DESC LIMIT ?1
             )",
            params![MAX_ITEMS as i64],
        )?;
        Ok(())
    }

    /// Вернуть историю от самой новой записи к старой, не более `limit` штук.
    pub fn list(&self, limit: usize) -> Result<Vec<ClipItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, text, created_at FROM items ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(ClipItem {
                id: row.get(0)?,
                content: Content::Text(row.get(1)?),
                created_at: row.get(2)?,
            })
        })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    /// Сколько записей сейчас в истории.
    pub fn count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))?;
        Ok(n as usize)
    }
}

/// Путь к базе по умолчанию: `~/.local/share/reclip/history.db` (2.1).
pub fn default_db_path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "reclip")
        .context("не удалось определить домашние каталоги пользователя")?;
    Ok(dirs.data_local_dir().join("history.db"))
}

/// Текущее время как Unix-миллисекунды.
fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(storage: &Storage) -> Vec<String> {
        storage
            .list(MAX_ITEMS)
            .unwrap()
            .into_iter()
            .map(|item| item.text().unwrap().to_string())
            .collect()
    }

    #[test]
    fn add_then_list_returns_item() {
        let s = Storage::open_in_memory().unwrap();
        assert_eq!(s.add_text("привет").unwrap(), AddOutcome::Added);
        assert_eq!(texts(&s), vec!["привет"]);
    }

    #[test]
    fn newest_item_is_first() {
        let s = Storage::open_in_memory().unwrap();
        s.add_text("первый").unwrap();
        s.add_text("второй").unwrap();
        s.add_text("третий").unwrap();
        assert_eq!(texts(&s), vec!["третий", "второй", "первый"]);
    }

    #[test]
    fn duplicate_is_bumped_to_top_not_duplicated() {
        let s = Storage::open_in_memory().unwrap();
        s.add_text("a").unwrap();
        s.add_text("b").unwrap();
        assert_eq!(s.add_text("a").unwrap(), AddOutcome::Bumped);
        // "a" поднялась наверх, дубликата нет.
        assert_eq!(texts(&s), vec!["a", "b"]);
        assert_eq!(s.count().unwrap(), 2);
    }

    #[test]
    fn empty_or_whitespace_is_ignored() {
        let s = Storage::open_in_memory().unwrap();
        assert_eq!(s.add_text("").unwrap(), AddOutcome::Ignored);
        assert_eq!(s.add_text("   \n\t ").unwrap(), AddOutcome::Ignored);
        assert_eq!(s.count().unwrap(), 0);
    }

    #[test]
    fn oversized_text_is_ignored() {
        let s = Storage::open_in_memory().unwrap();
        let big = "x".repeat(MAX_TEXT_BYTES + 1);
        assert_eq!(s.add_text(&big).unwrap(), AddOutcome::Ignored);
        assert_eq!(s.count().unwrap(), 0);
    }

    #[test]
    fn history_is_trimmed_to_max_items() {
        let s = Storage::open_in_memory().unwrap();
        for i in 0..(MAX_ITEMS + 10) {
            s.add_text(&format!("item-{i}")).unwrap();
        }
        assert_eq!(s.count().unwrap(), MAX_ITEMS);
        // Самая новая — последняя добавленная; самые старые вытеснены.
        let all = texts(&s);
        assert_eq!(all.first().unwrap(), &format!("item-{}", MAX_ITEMS + 9));
        assert!(!all.contains(&"item-0".to_string()));
    }

    #[test]
    fn schema_version_is_set() {
        let s = Storage::open_in_memory().unwrap();
        let v: i64 = s
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }
}
