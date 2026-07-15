//! Этап И0 — проба риска: умеет ли `arboard` работать с картинками на этом
//! GNOME/Wayland (см. docs/09-images.md, 9.0). Это ВРЕМЕННЫЙ пробник, не часть
//! продукта — после снятия риска его можно удалить.
//!
//! Режимы:
//!   cargo run --example image_probe roundtrip
//!       Сгенерировать картинку, положить в буфер, тут же прочитать и сверить
//!       байты. Проверяет чтение+запись одним махом, без участия человека.
//!   cargo run --example image_probe read
//!       Прочитать картинку, которая СЕЙЧАС в буфере (сначала скопируй скриншот),
//!       напечатать размеры. Проверяет чтение того, что положило другое приложение.
//!   cargo run --example image_probe write
//!       Положить в буфер сгенерированную картинку и подождать 30 c — вставь её
//!       (Ctrl+V) в GIMP/браузер/редактор. Проверяет запись для реальных приложений.

use std::borrow::Cow;
use std::time::Duration;

use arboard::{Clipboard, ImageData};

/// Сгенерировать простую RGBA-картинку W×H с узнаваемым узором (градиент +
/// диагональ), чтобы её было легко опознать глазом при вставке.
fn make_image(width: usize, height: usize) -> (Vec<u8>, usize, usize) {
    let mut bytes = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let r = (x * 255 / width.max(1)) as u8;
            let g = (y * 255 / height.max(1)) as u8;
            let b = if x == y { 255 } else { 40 }; // яркая диагональ
            bytes.extend_from_slice(&[r, g, b, 255]); // A = 255 (непрозрачно)
        }
    }
    (bytes, width, height)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "roundtrip".to_string());
    let mut cb = Clipboard::new()?;

    match mode.as_str() {
        "roundtrip" => {
            let (bytes, w, h) = make_image(64, 48);
            println!("Кладу в буфер картинку {w}×{h} ({} байт RGBA)…", bytes.len());
            cb.set_image(ImageData {
                width: w,
                height: h,
                bytes: Cow::Owned(bytes.clone()),
            })?;

            let got = cb.get_image()?;
            println!("Прочитал обратно: {}×{} ({} байт)", got.width, got.height, got.bytes.len());

            let same_dims = got.width == w && got.height == h;
            let same_bytes = got.bytes.as_ref() == bytes.as_slice();
            if same_dims && same_bytes {
                println!("✅ ЧТЕНИЕ+ЗАПИСЬ работают: размеры и байты совпали точь-в-точь.");
            } else if same_dims {
                println!("🟨 Размеры совпали, но байты отличаются (возможно перекодировка компоновщиком). Читать/писать всё равно можно.");
            } else {
                println!("❌ Не совпало: ожидал {w}×{h}, получил {}×{}.", got.width, got.height);
            }
        }
        "read" => {
            println!("Читаю картинку из буфера (скопируй скриншот ДО запуска)…");
            match cb.get_image() {
                Ok(img) => println!(
                    "✅ Прочитано: {}×{} ({} байт RGBA)",
                    img.width, img.height, img.bytes.len()
                ),
                Err(arboard::Error::ContentNotAvailable) => {
                    println!("❌ В буфере нет картинки (или это не картинка). Скопируй скриншот и повтори.")
                }
                Err(e) => println!("❌ Ошибка чтения: {e}"),
            }
        }
        "write" => {
            let (bytes, w, h) = make_image(320, 200);
            cb.set_image(ImageData {
                width: w,
                height: h,
                bytes: Cow::Owned(bytes),
            })?;
            println!("Положил в буфер картинку {w}×{h} (градиент с яркой диагональю).");
            println!("Вставь её (Ctrl+V) в GIMP / браузер / редактор в течение 30 секунд.");
            println!("(Держу владение буфером, пока жив этот процесс.)");
            std::thread::sleep(Duration::from_secs(30));
            println!("Время вышло, выхожу.");
        }
        other => {
            eprintln!("Неизвестный режим: {other}. Используй roundtrip | read | write.");
            std::process::exit(2);
        }
    }
    Ok(())
}
