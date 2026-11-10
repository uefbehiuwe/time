mod api;
mod app;
mod bot;
mod config;
mod db;
mod demo;
mod excel;
mod service;
mod tgtext;

use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::app::AppState;
use crate::config::{Command, Config};
use crate::db::Db;
use crate::service::Engine;

fn main() -> Result<()> {
    // .env читается до запуска потоков: менять окружение безопасно только так
    config::load_dotenv();
    let cfg = Config::parse().normalized();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let db = Db::open(&cfg.db)?;
    match cfg.command.clone().unwrap_or(Command::Serve) {
        Command::Import { files } => {
            for file in files {
                let bytes = std::fs::read(&file)
                    .with_context(|| format!("не удалось прочитать {}", file.display()))?;
                let report = excel::import_workbook(&db, bytes)
                    .with_context(|| format!("импорт {}", file.display()))?;
                println!(
                    "{}: месяц {}, сотрудников {} (новых {}), дней с отметками {}, сообщений добавлено {}, пропущено {}",
                    file.display(),
                    report.month.as_deref().unwrap_or("?"),
                    report.employees_total,
                    report.employees_added,
                    report.mark_days,
                    report.messages_added,
                    report.messages_skipped,
                );
                for note in report.notes {
                    println!("  примечание: {note}");
                }
            }
            Ok(())
        }
        Command::Demo => {
            let state = AppState::new(db, cfg);
            println!("{}", demo::seed(&state.db, state.now().date())?);
            Ok(())
        }
        Command::Export { month, out } => {
            let (year, m) = month
                .split_once('-')
                .and_then(|(y, m)| Some((y.parse::<i32>().ok()?, m.parse::<u32>().ok()?)))
                .filter(|(_, m)| (1..=12).contains(m))
                .context("месяц указывается как ГГГГ-ММ")?;
            let state = AppState::new(db, cfg);
            let view = Engine::new(&state.db)?.month(year, m, state.now().date())?;
            let path = out.unwrap_or_else(|| format!("TimeList-{year}-{m:02}.xlsx").into());
            std::fs::write(&path, excel::export_month(&view)?)
                .with_context(|| format!("не удалось записать {}", path.display()))?;
            println!("{}", path.display());
            Ok(())
        }
        Command::Serve => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(serve(db, cfg)),
    }
}

async fn serve(db: Db, cfg: Config) -> Result<()> {
    let state = AppState::new(db, cfg);
    let listener = tokio::net::TcpListener::bind(&state.cfg.listen)
        .await
        .with_context(|| format!("не удалось занять адрес {}", state.cfg.listen))?;
    let addr: SocketAddr = listener.local_addr()?;
    info!("интерфейс и API: http://{addr}");
    if !addr.ip().is_loopback() && !state.cfg.auth_required() {
        warn!("сервер доступен по сети без пароля — задайте TIMELIST_PASSWORD");
    }
    if state.cfg.bot_token.is_some() {
        tokio::spawn(bot::run(state.clone()));
    } else {
        info!("TELEGRAM_BOT_TOKEN не задан — бот выключен, работает только интерфейс");
    }
    tokio::select! {
        result = axum::serve(listener, api::router(state)) => result.context("сервер остановился с ошибкой"),
        _ = tokio::signal::ctrl_c() => {
            info!("остановка");
            Ok(())
        }
    }
}
