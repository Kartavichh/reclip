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
use crate::storage::{rgba_hash, AddOutcome, Storage};

/// Интервал опроса буфера (3.1).
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Что демон видел в буфере в прошлый тик — чтобы не переобрабатывать одно и то
/// же каждые 500 мс (9.7). Текст сравниваем целиком, картинку — по хешу RGBA
/// (сама картинка может весить мегабайты, хранить её копию ни к чему).
#[derive(Debug, PartialEq, Eq)]
enum LastSeen {
    Text(String),
    Image(String),
}

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

    let mut last_seen: Option<LastSeen> = None;
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

/// Одна итерация опроса: прочитать буфер и, если содержимое изменилось, записать
/// в историю. Приоритет — текст (дешевле и чаще); нет текста → пробуем картинку
/// (9.7). `last_seen` хранит, что обработали в прошлый тик, чтобы не писать одно
/// и то же каждые 500 мс. Возвращает результат записи (для логов/тестов) или
/// `None`, если писать было нечего (буфер пуст, не текст/картинка, или не изменился).
///
/// Вынесено отдельно от [`run`], чтобы покрыть логику юнит-тестами без живого
/// буфера и без бесконечного цикла.
fn poll_once(
    clipboard: &mut dyn Clipboard,
    storage: &Storage,
    last_seen: &mut Option<LastSeen>,
) -> Result<Option<AddOutcome>> {
    // 1) Текст. Запоминаем ДО записи: даже если фильтр отбросит запись
    //    (пустое/крупное), не будем переобрабатывать тот же текст на каждом тике.
    if let Some(text) = clipboard.get_text()? {
        if matches!(last_seen.as_ref(), Some(LastSeen::Text(t)) if t == &text) {
            return Ok(None); // тот же текст, что и в прошлый раз
        }
        *last_seen = Some(LastSeen::Text(text.clone()));
        return Ok(Some(storage.add_text(&text)?));
    }

    // 2) Картинка. Сравниваем по хешу RGBA — тем же, что использует storage для
    //    дедупа, чтобы «та же картинка» трактовалась одинаково.
    if let Some(img) = clipboard.get_image()? {
        let fingerprint = rgba_hash(&img.rgba, img.width, img.height);
        if matches!(last_seen.as_ref(), Some(LastSeen::Image(h)) if h == &fingerprint) {
            return Ok(None); // та же картинка, что и в прошлый раз
        }
        *last_seen = Some(LastSeen::Image(fingerprint));
        return Ok(Some(storage.add_image(&img.rgba, img.width, img.height)?));
    }

    Ok(None) // в буфере пусто или не текст и не картинка
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
        assert_eq!(last, Some(LastSeen::Text("привет".to_string())));
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

    // --- Картинки (Этап И3) ---

    use crate::clipboard::ClipImage;

    fn image(fill: u8) -> ClipImage {
        ClipImage { width: 2, height: 2, rgba: vec![fill; 2 * 2 * 4] }
    }

    #[test]
    fn adds_new_image() {
        let storage = Storage::open_in_memory().unwrap();
        let mut cb = MockClipboard::new();
        let mut last = None;

        cb.put_image(image(100));
        let out = poll_once(&mut cb, &storage, &mut last).unwrap();

        assert_eq!(out, Some(AddOutcome::Added));
        assert_eq!(storage.count().unwrap(), 1);
        assert!(storage.list(10).unwrap()[0].image().is_some());
    }

    #[test]
    fn unchanged_image_is_not_added_again() {
        let storage = Storage::open_in_memory().unwrap();
        let mut cb = MockClipboard::new();
        let mut last = None;

        cb.put_image(image(50));
        poll_once(&mut cb, &storage, &mut last).unwrap();
        // Картинка не менялась — второй опрос молчит (не переупаковывает PNG).
        let out = poll_once(&mut cb, &storage, &mut last).unwrap();

        assert_eq!(out, None);
        assert_eq!(storage.count().unwrap(), 1);
    }

    #[test]
    fn switching_between_text_and_image_records_both() {
        let storage = Storage::open_in_memory().unwrap();
        let mut cb = MockClipboard::new();
        let mut last = None;

        cb.put("текст");
        assert_eq!(poll_once(&mut cb, &storage, &mut last).unwrap(), Some(AddOutcome::Added));
        cb.put_image(image(7));
        assert_eq!(poll_once(&mut cb, &storage, &mut last).unwrap(), Some(AddOutcome::Added));
        cb.put("снова текст");
        assert_eq!(poll_once(&mut cb, &storage, &mut last).unwrap(), Some(AddOutcome::Added));

        assert_eq!(storage.count().unwrap(), 3);
        let items = storage.list(10).unwrap();
        assert_eq!(items[0].text(), Some("снова текст"));
        assert!(items[1].image().is_some());
        assert_eq!(items[2].text(), Some("текст"));
    }

    #[test]
    fn reselecting_old_image_bumps_it_to_top() {
        // Эхо (5.1) для картинок: пикер вернул старую картинку в буфер — демон
        // видит изменение и через дедуп-по-хешу поднимает её наверх, без дубля.
        let storage = Storage::open_in_memory().unwrap();
        let mut cb = MockClipboard::new();
        let mut last = None;

        cb.put_image(image(1));
        poll_once(&mut cb, &storage, &mut last).unwrap();
        cb.put_image(image(2));
        poll_once(&mut cb, &storage, &mut last).unwrap();
        // Пикер вернул первую картинку:
        cb.put_image(image(1));
        let out = poll_once(&mut cb, &storage, &mut last).unwrap();

        assert_eq!(out, Some(AddOutcome::Bumped));
        assert_eq!(storage.count().unwrap(), 2); // без дубликата
    }
}
