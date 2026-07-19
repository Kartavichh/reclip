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

/// Порог размера закодированного PNG. Крупнее — картинку не сохраняем (9.3).
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024; // ~10 МБ

/// Текущая версия схемы (пишется в `PRAGMA user_version`, 2.5).
/// v1 — только текст; v2 — добавлены картинки (docs/09-images.md, 9.4).
const SCHEMA_VERSION: i64 = 2;

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
            // v1 — только текст (исходная схема MVP).
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS items (
                     id         INTEGER PRIMARY KEY AUTOINCREMENT,
                     kind       TEXT    NOT NULL DEFAULT 'text',
                     text       TEXT    NOT NULL,
                     created_at INTEGER NOT NULL
                 );",
            )?;
        }
        if version < 2 {
            // v2 — добавляем картинки (9.4). У картинок нет текста, поэтому
            // `text` больше не `NOT NULL`; появляются колонки `image/width/
            // height/hash`. SQLite не умеет снимать NOT NULL через ALTER, поэтому
            // пересоздаём таблицу и переносим старые текстовые записи.
            self.conn.execute_batch(
                "ALTER TABLE items RENAME TO items_v1;
                 CREATE TABLE items (
                     id         INTEGER PRIMARY KEY AUTOINCREMENT,
                     kind       TEXT    NOT NULL,
                     text       TEXT,
                     image      BLOB,
                     width      INTEGER,
                     height     INTEGER,
                     hash       TEXT,
                     created_at INTEGER NOT NULL
                 );
                 INSERT INTO items (id, kind, text, created_at)
                     SELECT id, kind, text, created_at FROM items_v1;
                 DROP TABLE items_v1;",
            )?;
        }
        // Будущие версии: `if version < 3 { ... }` и т.д.
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

    /// Добавить картинку в историю (docs/09-images.md).
    ///
    /// На вход — сырой RGBA (4 байта на пиксель) с размерами `width`×`height`.
    /// - хеш считаем по сырому RGBA (9.2) — стабильный ключ дедупа;
    /// - кодируем в PNG (9.1); битые данные или PNG крупнее `MAX_IMAGE_BYTES`
    ///   (9.3) — игнорируем;
    /// - дубликат (тот же хеш) поднимаем наверх «удалить+вставить», как текст;
    /// - после вставки обрезаем историю до `MAX_ITEMS`.
    pub fn add_image(&self, rgba: &[u8], width: u32, height: u32) -> Result<AddOutcome> {
        let hash = rgba_hash(rgba, width, height);

        // Кодируем в PNG. Несовпадение размера буфера с width×height даёт ошибку —
        // трактуем как «мусор в буфере» и молча пропускаем.
        let png = match encode_png(rgba, width, height) {
            Ok(png) => png,
            Err(_) => return Ok(AddOutcome::Ignored),
        };
        if png.len() > MAX_IMAGE_BYTES {
            return Ok(AddOutcome::Ignored);
        }

        // Дедуп по хешу (у текстовых записей hash = NULL, их не заденет).
        let removed = self
            .conn
            .execute("DELETE FROM items WHERE hash = ?1", params![hash])?;
        self.conn.execute(
            "INSERT INTO items (kind, image, width, height, hash, created_at)
             VALUES ('image', ?1, ?2, ?3, ?4, ?5)",
            params![png, width as i64, height as i64, hash, now_millis()],
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
            "SELECT id, kind, text, image, width, height, created_at
             FROM items ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            let kind: String = row.get(1)?;
            let content = if kind == "image" {
                let png: Vec<u8> = row.get(3)?;
                let width: i64 = row.get(4)?;
                let height: i64 = row.get(5)?;
                Content::Image {
                    png,
                    width: width as u32,
                    height: height as u32,
                }
            } else {
                Content::Text(row.get(2)?)
            };
            Ok(ClipItem {
                id: row.get(0)?,
                content,
                created_at: row.get(6)?,
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

/// Хеш картинки для дедупа (9.2). Считаем по сырому RGBA и размерам —
/// стабильно и не зависит от параметров PNG-кодирования. Для истории в пределах
/// сотни записей коллизии `DefaultHasher` практически невозможны.
///
/// `pub(crate)`: демон тем же хешем определяет «та же картинка, что и в прошлый
/// тик» (9.7), чтобы не переобрабатывать её каждые 500 мс.
pub(crate) fn rgba_hash(rgba: &[u8], width: u32, height: u32) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    width.hash(&mut h);
    height.hash(&mut h);
    rgba.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Закодировать сырой RGBA в PNG (9.1). Ошибка, если длина буфера не бьётся с
/// `width`×`height`×4 (тогда вызывающий код трактует это как мусор и пропускает).
fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    use image::codecs::png::PngEncoder;
    use image::{ExtendedColorType, ImageEncoder};

    // Проверяем длину сами: `write_image` при несовпадении паникует, а не
    // возвращает ошибку, — до него дело доводить нельзя.
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .context("слишком большие размеры картинки")?;
    if rgba.len() != expected {
        anyhow::bail!(
            "длина RGBA {} не совпадает с ожидаемой {} для {width}×{height}",
            rgba.len(),
            expected
        );
    }

    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(rgba, width, height, ExtendedColorType::Rgba8)
        .context("не удалось закодировать картинку в PNG")?;
    Ok(png)
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

    // --- Картинки (Этап И1) ---

    /// Однотонная RGBA-картинка width×height (легко сжимается в маленький PNG).
    fn solid_rgba(width: u32, height: u32, fill: u8) -> Vec<u8> {
        vec![fill; (width * height * 4) as usize]
    }

    /// Шумовая RGBA-картинка — плохо сжимается, PNG выходит крупным (для теста
    /// лимита размера). Детерминированный xorshift, чтобы тест был стабилен.
    fn noise_rgba(width: u32, height: u32) -> Vec<u8> {
        let n = (width * height * 4) as usize;
        let mut v = Vec::with_capacity(n);
        let mut state: u32 = 0x1234_5678;
        for _ in 0..n {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            v.push((state & 0xff) as u8);
        }
        v
    }

    #[test]
    fn add_image_then_list_returns_image_as_png() {
        let s = Storage::open_in_memory().unwrap();
        assert_eq!(s.add_image(&solid_rgba(4, 3, 200), 4, 3).unwrap(), AddOutcome::Added);

        let items = s.list(MAX_ITEMS).unwrap();
        assert_eq!(items.len(), 1);
        let (png, w, h) = items[0].image().expect("должна быть картинка");
        assert_eq!((w, h), (4, 3));
        // PNG-сигнатура: 0x89 'P' 'N' 'G'.
        assert_eq!(&png[..4], &[0x89, b'P', b'N', b'G']);
    }

    #[test]
    fn duplicate_image_is_bumped_to_top() {
        let s = Storage::open_in_memory().unwrap();
        let a = solid_rgba(2, 2, 10);
        let b = solid_rgba(2, 2, 20);
        s.add_image(&a, 2, 2).unwrap();
        s.add_image(&b, 2, 2).unwrap();
        // Повторная та же картинка "a" — поднять наверх, без дубля.
        assert_eq!(s.add_image(&a, 2, 2).unwrap(), AddOutcome::Bumped);
        assert_eq!(s.count().unwrap(), 2);

        let items = s.list(MAX_ITEMS).unwrap();
        let (_, w0, h0) = items[0].image().unwrap();
        assert_eq!((w0, h0), (2, 2));
        // Наверху именно "a" (сверяем по её PNG).
        let a_png = encode_png(&a, 2, 2).unwrap();
        assert_eq!(items[0].image().unwrap().0, a_png.as_slice());
    }

    #[test]
    fn oversized_image_is_ignored() {
        let s = Storage::open_in_memory().unwrap();
        // Шум 2048×2048 → PNG заведомо больше 10 МБ.
        let big = noise_rgba(2048, 2048);
        assert_eq!(s.add_image(&big, 2048, 2048).unwrap(), AddOutcome::Ignored);
        assert_eq!(s.count().unwrap(), 0);
    }

    #[test]
    fn malformed_rgba_is_ignored() {
        let s = Storage::open_in_memory().unwrap();
        // Длина буфера не бьётся с 3×3×4 = 36 байт.
        assert_eq!(s.add_image(&[0u8; 10], 3, 3).unwrap(), AddOutcome::Ignored);
        assert_eq!(s.count().unwrap(), 0);
    }

    #[test]
    fn migrates_v1_text_db_to_v2_preserving_rows() {
        // Собираем «старую» базу схемы v1 (только текст) и проверяем, что
        // апгрейд до v2 не теряет записи и разрешает добавлять картинки. Это тот
        // самый путь, по которому пойдёт уже установленная у пользователя база.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE items (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 kind       TEXT    NOT NULL DEFAULT 'text',
                 text       TEXT    NOT NULL,
                 created_at INTEGER NOT NULL
             );
             INSERT INTO items (kind, text, created_at) VALUES ('text', 'старьё', 111);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 1i64).unwrap();

        let s = Storage { conn };
        s.migrate().unwrap();

        let v: i64 = s
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, 2);

        let items = s.list(MAX_ITEMS).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text(), Some("старьё"));

        // В мигрированную схему можно добавить картинку.
        assert_eq!(
            s.add_image(&solid_rgba(2, 2, 1), 2, 2).unwrap(),
            AddOutcome::Added
        );
        assert_eq!(s.count().unwrap(), 2);
    }

    #[test]
    fn text_and_images_coexist_newest_first() {
        let s = Storage::open_in_memory().unwrap();
        s.add_text("привет").unwrap();
        s.add_image(&solid_rgba(2, 2, 5), 2, 2).unwrap();
        s.add_text("пока").unwrap();

        let items = s.list(MAX_ITEMS).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].text(), Some("пока"));
        assert!(items[1].image().is_some());
        assert_eq!(items[2].text(), Some("привет"));
    }
}
