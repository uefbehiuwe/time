//! Хранилище: SQLite в одном файле.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result};
use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use timelist_core::calendar::Special;
use timelist_core::model::Field;
use timelist_core::{Mark, Rules};

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS employees (
    id            INTEGER PRIMARY KEY,
    full_name     TEXT NOT NULL,
    nick          TEXT NOT NULL UNIQUE,
    tg_name       TEXT NOT NULL DEFAULT '',
    tg_user_id    INTEGER UNIQUE,
    aliases       TEXT NOT NULL DEFAULT '',
    auto_schedule INTEGER NOT NULL DEFAULT 0,
    active        INTEGER NOT NULL DEFAULT 1,
    sort          INTEGER NOT NULL DEFAULT 0,
    works_from    TEXT,
    works_to      TEXT
);
CREATE TABLE IF NOT EXISTS codes (
    code        TEXT PRIMARY KEY,
    kind        TEXT NOT NULL CHECK (kind IN ('absence', 'trip')),
    coef        REAL NOT NULL,
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    sort        INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS month_norms (
    year INTEGER NOT NULL, month INTEGER NOT NULL, hours REAL NOT NULL, days INTEGER NOT NULL,
    PRIMARY KEY (year, month)
);
CREATE TABLE IF NOT EXISTS month_limits (
    year INTEGER NOT NULL, month INTEGER NOT NULL, lim REAL NOT NULL,
    PRIMARY KEY (year, month)
);
CREATE TABLE IF NOT EXISTS calendar_days (
    date TEXT PRIMARY KEY, kind TEXT NOT NULL CHECK (kind IN ('holiday', 'workday', 'short')), title TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS messages (
    id            INTEGER PRIMARY KEY,
    source        TEXT NOT NULL,
    chat_id       INTEGER,
    tg_message_id INTEGER,
    tg_user_id    INTEGER,
    sender_name   TEXT NOT NULL DEFAULT '',
    employee_id   INTEGER REFERENCES employees(id) ON DELETE SET NULL,
    sent_at       TEXT NOT NULL,
    text          TEXT NOT NULL,
    edited        INTEGER NOT NULL DEFAULT 0,
    ignored       INTEGER NOT NULL DEFAULT 0,
    UNIQUE (chat_id, tg_message_id)
);
CREATE INDEX IF NOT EXISTS messages_sent_at ON messages (sent_at);
CREATE TABLE IF NOT EXISTS marks (
    employee_id INTEGER NOT NULL REFERENCES employees(id) ON DELETE CASCADE,
    date        TEXT NOT NULL,
    start INTEGER, start_code TEXT, away INTEGER, away_code TEXT, "end" INTEGER, end_code TEXT, day_code TEXT,
    note        TEXT NOT NULL DEFAULT '',
    manual_mask INTEGER NOT NULL DEFAULT 0,
    reviewed    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (employee_id, date)
);
CREATE TABLE IF NOT EXISTS periods (
    id          INTEGER PRIMARY KEY,
    employee_id INTEGER NOT NULL REFERENCES employees(id) ON DELETE CASCADE,
    code        TEXT NOT NULL,
    date_from   TEXT NOT NULL,
    date_to     TEXT NOT NULL,
    tariff      INTEGER NOT NULL DEFAULT 0,
    note        TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS month_locks (
    year INTEGER NOT NULL, month INTEGER NOT NULL, locked_at TEXT NOT NULL, locked_by TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (year, month)
);
CREATE TABLE IF NOT EXISTS snapshots (
    employee_id INTEGER NOT NULL REFERENCES employees(id) ON DELETE CASCADE,
    date        TEXT NOT NULL,
    start INTEGER, start_code TEXT, away INTEGER, away_code TEXT, "end" INTEGER, end_code TEXT, day_code TEXT,
    PRIMARY KEY (employee_id, date)
);
CREATE TABLE IF NOT EXISTS audit (
    id          INTEGER PRIMARY KEY,
    at          TEXT NOT NULL,
    user        TEXT NOT NULL DEFAULT '',
    employee_id INTEGER,
    date        TEXT,
    field       TEXT NOT NULL,
    old_value   TEXT,
    new_value   TEXT
);
CREATE INDEX IF NOT EXISTS audit_day ON audit (employee_id, date);
CREATE TABLE IF NOT EXISTS tg_users (
    tg_user_id INTEGER PRIMARY KEY,
    username   TEXT NOT NULL DEFAULT '',
    name       TEXT NOT NULL DEFAULT '',
    chat_id    INTEGER,
    last_seen  TEXT NOT NULL DEFAULT ''
);
"#;

/// Категории листа «Список» (B21:D27, B30:D32).
const SEED_CODES: &[(&str, &str, f64, &str, &str)] = &[
    (
        "О",
        "absence",
        1.0,
        "Отпуск",
        "Ежегодный оплачиваемый трудовой отпуск по графику",
    ),
    (
        "У",
        "absence",
        1.0,
        "Учебный отпуск",
        "Оплачиваемый отпуск на экзаменационные сессии (по учебному плану)",
    ),
    (
        "К",
        "absence",
        1.0,
        "Командировка",
        "Любая командировка, встреча, плановая работа вне офиса",
    ),
    (
        "Б",
        "absence",
        0.8,
        "Болезнь",
        "Болезнь или уход за больным ребёнком или членами семьи, с уведомлением",
    ),
    (
        "Ф",
        "absence",
        0.5,
        "Форс-мажор",
        "Уважительные причины, согласованные с руководством",
    ),
    (
        "Г",
        "absence",
        0.0,
        "Отгул",
        "Согласованный с руководством отгул без содержания, за свой счёт",
    ),
    (
        "П",
        "absence",
        0.0,
        "Прогул",
        "Пропуск рабочего времени без согласования с руководством",
    ),
    (
        "К1",
        "trip",
        1.0,
        "Командировка: дорога",
        "День отбытия/прибытия или вынужденный простой/отдых в командировке",
    ),
    (
        "К2",
        "trip",
        1.25,
        "Командировка: работа",
        "День полноценной работы на объекте, местности, полигоне",
    ),
    (
        "К3",
        "trip",
        1.5,
        "Командировка: выходной",
        "Работа в выходные и праздничные дни, согласованная с руководством",
    ),
];

const SEED_SETTINGS: &[(&str, &str)] = &[
    ("work_start", "540"),
    ("work_end", "1080"),
    ("lunch", "60"),
    ("limit", "1"),
    ("utc_offset_minutes", "300"),
    ("trip_use_times", "0"),
];

/// Баланс рабочего времени Узбекистана на 2026 год, 5-дневная неделя (часы, дни).
const SEED_NORMS_2026: [(f64, i64); 12] = [
    (160.0, 20),
    (160.0, 20),
    (151.0, 19),
    (176.0, 22),
    (134.0, 17),
    (176.0, 22),
    (184.0, 23),
    (160.0, 20),
    (167.0, 21),
    (168.0, 21),
    (168.0, 21),
    (175.0, 22),
];

/// Праздники, дополнительные и перенесённые выходные 2026 года (УП-257 от 24.12.2025).
const SEED_DAYS_2026: &[(&str, &str, &str)] = &[
    ("2026-01-01", "holiday", "Новый год"),
    ("2026-01-02", "holiday", "Дополнительный выходной"),
    ("2026-03-09", "holiday", "Перенос: День женщин (8 марта)"),
    ("2026-03-19", "short", "Предпраздничный день"),
    ("2026-03-20", "holiday", "Руза хайит"),
    ("2026-03-23", "holiday", "Перенос: Навруз (21 марта)"),
    ("2026-05-08", "short", "Предпраздничный день"),
    (
        "2026-05-11",
        "holiday",
        "Перенос: День памяти и почестей (9 мая)",
    ),
    ("2026-05-26", "short", "Предпраздничный день"),
    ("2026-05-27", "holiday", "Курбан хайит"),
    ("2026-05-28", "holiday", "Дополнительный выходной"),
    ("2026-05-29", "holiday", "Дополнительный выходной"),
    ("2026-08-31", "holiday", "Дополнительный выходной"),
    ("2026-09-01", "holiday", "День независимости"),
    ("2026-09-30", "short", "Предпраздничный день"),
    ("2026-10-01", "holiday", "День учителя и наставника"),
    ("2026-12-07", "short", "Предпраздничный день"),
    ("2026-12-08", "holiday", "День Конституции"),
    (
        "2026-12-12",
        "workday",
        "Рабочая суббота (перенос на 31 декабря)",
    ),
    ("2026-12-31", "holiday", "Перенесённый выходной"),
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Employee {
    #[serde(default)]
    pub id: i64,
    pub full_name: String,
    pub nick: String,
    #[serde(default)]
    pub tg_name: String,
    #[serde(default)]
    pub tg_user_id: Option<i64>,
    #[serde(default)]
    pub aliases: String,
    #[serde(default)]
    pub auto_schedule: bool,
    #[serde(default = "yes")]
    pub active: bool,
    #[serde(default)]
    pub sort: i64,
    #[serde(default)]
    pub works_from: Option<NaiveDate>,
    #[serde(default)]
    pub works_to: Option<NaiveDate>,
}

fn yes() -> bool {
    true
}

impl Employee {
    /// Имена для упоминаний в сообщениях: псевдонимы и имя из Telegram.
    pub fn alias_list(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .aliases
            .split([',', ';'])
            .map(timelist_core::parser::normalize)
            .filter(|a| !a.is_empty())
            .collect();
        if let Some(first) = self.tg_name.split_whitespace().next() {
            let first = timelist_core::parser::normalize(first);
            if !first.is_empty() && !out.contains(&first) {
                out.push(first);
            }
        }
        out
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Code {
    pub code: String,
    pub kind: String,
    pub coef: f64,
    pub title: String,
    pub description: String,
    pub sort: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Message {
    pub id: i64,
    pub source: String,
    pub chat_id: Option<i64>,
    pub tg_message_id: Option<i64>,
    pub tg_user_id: Option<i64>,
    pub sender_name: String,
    pub employee_id: Option<i64>,
    pub sent_at: NaiveDateTime,
    pub text: String,
    pub edited: bool,
    pub ignored: bool,
}

pub struct NewMessage<'a> {
    pub source: &'a str,
    pub chat_id: Option<i64>,
    pub tg_message_id: Option<i64>,
    pub tg_user_id: Option<i64>,
    pub sender_name: &'a str,
    pub employee_id: Option<i64>,
    pub sent_at: NaiveDateTime,
    pub text: &'a str,
}

#[derive(Clone, Debug, Default)]
pub struct MarkRow {
    pub mark: Mark,
    pub note: String,
    pub manual_mask: u8,
    pub reviewed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Period {
    #[serde(default)]
    pub id: i64,
    pub employee_id: i64,
    pub code: String,
    pub date_from: NaiveDate,
    pub date_to: NaiveDate,
    #[serde(default)]
    pub tariff: bool,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct CalendarDay {
    pub date: NaiveDate,
    pub kind: String,
    pub title: String,
}

/// Запись журнала изменений: кто, когда и что поменял.
pub struct NewAudit<'a> {
    pub at: &'a str,
    pub user: &'a str,
    pub employee_id: Option<i64>,
    pub day: Option<NaiveDate>,
    pub field: &'a str,
    pub old: Option<&'a str>,
    pub new: Option<&'a str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub at: String,
    pub user: String,
    pub employee_id: Option<i64>,
    pub date: Option<String>,
    pub field: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TgUser {
    pub tg_user_id: i64,
    pub username: String,
    pub name: String,
    pub chat_id: Option<i64>,
    pub last_seen: String,
    pub employee_id: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Lock {
    pub locked_at: String,
    pub locked_by: String,
}

pub struct Db {
    conn: Mutex<Connection>,
}

const SQL_DATETIME: &str = "%Y-%m-%d %H:%M:%S";

fn date(s: String) -> rusqlite::Result<NaiveDate> {
    NaiveDate::parse_from_str(&s, "%Y-%m-%d").map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn datetime(s: String) -> rusqlite::Result<NaiveDateTime> {
    NaiveDateTime::parse_from_str(&s, SQL_DATETIME).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn employee(row: &Row) -> rusqlite::Result<Employee> {
    Ok(Employee {
        id: row.get("id")?,
        full_name: row.get("full_name")?,
        nick: row.get("nick")?,
        tg_name: row.get("tg_name")?,
        tg_user_id: row.get("tg_user_id")?,
        aliases: row.get("aliases")?,
        auto_schedule: row.get("auto_schedule")?,
        active: row.get("active")?,
        sort: row.get("sort")?,
        works_from: row
            .get::<_, Option<String>>("works_from")?
            .map(date)
            .transpose()?,
        works_to: row
            .get::<_, Option<String>>("works_to")?
            .map(date)
            .transpose()?,
    })
}

fn message(row: &Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get("id")?,
        source: row.get("source")?,
        chat_id: row.get("chat_id")?,
        tg_message_id: row.get("tg_message_id")?,
        tg_user_id: row.get("tg_user_id")?,
        sender_name: row.get("sender_name")?,
        employee_id: row.get("employee_id")?,
        sent_at: datetime(row.get("sent_at")?)?,
        text: row.get("text")?,
        edited: row.get("edited")?,
        ignored: row.get("ignored")?,
    })
}

fn mark(row: &Row) -> rusqlite::Result<Mark> {
    Ok(Mark {
        start: row.get("start")?,
        start_code: row.get("start_code")?,
        away: row.get("away")?,
        away_code: row.get("away_code")?,
        end: row.get("end")?,
        end_code: row.get("end_code")?,
        day_code: row.get("day_code")?,
    })
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("не удалось создать каталог {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("не удалось открыть БД {}", path.display()))?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Db> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Db> {
        conn.execute_batch(SCHEMA)?;
        for (i, (code, kind, coef, title, description)) in SEED_CODES.iter().enumerate() {
            conn.execute(
                "INSERT OR IGNORE INTO codes (code, kind, coef, title, description, sort) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![code, kind, coef, title, description, i as i64],
            )?;
        }
        for (key, value) in SEED_SETTINGS {
            conn.execute(
                "INSERT OR IGNORE INTO settings (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
        }
        let seeded: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'calendar_seeded'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if seeded.is_none() {
            for (i, (hours, days)) in SEED_NORMS_2026.iter().enumerate() {
                conn.execute(
                    "INSERT OR IGNORE INTO month_norms (year, month, hours, days) VALUES (2026, ?1, ?2, ?3)",
                    params![i as i64 + 1, hours, days],
                )?;
            }
            for (date, kind, title) in SEED_DAYS_2026 {
                conn.execute(
                    "INSERT OR IGNORE INTO calendar_days (date, kind, title) VALUES (?1, ?2, ?3)",
                    params![date, kind, title],
                )?;
            }
            conn.execute(
                "INSERT INTO settings (key, value) VALUES ('calendar_seeded', '2026')",
                [],
            )?;
        }
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // --- настройки

    pub fn settings(&self) -> Result<BTreeMap<String, String>> {
        let conn = self.lock();
        let mut st = conn.prepare("SELECT key, value FROM settings")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.lock();
        Ok(conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.lock().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn codes(&self) -> Result<Vec<Code>> {
        let conn = self.lock();
        let mut st = conn.prepare("SELECT code, kind, coef, title, description, sort FROM codes ORDER BY kind, sort, code")?;
        let rows = st.query_map([], |r| {
            Ok(Code {
                code: r.get(0)?,
                kind: r.get(1)?,
                coef: r.get(2)?,
                title: r.get(3)?,
                description: r.get(4)?,
                sort: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn update_code(
        &self,
        code: &str,
        coef: f64,
        title: Option<&str>,
        description: Option<&str>,
    ) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE codes SET coef = ?2, title = COALESCE(?3, title), description = COALESCE(?4, description) WHERE code = ?1",
            params![code, coef, title, description],
        )?;
        Ok(n > 0)
    }

    /// Параметры расчёта: график из настроек и коэффициенты из справочника кодов.
    pub fn rules(&self) -> Result<Rules> {
        let settings = self.settings()?;
        let minutes = |key: &str, default: i32| {
            settings
                .get(key)
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        let mut rules = Rules {
            work_start: minutes("work_start", 540),
            work_end: minutes("work_end", 1080),
            lunch: minutes("lunch", 60),
            absence: BTreeMap::new(),
            trip: BTreeMap::new(),
        };
        for c in self.codes()? {
            if c.kind == "trip" {
                rules.trip.insert(c.code, c.coef)
            } else {
                rules.absence.insert(c.code, c.coef)
            };
        }
        Ok(rules)
    }

    // --- сотрудники

    pub fn employees(&self) -> Result<Vec<Employee>> {
        let conn = self.lock();
        let mut st = conn.prepare("SELECT * FROM employees ORDER BY sort, id")?;
        let rows = st.query_map([], employee)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn employee_by_nick(&self, nick: &str) -> Result<Option<Employee>> {
        let conn = self.lock();
        Ok(conn
            .query_row("SELECT * FROM employees WHERE nick = ?1", [nick], employee)
            .optional()?)
    }

    pub fn insert_employee(&self, e: &Employee) -> Result<i64> {
        let conn = self.lock();
        let sort: i64 = if e.sort > 0 {
            e.sort
        } else {
            conn.query_row(
                "SELECT COALESCE(MAX(sort), 0) + 1 FROM employees",
                [],
                |r| r.get(0),
            )?
        };
        conn.execute(
            "INSERT INTO employees (full_name, nick, tg_name, tg_user_id, aliases, auto_schedule, active, sort, works_from, works_to)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                e.full_name.trim(), e.nick.trim(), e.tg_name.trim(), e.tg_user_id, e.aliases.trim(), e.auto_schedule, e.active, sort,
                e.works_from.map(|d| d.to_string()), e.works_to.map(|d| d.to_string())
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update_employee(&self, e: &Employee) -> Result<bool> {
        let n = self.lock().execute(
            "UPDATE employees SET full_name = ?2, nick = ?3, tg_name = ?4, tg_user_id = ?5, aliases = ?6, auto_schedule = ?7,
                    active = ?8, sort = ?9, works_from = ?10, works_to = ?11 WHERE id = ?1",
            params![
                e.id, e.full_name.trim(), e.nick.trim(), e.tg_name.trim(), e.tg_user_id, e.aliases.trim(), e.auto_schedule, e.active,
                e.sort, e.works_from.map(|d| d.to_string()), e.works_to.map(|d| d.to_string())
            ],
        )?;
        Ok(n > 0)
    }

    pub fn delete_employee(&self, id: i64) -> Result<bool> {
        Ok(self
            .lock()
            .execute("DELETE FROM employees WHERE id = ?1", [id])?
            > 0)
    }

    pub fn link_tg_user(&self, employee_id: i64, tg_user_id: Option<i64>) -> Result<()> {
        let conn = self.lock();
        if let Some(uid) = tg_user_id {
            conn.execute(
                "UPDATE employees SET tg_user_id = NULL WHERE tg_user_id = ?1 AND id <> ?2",
                params![uid, employee_id],
            )?;
        }
        conn.execute(
            "UPDATE employees SET tg_user_id = ?2 WHERE id = ?1",
            params![employee_id, tg_user_id],
        )?;
        Ok(())
    }

    // --- календарь

    pub fn calendar_days(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<CalendarDay>> {
        let conn = self.lock();
        let mut st = conn.prepare("SELECT date, kind, title FROM calendar_days WHERE date BETWEEN ?1 AND ?2 ORDER BY date")?;
        let rows = st.query_map(params![from.to_string(), to.to_string()], |r| {
            Ok(CalendarDay {
                date: date(r.get(0)?)?,
                kind: r.get(1)?,
                title: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_calendar_day(&self, day: NaiveDate, kind: Special, title: &str) -> Result<()> {
        self.lock().execute(
            "INSERT INTO calendar_days (date, kind, title) VALUES (?1, ?2, ?3)
             ON CONFLICT(date) DO UPDATE SET kind = excluded.kind, title = excluded.title",
            params![day.to_string(), kind.as_str(), title.trim()],
        )?;
        Ok(())
    }

    pub fn delete_calendar_day(&self, day: NaiveDate) -> Result<bool> {
        Ok(self.lock().execute(
            "DELETE FROM calendar_days WHERE date = ?1",
            [day.to_string()],
        )? > 0)
    }

    pub fn month_norm(&self, year: i32, month: u32) -> Result<Option<(f64, i64)>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT hours, days FROM month_norms WHERE year = ?1 AND month = ?2",
                params![year, month],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn set_month_norm(&self, year: i32, month: u32, norm: Option<(f64, i64)>) -> Result<()> {
        let conn = self.lock();
        match norm {
            Some((hours, days)) => conn.execute(
                "INSERT INTO month_norms (year, month, hours, days) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(year, month) DO UPDATE SET hours = excluded.hours, days = excluded.days",
                params![year, month, hours, days],
            )?,
            None => conn.execute("DELETE FROM month_norms WHERE year = ?1 AND month = ?2", params![year, month])?,
        };
        Ok(())
    }

    pub fn month_limit(&self, year: i32, month: u32) -> Result<Option<f64>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT lim FROM month_limits WHERE year = ?1 AND month = ?2",
                params![year, month],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_month_limit(&self, year: i32, month: u32, limit: f64) -> Result<()> {
        self.lock().execute(
            "INSERT INTO month_limits (year, month, lim) VALUES (?1, ?2, ?3)
             ON CONFLICT(year, month) DO UPDATE SET lim = excluded.lim",
            params![year, month, limit],
        )?;
        Ok(())
    }

    // --- сообщения

    /// Сохраняет сообщение; для Telegram повторная доставка обновляет текст. Возвращает id и признак «новое».
    pub fn upsert_message(&self, m: &NewMessage) -> Result<(i64, bool)> {
        let conn = self.lock();
        let sent_at = m.sent_at.format(SQL_DATETIME).to_string();
        let existing: Option<(i64, String)> = match (m.chat_id, m.tg_message_id) {
            (Some(chat), Some(mid)) => conn
                .query_row("SELECT id, text FROM messages WHERE chat_id = ?1 AND tg_message_id = ?2", params![chat, mid], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?,
            _ => conn
                .query_row(
                    "SELECT id, text FROM messages WHERE source = ?1 AND sender_name = ?2 AND sent_at = ?3 AND text = ?4",
                    params![m.source, m.sender_name, sent_at, m.text],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?,
        };
        if let Some((id, text)) = existing {
            if text != m.text {
                conn.execute(
                    "UPDATE messages SET text = ?2, edited = 1 WHERE id = ?1",
                    params![id, m.text],
                )?;
            }
            return Ok((id, false));
        }
        conn.execute(
            "INSERT INTO messages (source, chat_id, tg_message_id, tg_user_id, sender_name, employee_id, sent_at, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![m.source, m.chat_id, m.tg_message_id, m.tg_user_id, m.sender_name, m.employee_id, sent_at, m.text],
        )?;
        Ok((conn.last_insert_rowid(), true))
    }

    pub fn messages_between(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<Message>> {
        let conn = self.lock();
        let mut st = conn.prepare(
            "SELECT * FROM messages WHERE sent_at >= ?1 AND sent_at < ?2 ORDER BY sent_at, id",
        )?;
        let end = to.succ_opt().unwrap_or(to);
        let rows = st.query_map(
            params![format!("{from} 00:00:00"), format!("{end} 00:00:00")],
            message,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn message(&self, id: i64) -> Result<Option<Message>> {
        let conn = self.lock();
        Ok(conn
            .query_row("SELECT * FROM messages WHERE id = ?1", [id], message)
            .optional()?)
    }

    pub fn set_message_ignored(&self, id: i64, ignored: bool) -> Result<bool> {
        Ok(self.lock().execute(
            "UPDATE messages SET ignored = ?2 WHERE id = ?1",
            params![id, ignored],
        )? > 0)
    }

    pub fn set_message_employee(&self, id: i64, employee_id: Option<i64>) -> Result<bool> {
        Ok(self.lock().execute(
            "UPDATE messages SET employee_id = ?2 WHERE id = ?1",
            params![id, employee_id],
        )? > 0)
    }

    pub fn delete_message(&self, id: i64) -> Result<bool> {
        Ok(self
            .lock()
            .execute("DELETE FROM messages WHERE id = ?1", [id])?
            > 0)
    }

    /// Дата самого раннего сообщения — с неё от сотрудников ожидаются отметки.
    pub fn first_message_date(&self) -> Result<Option<NaiveDate>> {
        let conn = self.lock();
        let first: Option<String> =
            conn.query_row("SELECT MIN(sent_at) FROM messages", [], |r| r.get(0))?;
        Ok(first.and_then(|s| NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()))
    }

    pub fn last_message_at(&self) -> Result<Option<String>> {
        let conn = self.lock();
        Ok(conn.query_row(
            "SELECT MAX(sent_at) FROM messages WHERE source = 'telegram'",
            [],
            |r| r.get(0),
        )?)
    }

    // --- Telegram-пользователи

    pub fn touch_tg_user(
        &self,
        tg_user_id: i64,
        username: &str,
        name: &str,
        chat_id: i64,
        seen: NaiveDateTime,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO tg_users (tg_user_id, username, name, chat_id, last_seen) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(tg_user_id) DO UPDATE SET username = excluded.username, name = excluded.name,
                 chat_id = excluded.chat_id, last_seen = excluded.last_seen",
            params![tg_user_id, username, name, chat_id, seen.format(SQL_DATETIME).to_string()],
        )?;
        Ok(())
    }

    pub fn tg_users(&self) -> Result<Vec<TgUser>> {
        let conn = self.lock();
        let mut st = conn.prepare(
            "SELECT u.tg_user_id, u.username, u.name, u.chat_id, u.last_seen, e.id
             FROM tg_users u LEFT JOIN employees e ON e.tg_user_id = u.tg_user_id ORDER BY u.last_seen DESC",
        )?;
        let rows = st.query_map([], |r| {
            Ok(TgUser {
                tg_user_id: r.get(0)?,
                username: r.get(1)?,
                name: r.get(2)?,
                chat_id: r.get(3)?,
                last_seen: r.get(4)?,
                employee_id: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // --- отметки

    pub fn marks_between(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<BTreeMap<(i64, NaiveDate), MarkRow>> {
        let conn = self.lock();
        let mut st = conn.prepare("SELECT * FROM marks WHERE date BETWEEN ?1 AND ?2")?;
        let rows = st.query_map(params![from.to_string(), to.to_string()], |r| {
            let key = (r.get::<_, i64>("employee_id")?, date(r.get("date")?)?);
            Ok((
                key,
                MarkRow {
                    mark: mark(r)?,
                    note: r.get("note")?,
                    manual_mask: r.get("manual_mask")?,
                    reviewed: r.get("reviewed")?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn mark_row(&self, employee_id: i64, day: NaiveDate) -> Result<MarkRow> {
        Ok(self
            .marks_between(day, day)?
            .remove(&(employee_id, day))
            .unwrap_or_default())
    }

    pub fn save_mark_row(&self, employee_id: i64, day: NaiveDate, row: &MarkRow) -> Result<()> {
        let conn = self.lock();
        if row.manual_mask == 0 && row.note.is_empty() && !row.reviewed {
            conn.execute(
                "DELETE FROM marks WHERE employee_id = ?1 AND date = ?2",
                params![employee_id, day.to_string()],
            )?;
            return Ok(());
        }
        // хранятся только поля, заданные вручную
        let kept = Mark::default().overlay(&row.mark, row.manual_mask);
        conn.execute(
            r#"INSERT INTO marks (employee_id, date, start, start_code, away, away_code, "end", end_code, day_code, note, manual_mask, reviewed)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
               ON CONFLICT(employee_id, date) DO UPDATE SET start = excluded.start, start_code = excluded.start_code,
                   away = excluded.away, away_code = excluded.away_code, "end" = excluded."end", end_code = excluded.end_code,
                   day_code = excluded.day_code, note = excluded.note, manual_mask = excluded.manual_mask, reviewed = excluded.reviewed"#,
            params![
                employee_id, day.to_string(), kept.start, kept.start_code, kept.away, kept.away_code, kept.end, kept.end_code,
                kept.day_code, row.note, row.manual_mask, row.reviewed
            ],
        )?;
        Ok(())
    }

    // --- периоды отсутствия

    pub fn periods_between(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<Period>> {
        let conn = self.lock();
        let mut st = conn.prepare(
            "SELECT id, employee_id, code, date_from, date_to, tariff, note FROM periods
             WHERE date_from <= ?2 AND date_to >= ?1 ORDER BY date_from, id",
        )?;
        let rows = st.query_map(params![from.to_string(), to.to_string()], |r| {
            Ok(Period {
                id: r.get(0)?,
                employee_id: r.get(1)?,
                code: r.get(2)?,
                date_from: date(r.get(3)?)?,
                date_to: date(r.get(4)?)?,
                tariff: r.get(5)?,
                note: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn insert_period(&self, p: &Period) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO periods (employee_id, code, date_from, date_to, tariff, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![p.employee_id, p.code, p.date_from.to_string(), p.date_to.to_string(), p.tariff, p.note.trim()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn period(&self, id: i64) -> Result<Option<Period>> {
        let far = |y| NaiveDate::from_ymd_opt(y, 1, 1).expect("valid date");
        Ok(self
            .periods_between(far(1900), far(9000))?
            .into_iter()
            .find(|p| p.id == id))
    }

    pub fn delete_period(&self, id: i64) -> Result<bool> {
        Ok(self
            .lock()
            .execute("DELETE FROM periods WHERE id = ?1", [id])?
            > 0)
    }

    // --- закрытие месяца

    pub fn month_lock(&self, year: i32, month: u32) -> Result<Option<Lock>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT locked_at, locked_by FROM month_locks WHERE year = ?1 AND month = ?2",
                params![year, month],
                |r| {
                    Ok(Lock {
                        locked_at: r.get(0)?,
                        locked_by: r.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// Закрывает месяц: итоговые отметки замораживаются и больше не зависят от сообщений и периодов.
    pub fn lock_month(
        &self,
        year: i32,
        month: u32,
        at: &str,
        by: &str,
        frozen: &[(i64, NaiveDate, Mark)],
    ) -> Result<()> {
        let conn = self.lock();
        let tx = conn.unchecked_transaction()?;
        let (from, to) = month_bounds(year, month);
        tx.execute(
            "DELETE FROM snapshots WHERE date BETWEEN ?1 AND ?2",
            params![from.to_string(), to.to_string()],
        )?;
        for (employee_id, day, m) in frozen {
            tx.execute(
                r#"INSERT INTO snapshots (employee_id, date, start, start_code, away, away_code, "end", end_code, day_code)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
                params![employee_id, day.to_string(), m.start, m.start_code, m.away, m.away_code, m.end, m.end_code, m.day_code],
            )?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO month_locks (year, month, locked_at, locked_by) VALUES (?1, ?2, ?3, ?4)",
            params![year, month, at, by],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn unlock_month(&self, year: i32, month: u32) -> Result<()> {
        let conn = self.lock();
        let tx = conn.unchecked_transaction()?;
        let (from, to) = month_bounds(year, month);
        tx.execute(
            "DELETE FROM snapshots WHERE date BETWEEN ?1 AND ?2",
            params![from.to_string(), to.to_string()],
        )?;
        tx.execute(
            "DELETE FROM month_locks WHERE year = ?1 AND month = ?2",
            params![year, month],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn snapshots_between(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<BTreeMap<(i64, NaiveDate), Mark>> {
        let conn = self.lock();
        let mut st = conn.prepare("SELECT * FROM snapshots WHERE date BETWEEN ?1 AND ?2")?;
        let rows = st.query_map(params![from.to_string(), to.to_string()], |r| {
            Ok((
                (r.get::<_, i64>("employee_id")?, date(r.get("date")?)?),
                mark(r)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // --- журнал изменений

    pub fn add_audit(&self, a: &NewAudit) -> Result<()> {
        self.lock().execute(
            "INSERT INTO audit (at, user, employee_id, date, field, old_value, new_value) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![a.at, a.user, a.employee_id, a.day.map(|d| d.to_string()), a.field, a.old, a.new],
        )?;
        Ok(())
    }

    pub fn audit(
        &self,
        employee_id: Option<i64>,
        day: Option<NaiveDate>,
        limit: i64,
    ) -> Result<Vec<AuditEntry>> {
        let conn = self.lock();
        let mut st = conn.prepare(
            "SELECT id, at, user, employee_id, date, field, old_value, new_value FROM audit
             WHERE (?1 IS NULL OR employee_id = ?1) AND (?2 IS NULL OR date = ?2) ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = st.query_map(
            params![employee_id, day.map(|d| d.to_string()), limit],
            |r| {
                Ok(AuditEntry {
                    id: r.get(0)?,
                    at: r.get(1)?,
                    user: r.get(2)?,
                    employee_id: r.get(3)?,
                    date: r.get(4)?,
                    field: r.get(5)?,
                    old_value: r.get(6)?,
                    new_value: r.get(7)?,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// Первый и последний день месяца.
pub fn month_bounds(year: i32, month: u32) -> (NaiveDate, NaiveDate) {
    let days = timelist_core::calendar::month_days(year, month);
    match (days.first(), days.last()) {
        (Some(first), Some(last)) => (*first, *last),
        _ => (NaiveDate::MIN, NaiveDate::MIN),
    }
}

/// Все поля отметки заданы вручную.
pub const ALL_FIELDS: u8 = {
    let mut mask = 0;
    let mut i = 0;
    while i < Field::ALL.len() {
        mask |= 1 << i;
        i += 1;
    }
    mask
};
