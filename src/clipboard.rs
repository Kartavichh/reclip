//! Доступ к системному буферу обмена (docs/03-daemon.md 3.4, docs/07 7.2).
//!
//! Весь остальной код работает с буфером только через trait [`Clipboard`],
//! не зная про конкретную библиотеку. В MVP единственная реализация —
//! [`ArboardClipboard`] поверх крейта `arboard` (проверен на GNOME/Wayland,
//! Этап 0). Если `arboard` где-то подведёт, план Б — другая реализация того
//! же trait (`wl-clipboard-rs`/`xclip`), остальной код не меняется.

use std::borrow::Cow;

use anyhow::{Context, Result};

/// Картинка из буфера: сырой **RGBA** (4 байта на пиксель) и размеры в пикселях.
/// Именно в таком виде картинка приходит из буфера и уходит обратно; кодирование
/// в PNG для хранения делает `storage` (docs/09-images.md, 9.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// «Переходник» к системному буферу обмена.
/// Прячет различия X11 / Wayland / Windows за одним интерфейсом.
pub trait Clipboard {
    /// Прочитать текущий текст из буфера.
    /// `Ok(None)` — в буфере нет текста (пусто или там не-текст, например картинка).
    fn get_text(&mut self) -> Result<Option<String>>;

    /// Положить текст в буфер.
    fn set_text(&mut self, text: &str) -> Result<()>;

    /// Прочитать текущую картинку из буфера (сырой RGBA).
    /// `Ok(None)` — в буфере нет картинки (пусто или там текст).
    fn get_image(&mut self) -> Result<Option<ClipImage>>;

    /// Положить картинку в буфер.
    fn set_image(&mut self, image: &ClipImage) -> Result<()>;
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

    fn get_image(&mut self) -> Result<Option<ClipImage>> {
        match self.inner.get_image() {
            Ok(img) => Ok(Some(ClipImage {
                width: img.width as u32,
                height: img.height as u32,
                rgba: img.bytes.into_owned(),
            })),
            // Пустой буфер / там текст — не ошибка, просто «картинки нет».
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => {
                Err(anyhow::Error::new(e).context("не удалось прочитать картинку из буфера"))
            }
        }
    }

    fn set_image(&mut self, image: &ClipImage) -> Result<()> {
        self.inner
            .set_image(arboard::ImageData {
                width: image.width as usize,
                height: image.height as usize,
                bytes: Cow::Borrowed(&image.rgba),
            })
            .context("не удалось записать картинку в буфер")
    }
}

/// Заглушка буфера в памяти — для тестов логики без живого дисплея.
/// Доступна и другим модулям (например, тестам демона на Этапе 3).
#[cfg(test)]
#[derive(Default)]
pub struct MockClipboard {
    text: Option<String>,
    image: Option<ClipImage>,
}

#[cfg(test)]
impl MockClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Задать «текущее содержимое буфера» текстом (имитация внешнего копирования).
    /// Как в реальном буфере, новое содержимое вытесняет прежнюю картинку.
    pub fn put(&mut self, text: &str) {
        self.text = Some(text.to_string());
        self.image = None;
    }

    /// То же для картинки — вытесняет прежний текст.
    pub fn put_image(&mut self, image: ClipImage) {
        self.image = Some(image);
        self.text = None;
    }
}

#[cfg(test)]
impl Clipboard for MockClipboard {
    fn get_text(&mut self) -> Result<Option<String>> {
        Ok(self.text.clone())
    }

    fn set_text(&mut self, text: &str) -> Result<()> {
        self.text = Some(text.to_string());
        self.image = None;
        Ok(())
    }

    fn get_image(&mut self) -> Result<Option<ClipImage>> {
        Ok(self.image.clone())
    }

    fn set_image(&mut self, image: &ClipImage) -> Result<()> {
        self.image = Some(image.clone());
        self.text = None;
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

    fn sample_image() -> ClipImage {
        ClipImage {
            width: 2,
            height: 2,
            rgba: vec![
                255, 0, 0, 255, 0, 255, 0, 255, // красный, зелёный
                0, 0, 255, 255, 255, 255, 255, 255, // синий, белый
            ],
        }
    }

    #[test]
    fn mock_image_roundtrip_set_then_get() {
        let mut cb = MockClipboard::new();
        assert_eq!(cb.get_image().unwrap(), None);
        cb.set_image(&sample_image()).unwrap();
        assert_eq!(cb.get_image().unwrap(), Some(sample_image()));
    }

    #[test]
    fn text_and_image_are_mutually_exclusive_in_mock() {
        let mut cb = MockClipboard::new();
        // Положили картинку — текста нет.
        cb.put_image(sample_image());
        assert_eq!(cb.get_text().unwrap(), None);
        assert!(cb.get_image().unwrap().is_some());
        // Положили текст — картинка вытеснена.
        cb.put("привет");
        assert_eq!(cb.get_text().unwrap(), Some("привет".to_string()));
        assert_eq!(cb.get_image().unwrap(), None);
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

    /// Живой roundtrip картинки через arboard — вручную: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn arboard_image_roundtrip_real_clipboard() {
        let img = sample_image();
        let mut cb = ArboardClipboard::new().unwrap();
        cb.set_image(&img).unwrap();
        let got = cb.get_image().unwrap().expect("картинка должна прочитаться");
        assert_eq!((got.width, got.height), (img.width, img.height));
        assert_eq!(got.rgba, img.rgba);
    }
}
