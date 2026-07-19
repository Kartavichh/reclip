//! Пикер — окно выбора записи по `Super+V` (docs/04-picker.md).
//!
//! Запускается как `reclip show` (GNOME вешает на `Super+V`, 4.1). Пикер только
//! ЧИТАЕТ базу (docs/05, 5.1): показывает список превью, по выбору кладёт запись
//! в системный буфер (4.2) и закрывается. Дальше пользователь вставляет обычным
//! `Ctrl+V`. Демон, если запущен, увидит новый буфер и поднимет запись наверх
//! (это и есть «эхо» из 5.1 — самоустраняется дедупом).
//!
//! Особенности GNOME/Wayland: окно нельзя позиционировать самому — композитор
//! сам ставит его по центру (4.3). Буфер пишем через GDK (родной клиент GTK),
//! чтобы GNOME корректно сохранил содержимое после закрытия окна.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Application, ApplicationWindow, Box as GtkBox, EventControllerKey, Label, ListBox, ListBoxRow,
    Orientation, Picture, ScrolledWindow, SelectionMode,
};

use crate::model::{ClipItem, Content};
use crate::storage::{Storage, MAX_ITEMS};

const APP_ID: &str = "io.github.kartavich.reclip";

/// Открыть окно пикера. Читает историю из базы и запускает главный цикл GTK.
pub fn run(storage: Storage) -> Result<()> {
    // Читаем историю ОДИН раз при старте (пикер базу не пишет, 5.1).
    let items: Rc<Vec<ClipItem>> = Rc::new(storage.list(MAX_ITEMS)?);

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(move |app| build_ui(app, items.clone()));

    // Пустой argv: GTK не должен разбирать наши под-команды (их уже разобрал clap).
    let no_args: [&str; 0] = [];
    app.run_with_args(&no_args);
    Ok(())
}

/// Построить окно и всю его логику.
fn build_ui(app: &Application, items: Rc<Vec<ClipItem>>) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("reclip")
        .default_width(600)
        .default_height(420)
        .resizable(false)
        .build();

    // Строка, которой отдадим клавиатурный фокус после показа окна (чтобы ↑/↓
    // работали сразу, без предварительного нажатия других клавиш).
    let mut initial_focus: Option<ListBoxRow> = None;

    if items.is_empty() {
        // Пустая история — просто сообщение (8.4), закрыть можно по Esc.
        let label = Label::builder()
            .label("История пуста")
            .margin_top(40)
            .margin_bottom(40)
            .margin_start(40)
            .margin_end(40)
            .build();
        window.set_child(Some(&label));
    } else {
        let list = ListBox::new();
        list.set_selection_mode(SelectionMode::Single);

        for item in items.iter() {
            let row = ListBoxRow::new();
            let child = row_child(item);
            row.set_child(Some(&child));
            list.append(&row);
        }

        // Выбор строки мышью (двойной клик) или Enter на ней — кладём в буфер.
        {
            let items = items.clone();
            let window = window.clone();
            list.connect_row_activated(move |_list, row| {
                let idx = row.index();
                if idx >= 0 {
                    commit_selection(&window, &items, idx as usize);
                }
            });
        }

        let scrolled = ScrolledWindow::builder().child(&list).build();
        window.set_child(Some(&scrolled));

        // Выделяем первую строку и запоминаем её как цель клавиатурного фокуса —
        // тогда стрелки ↑/↓ и Enter работают штатными средствами GTK сразу (4.4).
        if let Some(first) = list.row_at_index(0) {
            list.select_row(Some(&first));
            initial_focus = Some(first);
        }
    }

    // Клавиатура: Esc — закрыть; цифры 1–9 — выбрать запись по номеру (4.4).
    // Стрелки и Enter обрабатывает сам ListBox (мы их пропускаем дальше).
    let key = EventControllerKey::new();
    {
        let items = items.clone();
        let window = window.clone();
        key.connect_key_pressed(move |_ctrl, keyval, _code, _state| {
            if keyval == gdk::Key::Escape {
                window.close();
                return glib::Propagation::Stop;
            }
            if let Some(ch) = keyval.to_unicode() {
                if ('1'..='9').contains(&ch) {
                    let idx = ch as usize - '1' as usize;
                    if idx < items.len() {
                        commit_selection(&window, &items, idx);
                    }
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
    }
    window.add_controller(key);

    // Закрытие при потере фокуса (4.3). Ждём, пока окно СНАЧАЛА станет активным,
    // иначе можно закрыться на старте до появления.
    {
        let seen_active = Rc::new(Cell::new(false));
        window.connect_is_active_notify(move |w| {
            if w.is_active() {
                seen_active.set(true);
            } else if seen_active.get() {
                w.close();
            }
        });
    }

    window.present();

    // Ставим клавиатурный фокус на первую строку уже после показа окна, чтобы
    // стрелки ↑/↓ работали без предварительного нажатия других клавиш.
    if let Some(row) = initial_focus {
        gtk4::prelude::GtkWindowExt::set_focus(&window, Some(&row));
    }
}

/// Положить выбранную запись в системный буфер и закрыть окно.
///
/// Буфер пишем через GDK (родной клиент GTK — GNOME сохранит содержимое после
/// выхода): текст через `set_text`, картинку — текстурой через `set_texture`
/// (9.6). Закрываем с небольшой задержкой, чтобы композитор успел принять
/// владение буфером до завершения процесса.
fn commit_selection(window: &ApplicationWindow, items: &[ClipItem], idx: usize) {
    let Some(item) = items.get(idx) else {
        return;
    };
    let clipboard = window.clipboard();
    match &item.content {
        Content::Text(text) => clipboard.set_text(text),
        Content::Image { png, .. } => {
            match gdk::Texture::from_bytes(&glib::Bytes::from(&png[..])) {
                Ok(texture) => clipboard.set_texture(&texture),
                Err(e) => {
                    // Не должно случаться (PNG мы сами и записали), но на всякий
                    // случай не роняем окно — пусть пользователь выберет другое.
                    log::warn!("не удалось подготовить картинку для буфера: {e}");
                    return;
                }
            }
        }
    }

    let w = window.clone();
    glib::timeout_add_local_once(Duration::from_millis(120), move || w.close());
}

/// Виджет содержимого строки списка: для текста — подпись-превью, для картинки —
/// миниатюра плюс подпись «🖼 Ш×В». Если PNG почему-то не декодировался —
/// откатываемся на одну подпись, строка всё равно выбирается.
fn row_child(item: &ClipItem) -> gtk4::Widget {
    let caption = item.content.preview(80);
    match &item.content {
        Content::Text(_) => preview_label(&caption).upcast(),
        Content::Image { png, width, height } => {
            match gdk::Texture::from_bytes(&glib::Bytes::from(&png[..])) {
                Ok(texture) => image_row(&texture, *width, *height, &caption),
                Err(_) => preview_label(&caption).upcast(),
            }
        }
    }
}

/// Подпись-строка с типовыми отступами (текстовая запись или откат).
fn preview_label(text: &str) -> Label {
    Label::builder()
        .label(text)
        .xalign(0.0) // текст по левому краю
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(10)
        .margin_end(10)
        .build()
}

/// Строка-картинка: миниатюра фиксированной высоты (пропорции сохраняются) и
/// подпись справа.
fn image_row(texture: &gdk::Texture, width: u32, height: u32, caption: &str) -> gtk4::Widget {
    const THUMB_H: i32 = 80;
    let thumb_w = if height > 0 {
        ((width as f64) * f64::from(THUMB_H) / f64::from(height)).round() as i32
    } else {
        THUMB_H
    };
    let thumb_w = thumb_w.clamp(16, 480);

    let picture = Picture::for_paintable(texture);
    picture.set_can_shrink(true);
    picture.set_size_request(thumb_w, THUMB_H);

    let label = Label::builder().label(caption).xalign(0.0).build();

    let hbox = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(10)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(10)
        .margin_end(10)
        .build();
    hbox.append(&picture);
    hbox.append(&label);
    hbox.upcast()
}
