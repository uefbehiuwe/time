//! Параметры запуска: аргументы командной строки и переменные окружения.

use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand};

#[derive(Parser, Clone, Debug)]
#[command(
    name = "timelist",
    version,
    about = "Тайм-лист: Telegram-бот, веб-интерфейс и REST API учёта рабочего времени"
)]
pub struct Config {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Файл базы данных SQLite.
    #[arg(
        long,
        env = "TIMELIST_DB",
        default_value = "data/timelist.db",
        global = true
    )]
    pub db: PathBuf,

    /// Адрес веб-интерфейса и API.
    #[arg(
        long,
        env = "TIMELIST_LISTEN",
        default_value = "127.0.0.1:8080",
        global = true
    )]
    pub listen: String,

    /// Токен бота от @BotFather. Без него сервер работает без бота.
    #[arg(
        long,
        env = "TELEGRAM_BOT_TOKEN",
        hide_env_values = true,
        global = true
    )]
    pub bot_token: Option<String>,

    /// ID рабочих групп через запятую. Пусто — принимаются сообщения из любой группы, куда добавлен бот.
    #[arg(
        long,
        env = "TELEGRAM_CHAT_ID",
        allow_hyphen_values = true,
        global = true
    )]
    pub chat_id: Option<String>,

    /// Адрес Bot API (меняется только для тестов или собственного сервера Bot API).
    #[arg(
        long,
        env = "TELEGRAM_API_BASE",
        default_value = "https://api.telegram.org",
        global = true
    )]
    pub telegram_api: String,

    /// Ставить реакцию на сообщения: ✍ — записано, 🤔 — не понял.
    #[arg(long, env = "TELEGRAM_REACTIONS", default_value_t = true, action = ArgAction::Set, global = true)]
    pub reactions: bool,

    /// Пароль веб-интерфейса. Без него доступ открыт всем, кто видит адрес.
    #[arg(long, env = "TIMELIST_PASSWORD", hide_env_values = true, global = true)]
    pub password: Option<String>,

    /// Токен для интеграций (заголовок `Authorization: Bearer …`).
    #[arg(
        long,
        env = "TIMELIST_API_TOKEN",
        hide_env_values = true,
        global = true
    )]
    pub api_token: Option<String>,

    /// Каталог со статикой интерфейса вместо встроенной (для доработки UI без пересборки).
    #[arg(long, env = "TIMELIST_ASSETS_DIR", global = true)]
    pub assets_dir: Option<PathBuf>,
}

#[derive(Subcommand, Clone, Debug)]
pub enum Command {
    /// Запустить сервер и бота (по умолчанию).
    Serve,
    /// Импортировать книги Excel «TimeList-ГГГГ-М.xlsm».
    Import {
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Заполнить пустую базу демонстрационными данными (вымышленные сотрудники и переписка).
    Demo,
    /// Выгрузить месяц в .xlsx.
    Export {
        /// Месяц в формате ГГГГ-ММ.
        month: String,
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
}

impl Config {
    /// Пустые значения из окружения (`TELEGRAM_BOT_TOKEN=`) считаются незаданными.
    pub fn normalized(mut self) -> Self {
        let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        self.bot_token = clean(self.bot_token);
        self.chat_id = clean(self.chat_id);
        self.password = clean(self.password);
        self.api_token = clean(self.api_token);
        self
    }

    pub fn chat_ids(&self) -> Vec<i64> {
        self.chat_id
            .as_deref()
            .unwrap_or("")
            .split([',', ' ', ';'])
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    }

    pub fn auth_required(&self) -> bool {
        self.password.is_some() || self.api_token.is_some()
    }
}

/// Подхватывает `.env` из текущего каталога; уже заданные переменные не трогает.
pub fn load_dotenv() {
    let Ok(text) = std::fs::read_to_string(".env") else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim().trim_start_matches("export ").trim();
            let value = value.trim().trim_matches(['"', '\'']);
            if !key.is_empty() && std::env::var_os(key).is_none() {
                std::env::set_var(key, value);
            }
        }
    }
}
