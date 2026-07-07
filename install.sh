#!/usr/bin/env bash
#
# Установка reclip: сборка из исходников, установка бинаря и systemd-сервиса,
# включение автозапуска демона. Подробности — в docs/06-build-install.md.
#
# Запуск:  ./install.sh
#
set -euo pipefail

# --- Пути ------------------------------------------------------------------
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="$HOME/.local/bin"
SERVICE_DIR="$HOME/.config/systemd/user"
BIN_PATH="$BIN_DIR/reclip"

say()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[!]\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31m[ошибка]\033[0m %s\n' "$*" >&2; exit 1; }

# --- 1. Проверка зависимостей сборки (6.2) ---------------------------------
say "Проверяю зависимости сборки…"

command -v cargo >/dev/null 2>&1 || die \
  "не найден cargo (Rust). Поставьте Rust: https://rustup.rs"

command -v cc >/dev/null 2>&1 || command -v gcc >/dev/null 2>&1 || die \
  "не найден C-компилятор. Поставьте: sudo apt install build-essential"

if ! pkg-config --exists gtk4 2>/dev/null; then
  die "не найдена библиотека GTK4 (для сборки GUI).
     Поставьте: sudo apt install libgtk-4-dev build-essential"
fi

say "Зависимости на месте."

# --- 2. Сборка релиза ------------------------------------------------------
say "Собираю релизную версию (cargo build --release)…"
( cd "$REPO_DIR" && cargo build --release )

# --- 3. Установка бинаря ---------------------------------------------------
say "Устанавливаю бинарь в $BIN_PATH"
mkdir -p "$BIN_DIR"
install -m 755 "$REPO_DIR/target/release/reclip" "$BIN_PATH"

# Предупредим, если ~/.local/bin не в PATH (нужно для запуска по имени и ярлыка).
case ":$PATH:" in
  *":$BIN_DIR:"*) : ;;
  *) warn "Каталог $BIN_DIR не в PATH. Обычно он добавляется при следующем входе.
       Если нет — добавьте в ~/.profile:  export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
esac

# --- 4. Установка и запуск systemd-сервиса (3.2) ---------------------------
say "Устанавливаю systemd user-сервис (автозапуск демона)…"
mkdir -p "$SERVICE_DIR"
install -m 644 "$REPO_DIR/dist/reclip.service" "$SERVICE_DIR/reclip.service"

systemctl --user daemon-reload
systemctl --user enable reclip.service >/dev/null 2>&1 || true

# Передаём демону переменные дисплея, чтобы он видел буфер уже сейчас.
systemctl --user import-environment WAYLAND_DISPLAY DISPLAY XDG_RUNTIME_DIR 2>/dev/null || true

if systemctl --user restart reclip.service 2>/dev/null; then
  say "Демон запущен и включён в автозапуск."
else
  warn "Не удалось стартовать демон прямо сейчас (возможно, нет графического
       сеанса). Он запустится автоматически при следующем входе в систему."
fi

# --- 5. Инструкция по горячей клавише Super+V (4.1) ------------------------
cat <<EOF

$(say "Готово! Осталось назначить горячую клавишу.")

Откройте: Настройки → Клавиатура → Комбинации клавиш →
          Дополнительные комбинации → «+» (добавить свою) и задайте:

    Название:  reclip
    Команда:   $BIN_PATH show
    Клавиша:   Super+V   (клавиша Windows + V)

После этого Super+V будет открывать окно истории буфера.

Проверка:
  - статус демона:   systemctl --user status reclip.service
  - логи демона:     journalctl --user -u reclip.service -f
  - история в консоли: reclip list
EOF
