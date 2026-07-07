//! Демон — фоновое слежение за буфером обмена (docs/03-daemon.md).
//!
//! GNOME/Wayland не присылает событий об изменении буфера, поэтому мы **опрашиваем**
//! его каждые 500 мс (3.1). Демон — единственный, кто пишет в историю (docs/05,
//! 5.1); пикер только читает. Запускается один экземпляр — гарантируется файловой
//! блокировкой `flock` на `$XDG_RUNTIME_DIR/reclip.lock` (3.3).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use fs2::FileExt;
use log::{debug, info, warn};

use crate::clipboard::{ArboardClipboard, Clipboard};
use crate::storage::{AddOutcome, Storage};

/// Интервал опроса буфера (3.1).
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Запустить демон: захватить блокировку и крутить цикл опроса до завершения
/// процесса. Возвращает ошибку, если демон уже запущен или не удалось
/// подключиться к буферу.
pub fn run(storage: Storage) -> Result<()> {
    // Держим файл-блокировку живым всё время работы демона: как только процесс
    // завершится (fd закроется), блокировка снимется автоматически.
    let _lock = acquire_lock(&lock_path()?)?;

    let mut clipboard = ArboardClipboard::new()?;
    info!(
        "демон запущен, опрос буфера каждые {} мс",
        POLL_INTERVAL.as_millis()
    );

    let mut last_seen: Option<String> = None;
    loop {
        match poll_once(&mut clipboard, &storage, &mut last_seen) {
            Ok(Some(AddOutcome::Added)) => debug!("новая запись добавлена в историю"),
            Ok(Some(AddOutcome::Bumped)) => debug!("существующая запись поднята наверх"),
            Ok(Some(AddOutcome::Ignored)) => debug!("буфер изменился, но запись отброшена фильтром"),
            Ok(None) => {} // буфер пуст или не изменился — тихо
            // Разовая ошибка чтения буфера не должна ронять демон — логируем и живём дальше.
            Err(e) => warn!("ошибка опроса буфера: {e:#}"),
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Одна итерация опроса: прочитать буфер и, если текст изменился, записать в
/// историю. `last_seen` хранит последний обработанный текст, чтобы не писать одно
/// и то же каждые 500 мс. Возвращает результат записи (для логов/тестов) или
/// `None`, если писать было нечего (буфер пуст/не-текст или не изменился).
///
/// Вынесено отдельно от [`run`], чтобы покрыть логику юнит-тестами без живого
/// буфера и без бесконечного цикла.
fn poll_once(
    clipboard: &mut dyn Clipboard,
    storage: &Storage,
    last_seen: &mut Option<String>,
) -> Result<Option<AddOutcome>> {
    let text = match clipboard.get_text()? {
        Some(text) => text,
        None => return Ok(None), // в буфере пусто или не-текст
    };

    // Без изменений с прошлого раза — ничего не делаем.
    if last_seen.as_deref() == Some(text.as_str()) {
        return Ok(None);
    }

    // Запоминаем ДО записи: даже если фильтр отбросит запись (пустое/крупное),
    // не будем повторно обрабатывать тот же текст на каждом тике.
    *last_seen = Some(text.clone());

    let outcome = storage.add_text(&text)?;
    Ok(Some(outcome))
}

/// Путь к lock-файлу: `$XDG_RUNTIME_DIR/reclip.lock` (3.3).
/// Если `$XDG_RUNTIME_DIR` не задан — откатываемся во временный каталог.
pub fn lock_path() -> Result<PathBuf> {
    let dir = directories::BaseDirs::new()
        .and_then(|b| b.runtime_dir().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir);
    Ok(dir.join("reclip.lock"))
}

/// Захватить эксклюзивную блокировку lock-файла. Если её уже держит другой
/// экземпляр демона — вернуть ошибку (второй демон не должен запускаться).
fn acquire_lock(path: &Path) -> Result<File> {
    let file = File::create(path)
        .with_context(|| format!("не удалось открыть lock-файл {}", path.display()))?;
    file.try_lock_exclusive().map_err(|_| {
        anyhow::anyhow!(
            "демон уже запущен (блокировка занята: {})",
            path.display()
        )
    })?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::MockClipboard;

    #[test]
    fn adds_new_text() {
        let storage = Storage::open_in_memory().unwrap();
        let mut cb = MockClipboard::new();
        let mut last = None;

        cb.put("привет");
        let out = poll_once(&mut cb, &storage, &mut last).unwrap();

        assert_eq!(out, Some(AddOutcome::Added));
        assert_eq!(storage.count().unwrap(), 1);
        assert_eq!(last, Some("привет".to_string()));
    }

    #[test]
    fn unchanged_text_is_not_added_again() {
        let storage = Storage::open_in_memory().unwrap();
        let mut cb = MockClipboard::new();
        let mut last = None;

        cb.put("x");
        poll_once(&mut cb, &storage, &mut last).unwrap();
        // Буфер не менялся — второй опрос ничего не пишет.
        let out = poll_once(&mut cb, &storage, &mut last).unwrap();

        assert_eq!(out, None);
        assert_eq!(storage.count().unwrap(), 1);
    }

    #[test]
    fn changed_text_is_added() {
        let storage = Storage::open_in_memory().unwrap();
        let mut cb = MockClipboard::new();
        let mut last = None;

        cb.put("первый");
        poll_once(&mut cb, &storage, &mut last).unwrap();
        cb.put("второй");
        let out = poll_once(&mut cb, &storage, &mut last).unwrap();

        assert_eq!(out, Some(AddOutcome::Added));
        assert_eq!(storage.count().unwrap(), 2);
    }

    #[test]
    fn empty_clipboard_does_nothing() {
        let storage = Storage::open_in_memory().unwrap();
        let mut cb = MockClipboard::new(); // пусто
        let mut last = None;

        let out = poll_once(&mut cb, &storage, &mut last).unwrap();

        assert_eq!(out, None);
        assert_eq!(storage.count().unwrap(), 0);
    }

    #[test]
    fn filtered_text_is_not_reprocessed() {
        let storage = Storage::open_in_memory().unwrap();
        let mut cb = MockClipboard::new();
        let mut last = None;

        cb.put("   \n  "); // одни пробелы → фильтр 1.4
        let first = poll_once(&mut cb, &storage, &mut last).unwrap();
        let second = poll_once(&mut cb, &storage, &mut last).unwrap();

        assert_eq!(first, Some(AddOutcome::Ignored));
        assert_eq!(second, None); // тот же текст повторно не трогаем
        assert_eq!(storage.count().unwrap(), 0);
    }

    #[test]
    fn reselecting_old_item_bumps_it_to_top() {
        // Имитация «эха» (5.1): пикер положил в буфер старую запись — демон видит
        // изменение и через дедуп поднимает её наверх.
        let storage = Storage::open_in_memory().unwrap();
        let mut cb = MockClipboard::new();
        let mut last = None;

        cb.put("a");
        poll_once(&mut cb, &storage, &mut last).unwrap();
        cb.put("b");
        poll_once(&mut cb, &storage, &mut last).unwrap();
        // Пикер вернул "a" в буфер:
        cb.put("a");
        let out = poll_once(&mut cb, &storage, &mut last).unwrap();

        assert_eq!(out, Some(AddOutcome::Bumped));
        let items = storage.list(10).unwrap();
        assert_eq!(items[0].text().unwrap(), "a"); // "a" снова наверху
        assert_eq!(storage.count().unwrap(), 2); // без дубликата
    }
}
