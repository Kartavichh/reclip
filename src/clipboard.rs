//! Доступ к системному буферу обмена (docs/03-daemon.md 3.4, docs/07 7.2).
//!
//! Весь остальной код работает с буфером только через trait [`Clipboard`],
//! не зная про конкретную библиотеку. В MVP единственная реализация —
//! [`ArboardClipboard`] поверх крейта `arboard` (проверен на GNOME/Wayland,
//! Этап 0). Если `arboard` где-то подведёт, план Б — другая реализация того
//! же trait (`wl-clipboard-rs`/`xclip`), остальной код не меняется.

use anyhow::{Context, Result};

/// «Переходник» к системному буферу обмена.
/// Прячет различия X11 / Wayland / Windows за одним интерфейсом.
pub trait Clipboard {
    /// Прочитать текущий текст из буфера.
    /// `Ok(None)` — в буфере нет текста (пусто или там не-текст, например картинка).
    fn get_text(&mut self) -> Result<Option<String>>;

    /// Положить текст в буфер.
    fn set_text(&mut self, text: &str) -> Result<()>;
}

/// Реализация буфера на крейте `arboard`.
pub struct ArboardClipboard {
    inner: arboard::Clipboard,
}

impl ArboardClipboard {
    /// Подключиться к системному буферу обмена.
    ///
    /// Соединение держим открытым на всё время жизни: на X11 `arboard` обслуживает
    /// вставку из фонового потока, пока владелец буфера жив, — держать один
    /// экземпляр в демоне как раз то, что нужно.
    pub fn new() -> Result<Self> {
        let inner =
            arboard::Clipboard::new().context("не удалось подключиться к буферу обмена")?;
        Ok(Self { inner })
    }
}

impl Clipboard for ArboardClipboard {
    fn get_text(&mut self) -> Result<Option<String>> {
        match self.inner.get_text() {
            Ok(text) => Ok(Some(text)),
            // Пустой буфер / не-текст — это не ошибка, просто «читать нечего».
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => Err(anyhow::Error::new(e).context("не удалось прочитать текст из буфера")),
        }
    }

    fn set_text(&mut self, text: &str) -> Result<()> {
        self.inner
            .set_text(text)
            .context("не удалось записать текст в буфер")
    }
}

/// Заглушка буфера в памяти — для тестов логики без живого дисплея.
/// Доступна и другим модулям (например, тестам демона на Этапе 3).
#[cfg(test)]
#[derive(Default)]
pub struct MockClipboard {
    text: Option<String>,
}

#[cfg(test)]
impl MockClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Задать «текущее содержимое буфера» напрямую (имитация внешнего копирования).
    pub fn put(&mut self, text: &str) {
        self.text = Some(text.to_string());
    }
}

#[cfg(test)]
impl Clipboard for MockClipboard {
    fn get_text(&mut self) -> Result<Option<String>> {
        Ok(self.text.clone())
    }

    fn set_text(&mut self, text: &str) -> Result<()> {
        self.text = Some(text.to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_roundtrip_set_then_get() {
        let mut cb = MockClipboard::new();
        assert_eq!(cb.get_text().unwrap(), None);
        cb.set_text("привет").unwrap();
        assert_eq!(cb.get_text().unwrap(), Some("привет".to_string()));
    }

    /// Живой буфер обмена — запускать вручную: `cargo test -- --ignored`.
    /// По умолчанию пропускается (в headless-окружении дисплея нет).
    #[test]
    #[ignore]
    fn arboard_roundtrip_real_clipboard() {
        let mut cb = ArboardClipboard::new().unwrap();
        cb.set_text("reclip-проверка-буфера").unwrap();
        assert_eq!(
            cb.get_text().unwrap(),
            Some("reclip-проверка-буфера".to_string())
        );
    }
}
