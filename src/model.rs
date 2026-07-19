//! Модель данных — что именно мы храним (docs/01-model.md).

/// Содержимое одной записи буфера обмена.
///
/// Умеем текст (1.1) и картинки (docs/09-images.md). Картинка хранится уже
/// закодированной в **PNG** (9.1) вместе с размерами — их показываем в подписи
/// и используем при декодировании для вставки/миниатюры.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// Текстовая запись.
    Text(String),
    /// Картинка: PNG-байты + размеры в пикселях (9.1).
    Image { png: Vec<u8>, width: u32, height: u32 },
}

impl Content {
    /// Короткая строка-превью для показа в списке (пикер/CLI).
    ///
    /// Для текста: обрезает по количеству символов (не байт, чтобы не порвать
    /// UTF-8) и схлопывает переводы строк в пробелы — одна запись = одна строка.
    /// Для картинки: подпись вида «🖼 Ш×В» (9.5).
    pub fn preview(&self, max_chars: usize) -> String {
        match self {
            Content::Text(text) => {
                let one_line: String = text
                    .chars()
                    .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
                    .collect();
                let trimmed = one_line.trim();
                let mut preview: String = trimmed.chars().take(max_chars).collect();
                if trimmed.chars().count() > max_chars {
                    preview.push('…');
                }
                preview
            }
            Content::Image { width, height, .. } => format!("🖼 {width}×{height}"),
        }
    }
}

/// Одна запись истории буфера обмена.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipItem {
    /// Идентификатор записи в базе (растёт монотонно; больше id — новее запись).
    pub id: i64,
    /// Само содержимое.
    pub content: Content,
    /// Момент последнего появления записи в буфере — Unix-время в миллисекундах.
    /// Используется для показа; порядок в списке задаётся по `id` (см. storage).
    pub created_at: i64,
}

impl ClipItem {
    /// Текст записи, если она текстовая (иначе `None`).
    pub fn text(&self) -> Option<&str> {
        match &self.content {
            Content::Text(t) => Some(t),
            Content::Image { .. } => None,
        }
    }

    /// Картинка записи как `(PNG-байты, ширина, высота)`, если она картиночная.
    pub fn image(&self) -> Option<(&[u8], u32, u32)> {
        match &self.content {
            Content::Image { png, width, height } => Some((png, *width, *height)),
            Content::Text(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_shortens_long_text_with_ellipsis() {
        let c = Content::Text("абвгдежз".to_string());
        assert_eq!(c.preview(3), "абв…");
    }

    #[test]
    fn preview_keeps_short_text_as_is() {
        let c = Content::Text("абв".to_string());
        assert_eq!(c.preview(10), "абв");
    }

    #[test]
    fn preview_collapses_newlines_to_spaces() {
        let c = Content::Text("строка1\nстрока2".to_string());
        assert_eq!(c.preview(100), "строка1 строка2");
    }

    #[test]
    fn preview_trims_surrounding_whitespace() {
        let c = Content::Text("  привет  ".to_string());
        assert_eq!(c.preview(100), "привет");
    }

    #[test]
    fn preview_of_image_shows_dimensions() {
        let c = Content::Image { png: vec![], width: 1920, height: 1080 };
        assert_eq!(c.preview(80), "🖼 1920×1080");
    }

    #[test]
    fn image_accessor_returns_data_text_returns_none() {
        let item = ClipItem {
            id: 1,
            content: Content::Image { png: vec![1, 2, 3], width: 4, height: 2 },
            created_at: 0,
        };
        assert_eq!(item.text(), None);
        assert_eq!(item.image(), Some(([1u8, 2, 3].as_slice(), 4, 2)));
    }
}
