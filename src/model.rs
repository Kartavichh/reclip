//! Модель данных — что именно мы храним (docs/01-model.md).

/// Содержимое одной записи буфера обмена.
///
/// В MVP умеем только текст (1.1). Тип оформлен как `enum` заранее, чтобы
/// позже добавить картинки (`Image(...)`) без перестройки остального кода:
/// компилятор сам подсветит все места, где надо будет дописать новую ветку.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// Текстовая запись.
    Text(String),
    // Image(...) — появится, когда возьёмся за поддержку картинок.
}

impl Content {
    /// Короткая строка-превью для показа в списке (пикер/CLI).
    ///
    /// Обрезает по количеству символов (не байт, чтобы не порвать UTF-8) и
    /// схлопывает переводы строк в пробелы — одна запись = одна строка списка.
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
    /// Удобный доступ к тексту (в MVP содержимое всегда текстовое).
    pub fn text(&self) -> Option<&str> {
        match &self.content {
            Content::Text(t) => Some(t),
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
}
