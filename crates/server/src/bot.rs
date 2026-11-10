//! Telegram-бот: слушает рабочую группу и превращает сообщения в отметки.
//!
//! Работает через long polling (`getUpdates`), входящий адрес и вебхуки не нужны.
//! Чтобы бот видел все сообщения группы, у него должен быть отключён режим
//! приватности (@BotFather → /setprivacy → Disable) либо он должен быть администратором.

use std::collections::BTreeMap;
use std::time::Duration as StdDuration;

use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Datelike, Duration};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use timelist_core::parser::Kind;
use timelist_core::timefmt::{fmt_duration, fmt_hm};
use tracing::{debug, info, warn};

use crate::app::AppState;
use crate::db::NewMessage;
use crate::service::Engine;

const HELP: &str = "Я веду тайм-лист по сообщениям в рабочей группе.\n\n\
Что писать в группу:\n\
• в начале дня — «офис» (если до офиса ездили по делам: «офис, до банк»);\n\
• при выезде более чем на час — «выезд на объект …», по возвращении — «вернулся»;\n\
• в конце дня — «уход» или «убыл»;\n\
• если опаздываете или не выйдете — причину: «задержусь у врача», «болею», «отгул».\n\n\
Время берётся из сообщения. Если забыли отметиться — укажите его сами: «офис 9:10», «уход 18:30», \
за прошлый день — «17.09 уход 18:00».\n\n\
Команды: /me — мои отметки за сегодня и итог месяца, /id — мой Telegram ID.";

#[derive(Clone, Debug, Default, Serialize)]
pub struct BotStatus {
    pub enabled: bool,
    pub username: Option<String>,
    /// Время последнего успешного опроса Telegram (местное).
    pub last_ok: Option<String>,
    pub last_error: Option<String>,
    /// Группы, из которых приходили сообщения: id → название.
    pub chats: BTreeMap<i64, String>,
}

#[derive(Deserialize)]
struct Reply<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Deserialize)]
struct Update {
    update_id: i64,
    message: Option<TgMessage>,
    edited_message: Option<TgMessage>,
}

#[derive(Deserialize)]
struct TgMessage {
    message_id: i64,
    date: i64,
    chat: Chat,
    from: Option<User>,
    text: Option<String>,
    caption: Option<String>,
}

#[derive(Deserialize)]
struct Chat {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
    title: Option<String>,
}

#[derive(Deserialize)]
struct User {
    id: i64,
    #[serde(default)]
    is_bot: bool,
    #[serde(default)]
    first_name: String,
    last_name: Option<String>,
    username: Option<String>,
}

struct Api {
    http: reqwest::Client,
    /// `https://api.telegram.org/bot<токен>` — в журнал не выводится.
    base: String,
}

impl Api {
    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        body: serde_json::Value,
        timeout: u64,
    ) -> Result<T> {
        let request = self
            .http
            .post(format!("{}/{method}", self.base))
            .json(&body)
            .timeout(StdDuration::from_secs(timeout));
        // адрес содержит токен бота — убираем его из текста ошибки
        let response = request
            .send()
            .await
            .map_err(|e| anyhow!("{method}: {}", e.without_url()))?;
        let reply: Reply<T> = response
            .json()
            .await
            .map_err(|e| anyhow!("{method}: {}", e.without_url()))?;
        match (reply.ok, reply.result) {
            (true, Some(result)) => Ok(result),
            _ => bail!(
                "{method}: {}",
                reply
                    .description
                    .unwrap_or_else(|| "ошибка Telegram".into())
            ),
        }
    }

    async fn send(&self, chat_id: i64, text: &str, reply_to: i64) -> Result<()> {
        let body = json!({
            "chat_id": chat_id,
            "text": text,
            "reply_parameters": { "message_id": reply_to, "allow_sending_without_reply": true },
        });
        self.call::<serde_json::Value>("sendMessage", body, 20)
            .await
            .map(|_| ())
    }

    async fn react(&self, chat_id: i64, message_id: i64, emoji: &str) -> Result<()> {
        let body = json!({ "chat_id": chat_id, "message_id": message_id, "reaction": [{ "type": "emoji", "emoji": emoji }] });
        self.call::<bool>("setMessageReaction", body, 20)
            .await
            .map(|_| ())
    }
}

/// Бесконечный цикл опроса Telegram. Завершается только вместе с процессом.
pub async fn run(state: AppState) {
    let Some(token) = state.cfg.bot_token.clone() else {
        return;
    };
    let api = Api {
        http: reqwest::Client::new(),
        base: format!(
            "{}/bot{token}",
            state.cfg.telegram_api.trim_end_matches('/')
        ),
    };
    state.bot_status(|s| s.enabled = true);

    let mut offset: i64 = state
        .db
        .setting("tg_offset")
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut backoff = 1;
    let mut introduced = false;
    loop {
        if !introduced {
            match api.call::<User>("getMe", json!({}), 20).await {
                Ok(me) => {
                    info!(
                        bot = me.username.as_deref().unwrap_or("?"),
                        "бот подключён к Telegram"
                    );
                    state.bot_status(|s| s.username = me.username.clone());
                    introduced = true;
                }
                Err(e) => {
                    warn!("не удалось подключиться к Telegram: {e}");
                    state.bot_status(|s| s.last_error = Some(e.to_string()));
                    tokio::time::sleep(StdDuration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(60);
                    continue;
                }
            }
        }
        let body = json!({ "offset": offset, "timeout": 50, "allowed_updates": ["message", "edited_message"] });
        match api.call::<Vec<Update>>("getUpdates", body, 65).await {
            Ok(updates) => {
                backoff = 1;
                let now = state.now().format("%Y-%m-%d %H:%M:%S").to_string();
                state.bot_status(|s| {
                    s.last_ok = Some(now);
                    s.last_error = None;
                });
                for update in updates {
                    offset = offset.max(update.update_id + 1);
                    let (message, edited) = match (update.message, update.edited_message) {
                        (Some(m), _) => (m, false),
                        (None, Some(m)) => (m, true),
                        (None, None) => continue,
                    };
                    if let Err(e) = handle(&state, &api, message, edited).await {
                        warn!("сообщение Telegram не обработано: {e:#}");
                    }
                }
                if let Err(e) = state.db.set_setting("tg_offset", &offset.to_string()) {
                    warn!("не удалось сохранить позицию опроса: {e:#}");
                }
            }
            Err(e) => {
                warn!("ошибка опроса Telegram: {e}");
                state.bot_status(|s| s.last_error = Some(e.to_string()));
                tokio::time::sleep(StdDuration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(60);
            }
        }
    }
}

async fn handle(state: &AppState, api: &Api, msg: TgMessage, edited: bool) -> Result<()> {
    let Some(from) = msg.from.as_ref().filter(|u| !u.is_bot) else {
        return Ok(());
    };
    let Some(text) = msg
        .text
        .as_deref()
        .or(msg.caption.as_deref())
        .map(str::trim)
        .filter(|t| !t.is_empty())
    else {
        return Ok(());
    };
    let private = msg.chat.kind == "private";

    if text.starts_with('/') {
        let command = text
            .split_whitespace()
            .next()
            .unwrap_or("")
            .split('@')
            .next()
            .unwrap_or("")
            .to_lowercase();
        let reply = match command.as_str() {
            "/start" | "/help" => HELP.to_string(),
            "/id" => format!(
                "Ваш Telegram ID: {}\nID этого чата: {}",
                from.id, msg.chat.id
            ),
            "/me" | "/status" => my_report(state, from.id)?,
            _ => return Ok(()),
        };
        return api.send(msg.chat.id, &reply, msg.message_id).await;
    }
    if private {
        let hint =
            "Отметки принимаются только в рабочей группе. Здесь доступны команды /help, /me и /id.";
        return api.send(msg.chat.id, hint, msg.message_id).await;
    }
    let allowed = state.cfg.chat_ids();
    if !allowed.is_empty() && !allowed.contains(&msg.chat.id) {
        debug!(
            chat = msg.chat.id,
            "сообщение из посторонней группы пропущено"
        );
        return Ok(());
    }

    let sent_at = DateTime::from_timestamp(msg.date, 0)
        .ok_or_else(|| anyhow!("некорректное время сообщения"))?
        .naive_utc()
        + Duration::minutes(state.utc_offset());
    let name = format!(
        "{} {}",
        from.first_name,
        from.last_name.as_deref().unwrap_or("")
    )
    .trim()
    .to_string();
    state.db.touch_tg_user(
        from.id,
        from.username.as_deref().unwrap_or(""),
        &name,
        msg.chat.id,
        sent_at,
    )?;
    state.bot_status(|s| {
        s.chats
            .insert(msg.chat.id, msg.chat.title.clone().unwrap_or_default());
    });
    let (id, _) = state.db.upsert_message(&NewMessage {
        source: "telegram",
        chat_id: Some(msg.chat.id),
        tg_message_id: Some(msg.message_id),
        tg_user_id: Some(from.id),
        sender_name: &name,
        employee_id: None,
        sent_at,
        text,
    })?;
    state.notify();
    debug!(id, edited, "сообщение Telegram сохранено");

    if state.cfg.reactions {
        let engine = Engine::new(&state.db)?;
        let Some(stored) = state.db.message(id)? else {
            return Ok(());
        };
        let (view, _) = engine.interpret(&stored);
        let emoji = match (view.author_id, view.kind, view.recognized) {
            (None, _, _) => Some("🤷"),
            (_, Kind::Info, _) => None,
            (_, _, true) => Some("✍"),
            (_, _, false) => Some("🤔"),
        };
        if let Some(emoji) = emoji {
            if let Err(e) = api.react(msg.chat.id, msg.message_id, emoji).await {
                debug!("реакция не поставлена: {e}");
            }
        }
    }
    Ok(())
}

/// Ответ на /me: отметки за сегодня и итог месяца.
fn my_report(state: &AppState, tg_user_id: i64) -> Result<String> {
    let engine = Engine::new(&state.db)?;
    let Some(employee) = engine
        .employees
        .iter()
        .find(|e| e.tg_user_id == Some(tg_user_id))
    else {
        return Ok(format!("Ваш Telegram ID {tg_user_id} ещё не привязан к сотруднику. Передайте его администратору тайм-листа."));
    };
    let today = state.now().date();
    let view = engine.month(today.year(), today.month(), today)?;
    let Some(me) = view.employees.iter().find(|e| e.id == employee.id) else {
        return Ok("За этот месяц отметок пока нет.".into());
    };
    let cell = &me.cells[today.day0() as usize];
    let time = |v: Option<i32>| v.map(fmt_hm).unwrap_or_else(|| "—".into());
    let code = |v: &Option<String>| v.as_deref().map(|c| format!(" ({c})")).unwrap_or_default();
    let mut out = format!(
        "{} · {}\nНачало: {}{}\nКонец: {}{}\n",
        employee.nick,
        today.format("%d.%m.%Y"),
        time(cell.m.start),
        code(&cell.m.start_code),
        time(cell.m.end),
        code(&cell.m.end_code),
    );
    if let Some(day_code) = &cell.m.day_code {
        out.push_str(&format!("Командировка: {day_code}\n"));
    }
    let t = &me.tabel;
    out.push_str(&format!(
        "\n{}: учтено {} из {} ч ({:.1}%)\nОпозданий: {} ({}), ранних уходов: {} ({})",
        view.title,
        fmt_duration(t.max),
        view.norm_hours,
        t.usage * 100.0,
        t.late_n_calc,
        fmt_duration(t.late_t_calc),
        t.early_n_calc,
        fmt_duration(t.early_t_calc),
    ));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, State};
    use axum::routing::post;
    use axum::{Json, Router};
    use chrono::NaiveDate;
    use clap::Parser;

    use super::*;
    use crate::config::Config;
    use crate::db::{Db, Employee};

    /// Подставной Bot API: первый `getUpdates` отдаёт пачку обновлений, остальные — пусто.
    #[derive(Clone, Default)]
    struct Mock {
        calls: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
        served: Arc<Mutex<bool>>,
    }

    fn updates() -> serde_json::Value {
        // 02.11.2026 04:00 UTC = 09:00 по Ташкенту
        let t0 = NaiveDate::from_ymd_opt(2026, 11, 2)
            .unwrap()
            .and_hms_opt(4, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let group = json!({ "id": -100123, "type": "supergroup", "title": "Time List" });
        let user = |id: i64, first: &str, last: &str| json!({ "id": id, "is_bot": false, "first_name": first, "last_name": last, "username": "u" });
        let oleg = user(111, "Олег", "Сомов");
        json!([
            { "update_id": 10, "message": { "message_id": 1, "date": t0, "chat": group, "from": oleg, "text": "Офис" } },
            { "update_id": 11, "message": { "message_id": 2, "date": t0 + 300, "chat": group, "from": user(222, "Новый", "Человек"), "text": "Офис" } },
            { "update_id": 12, "message": { "message_id": 3, "date": t0 + 600, "chat": group, "from": oleg, "text": "/id@timelist_test_bot" } },
            { "update_id": 13, "message": { "message_id": 4, "date": t0 + 900, "chat": { "id": -100999, "type": "group", "title": "Чужая группа" }, "from": oleg, "text": "Офис" } },
            { "update_id": 14, "edited_message": { "message_id": 1, "date": t0, "edit_date": t0 + 1200, "chat": group, "from": oleg, "text": "Офис, до банк" } },
            { "update_id": 15, "message": { "message_id": 5, "date": t0 + 3600, "chat": group, "from": oleg, "text": "Коллеги, планёрка в три" } },
            { "update_id": 16, "message": { "message_id": 6, "date": t0 + 32400, "chat": group, "from": { "id": 7, "is_bot": true, "first_name": "Другой бот" }, "text": "Уход" } },
        ])
    }

    async fn method(
        State(mock): State<Mock>,
        Path((bot, method)): Path<(String, String)>,
        Json(body): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        assert_eq!(bot, "botTEST:SECRET");
        mock.calls.lock().unwrap().push((method.clone(), body));
        let result = match method.as_str() {
            "getMe" => {
                json!({ "id": 1, "is_bot": true, "first_name": "Тайм-лист", "username": "timelist_test_bot" })
            }
            "getUpdates" => {
                let first = !std::mem::replace(&mut *mock.served.lock().unwrap(), true);
                if first {
                    updates()
                } else {
                    tokio::time::sleep(StdDuration::from_millis(50)).await;
                    json!([])
                }
            }
            "setMessageReaction" => json!(true),
            _ => json!({ "message_id": 999 }),
        };
        Json(json!({ "ok": true, "result": result }))
    }

    #[tokio::test]
    async fn bot_stores_group_messages_and_answers() {
        let mock = Mock::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/{bot}/{method}", post(method))
            .with_state(mock.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let db = Db::in_memory().unwrap();
        let oleg = db
            .insert_employee(&Employee {
                id: 0,
                full_name: "Сомов О.А.".into(),
                nick: "ОлС".into(),
                tg_name: String::new(),
                tg_user_id: Some(111),
                aliases: String::new(),
                auto_schedule: false,
                active: true,
                sort: 0,
                works_from: None,
                works_to: None,
            })
            .unwrap();
        let cfg = Config::parse_from([
            "timelist",
            "--bot-token",
            "TEST:SECRET",
            "--telegram-api",
            &base,
            "--chat-id=-100123",
        ])
        .normalized();
        let state = AppState::new(db, cfg);
        let bot = tokio::spawn(run(state.clone()));

        // ждём, пока бот обработает пачку и подтвердит её следующим опросом
        for _ in 0..200 {
            let confirmed = mock
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|(m, body)| m == "getUpdates" && body["offset"] == 17);
            if confirmed {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(25)).await;
        }
        bot.abort();
        assert_eq!(
            state.db.setting("tg_offset").unwrap().as_deref(),
            Some("17")
        );

        let day = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        let stored = state.db.messages_between(day, day).unwrap();
        // чужая группа, команда и сообщение другого бота не сохраняются
        let texts: Vec<&str> = stored.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["Офис, до банк", "Офис", "Коллеги, планёрка в три"]);
        let first = &stored[0];
        assert_eq!(
            first.sent_at,
            day.and_hms_opt(9, 0, 0).unwrap(),
            "время переводится в местное (UTC+5)"
        );
        assert_eq!(
            (first.tg_user_id, first.sender_name.as_str(), first.edited),
            (Some(111), "Олег Сомов", true)
        );

        // отметка появилась в тайм-листе: приход 9:00 с кодом «К» из «до банк»
        let view = Engine::new(&state.db)
            .unwrap()
            .month(2026, 11, day)
            .unwrap();
        let cell = &view.employees.iter().find(|e| e.id == oleg).unwrap().cells[1];
        assert_eq!(
            (cell.m.start, cell.m.start_code.as_deref()),
            (Some(540), Some("К"))
        );

        let calls = mock.calls.lock().unwrap();
        let reactions: Vec<(i64, &str)> = calls
            .iter()
            .filter(|(m, _)| m == "setMessageReaction")
            .map(|(_, b)| {
                (
                    b["message_id"].as_i64().unwrap(),
                    b["reaction"][0]["emoji"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(reactions, [(1, "✍"), (2, "🤷"), (1, "✍"), (5, "🤔")]);
        let replies: Vec<&str> = calls
            .iter()
            .filter(|(m, _)| m == "sendMessage")
            .map(|(_, b)| b["text"].as_str().unwrap())
            .collect();
        assert_eq!(replies, ["Ваш Telegram ID: 111\nID этого чата: -100123"]);

        // незнакомый отправитель запомнен — его можно привязать к сотруднику в интерфейсе
        let users = state.db.tg_users().unwrap();
        assert!(users
            .iter()
            .any(|u| u.tg_user_id == 222 && u.name == "Новый Человек" && u.employee_id.is_none()));
        assert!(users
            .iter()
            .any(|u| u.tg_user_id == 111 && u.employee_id == Some(oleg)));
        assert_eq!(
            state.bot_status(|s| s.username.clone()).as_deref(),
            Some("timelist_test_bot")
        );
    }
}
