//! Общее состояние сервера.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use chrono::{Duration, NaiveDateTime, Utc};
use tokio::sync::broadcast;

use crate::bot::BotStatus;
use crate::config::Config;
use crate::db::Db;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    pub cfg: Arc<Config>,
    /// Уведомления интерфейсу об изменениях (SSE).
    pub changes: broadcast::Sender<&'static str>,
    pub sessions: Arc<Mutex<HashSet<String>>>,
    bot: Arc<Mutex<BotStatus>>,
}

impl AppState {
    pub fn new(db: Db, cfg: Config) -> Self {
        AppState {
            db: Arc::new(db),
            cfg: Arc::new(cfg),
            changes: broadcast::channel(64).0,
            sessions: Arc::default(),
            bot: Arc::default(),
        }
    }

    /// Смещение местного времени от UTC в минутах (настройка `utc_offset_minutes`, по умолчанию Ташкент, +5:00).
    pub fn utc_offset(&self) -> i64 {
        self.db
            .setting("utc_offset_minutes")
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300)
    }

    /// Текущее местное время.
    pub fn now(&self) -> NaiveDateTime {
        Utc::now().naive_utc() + Duration::minutes(self.utc_offset())
    }

    /// Сообщает открытым интерфейсам, что данные изменились.
    pub fn notify(&self) {
        let _ = self.changes.send("change");
    }

    pub fn bot_status<R>(&self, f: impl FnOnce(&mut BotStatus) -> R) -> R {
        f(&mut self.bot.lock().unwrap_or_else(|e| e.into_inner()))
    }
}
