//! Точка входа: разбираем под-команду (`clap`) и вызываем нужный модуль.
//! Под-команды: `reclip daemon` | `reclip show` | `reclip list` (docs/07, 7.1).
//!
//! На Этапе 1 реализованы модель и хранилище; сами под-команды пока заглушки —
//! они наполнятся на своих этапах (list — Этап 4, daemon — Этап 3, show — Этап 5).

use anyhow::Result;
use clap::{Parser, Subcommand};

use reclip::{daemon, storage::Storage};

#[derive(Parser)]
#[command(
    name = "reclip",
    version,
    about = "Менеджер истории буфера обмена (Win+V) для Linux GNOME/Wayland"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Фоновый демон: следит за буфером и наполняет историю.
    Daemon,
    /// Открыть окно выбора записи (пикер).
    Show,
    /// Напечатать историю в терминал.
    List,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Daemon => {
            init_logging();
            let storage = Storage::open(reclip::storage::default_db_path()?)?;
            daemon::run(storage)?;
        }
        Command::Show => {
            eprintln!("`reclip show` появится на Этапе 5.");
        }
        Command::List => {
            eprintln!("`reclip list` появится на Этапе 4.");
        }
    }
    Ok(())
}

/// Логи в stderr (их подхватит systemd → `journalctl --user`, 8.2).
/// Уровень по умолчанию — `info`; переопределяется переменной `RUST_LOG`.
fn init_logging() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
}
