//! REST API и раздача встроенного веб-интерфейса.

use std::convert::Infallible;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use timelist_core::calendar::{month_name, Calendar, Special};
use timelist_core::timefmt::{fmt_hm, parse_time};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use crate::app::AppState;
use crate::db::{month_bounds, Employee, Message, NewAudit, NewMessage, Period};
use crate::excel;
use crate::service::{EditError, Engine, MarkEdit, MessageView};

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");
const APP_CSS: &str = include_str!("../assets/app.css");
const SESSION_COOKIE: &str = "tl_session";

pub struct ApiError(StatusCode, String);

impl ApiError {
    fn bad(msg: impl Into<String>) -> Self {
        ApiError(StatusCode::BAD_REQUEST, msg.into())
    }

    fn not_found() -> Self {
        ApiError(StatusCode::NOT_FOUND, "не найдено".into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        let text = format!("{e:#}");
        if text.contains("UNIQUE constraint failed") {
            return ApiError(
                StatusCode::CONFLICT,
                "такое значение уже используется (ник или Telegram ID должны быть уникальны)"
                    .into(),
            );
        }
        tracing::error!("внутренняя ошибка: {text}");
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "внутренняя ошибка сервера".into(),
        )
    }
}

impl From<EditError> for ApiError {
    fn from(e: EditError) -> Self {
        match e {
            EditError::Locked => ApiError(
                StatusCode::CONFLICT,
                "месяц закрыт — откройте его, чтобы вносить правки".into(),
            ),
            EditError::NotFound => ApiError::not_found(),
            EditError::Invalid(msg) => ApiError::bad(msg),
            EditError::Internal(e) => e.into(),
        }
    }
}

type ApiResult<T> = Result<T, ApiError>;

fn parse_ym(ym: &str) -> ApiResult<(i32, u32)> {
    let parsed = ym
        .split_once('-')
        .and_then(|(y, m)| Some((y.parse::<i32>().ok()?, m.parse::<u32>().ok()?)));
    match parsed {
        Some((y, m)) if (2000..=2100).contains(&y) && (1..=12).contains(&m) => Ok((y, m)),
        _ => Err(ApiError::bad("месяц указывается как ГГГГ-ММ")),
    }
}

fn parse_date(s: &str) -> ApiResult<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| ApiError::bad("дата указывается как ГГГГ-ММ-ДД"))
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = match bytes.get(i + 1..i + 3) {
            Some(hex) if bytes[i] == b'%' => std::str::from_utf8(hex)
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok()),
            _ => None,
        };
        match byte {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Имя пользователя для журнала изменений (заголовок `X-Timelist-User`, percent-encoding).
fn user_of(headers: &HeaderMap) -> String {
    let raw = headers
        .get("x-timelist-user")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    percent_decode(raw).trim().chars().take(60).collect()
}

fn constant_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn session_of(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(header::COOKIE)?.to_str().ok()?;
    cookies
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find(|(k, _)| *k == SESSION_COOKIE)
        .map(|(_, v)| v.to_string())
}

fn is_authed(state: &AppState, headers: &HeaderMap) -> bool {
    if !state.cfg.auth_required() {
        return true;
    }
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if let (Some(given), Some(token)) = (bearer, state.cfg.api_token.as_deref()) {
        if constant_eq(given.trim(), token) {
            return true;
        }
    }
    session_of(headers).is_some_and(|sid| {
        state
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&sid)
    })
}

async fn auth(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if is_authed(&state, request.headers()) {
        next.run(request).await
    } else {
        ApiError(StatusCode::UNAUTHORIZED, "требуется вход".into()).into_response()
    }
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/month/{ym}", get(month))
        .route("/day/{employee}/{date}", get(day))
        .route("/marks/{employee}/{date}", put(put_mark))
        .route("/months/{ym}/lock", post(lock_month).delete(unlock_month))
        .route("/months/{ym}/limit", put(set_limit))
        .route("/messages", get(messages).post(add_message))
        .route("/messages/import", post(import_text))
        .route(
            "/messages/{id}",
            patch(patch_message).delete(delete_message),
        )
        .route("/parse", post(parse))
        .route("/employees", get(employees).post(add_employee))
        .route(
            "/employees/{id}",
            put(update_employee).delete(delete_employee),
        )
        .route("/codes", get(codes))
        .route("/codes/{code}", put(update_code))
        .route("/settings", get(settings).put(update_settings))
        .route("/calendar/{year}", get(calendar))
        .route(
            "/calendar/{year}/norms/{month}",
            put(set_norm).delete(reset_norm),
        )
        .route("/calendar/days/{date}", put(set_day).delete(delete_day))
        .route("/periods", get(periods).post(add_period))
        .route("/periods/{id}", delete(delete_period))
        .route("/tg/users", get(tg_users))
        .route("/tg/users/{id}/link", post(link_user))
        .route("/audit", get(audit))
        .route("/export/{ym}", get(export))
        .route(
            "/import/xlsx",
            post(import_xlsx).layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route("/events", get(events))
        .layer(middleware::from_fn_with_state(state.clone(), auth));
    let api = protected
        .route("/meta", get(meta))
        .route("/login", post(login))
        .route("/logout", post(logout));
    Router::new()
        .nest("/api", api)
        .route("/", get(index))
        .route("/assets/{file}", get(asset))
        .with_state(state)
}

// ---------------------------------------------------------------- интерфейс

fn static_file(
    state: &AppState,
    name: &str,
    embedded: &'static str,
    mime: &'static str,
) -> Response {
    let body = state
        .cfg
        .assets_dir
        .as_ref()
        .and_then(|dir| std::fs::read_to_string(dir.join(name)).ok())
        .unwrap_or_else(|| embedded.to_string());
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

async fn index(State(state): State<AppState>) -> Response {
    static_file(&state, "index.html", INDEX_HTML, "text/html; charset=utf-8")
}

async fn asset(State(state): State<AppState>, Path(file): Path<String>) -> Response {
    match file.as_str() {
        "app.js" => static_file(&state, "app.js", APP_JS, "text/javascript; charset=utf-8"),
        "app.css" => static_file(&state, "app.css", APP_CSS, "text/css; charset=utf-8"),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---------------------------------------------------------------- вход

async fn meta(State(state): State<AppState>, headers: HeaderMap) -> Json<Value> {
    let authed = is_authed(&state, &headers);
    let now = state.now();
    let mut out = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "auth_required": state.cfg.auth_required(),
        "authed": authed,
        "today": now.date(),
    });
    if authed {
        out["now"] = json!(now.format("%Y-%m-%d %H:%M:%S").to_string());
        out["bot"] = json!(state.bot_status(|s| s.clone()));
        out["last_message_at"] = json!(state.db.last_message_at().ok().flatten());
        out["chat_ids"] = json!(state.cfg.chat_ids());
    }
    Json(out)
}

#[derive(Deserialize)]
struct Login {
    password: String,
}

async fn login(State(state): State<AppState>, Json(body): Json<Login>) -> ApiResult<Response> {
    let secrets = [
        state.cfg.password.as_deref(),
        state.cfg.api_token.as_deref(),
    ];
    if !secrets
        .iter()
        .flatten()
        .any(|s| constant_eq(s, body.password.trim()))
    {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "неверный пароль".into()));
    }
    let mut raw = [0u8; 24];
    getrandom::getrandom(&mut raw)
        .map_err(|e| anyhow::anyhow!("нет источника случайных чисел: {e}"))?;
    let sid: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    state
        .sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(sid.clone());
    let cookie =
        format!("{SESSION_COOKIE}={sid}; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000");
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response())
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(sid) = session_of(&headers) {
        state
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&sid);
    }
    let cookie = format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    ([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response()
}

// ---------------------------------------------------------------- месяц и отметки

fn month_json(state: &AppState, year: i32, month: u32) -> ApiResult<Response> {
    let engine = Engine::new(&state.db)?;
    Ok(Json(engine.month(year, month, state.now().date())?).into_response())
}

async fn month(State(state): State<AppState>, Path(ym): Path<String>) -> ApiResult<Response> {
    let (year, month) = parse_ym(&ym)?;
    month_json(&state, year, month)
}

async fn day(
    State(state): State<AppState>,
    Path((employee, date)): Path<(i64, String)>,
) -> ApiResult<Response> {
    let engine = Engine::new(&state.db)?;
    Ok(Json(engine.day_detail(employee, parse_date(&date)?, state.now().date())?).into_response())
}

async fn put_mark(
    State(state): State<AppState>,
    Path((employee, date)): Path<(i64, String)>,
    headers: HeaderMap,
    Json(edit): Json<MarkEdit>,
) -> ApiResult<Response> {
    let date = parse_date(&date)?;
    Engine::new(&state.db)?.edit_mark(employee, date, &edit, &user_of(&headers), state.now())?;
    state.notify();
    month_json(&state, date.year(), date.month())
}

async fn lock_month(
    State(state): State<AppState>,
    Path(ym): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let (year, month) = parse_ym(&ym)?;
    Engine::new(&state.db)?.lock_month(year, month, &user_of(&headers), state.now())?;
    state.notify();
    month_json(&state, year, month)
}

async fn unlock_month(
    State(state): State<AppState>,
    Path(ym): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let (year, month) = parse_ym(&ym)?;
    state.db.unlock_month(year, month)?;
    let at = state.now().format("%Y-%m-%d %H:%M:%S").to_string();
    state.db.add_audit(&NewAudit {
        at: &at,
        user: &user_of(&headers),
        employee_id: None,
        day: None,
        field: "month_lock",
        old: None,
        new: Some(&format!("{year}-{month:02} открыт")),
    })?;
    state.notify();
    month_json(&state, year, month)
}

#[derive(Deserialize)]
struct LimitBody {
    limit: f64,
}

async fn set_limit(
    State(state): State<AppState>,
    Path(ym): Path<String>,
    Json(body): Json<LimitBody>,
) -> ApiResult<Response> {
    let (year, month) = parse_ym(&ym)?;
    if !(body.limit > 0.0 && body.limit <= 1000.0) {
        return Err(ApiError::bad("лимит задаётся долей: 1 = 100%"));
    }
    state.db.set_month_limit(year, month, body.limit)?;
    state.notify();
    month_json(&state, year, month)
}

// ---------------------------------------------------------------- сообщения

#[derive(Deserialize)]
struct MessagesQuery {
    ym: String,
}

async fn messages(
    State(state): State<AppState>,
    Query(q): Query<MessagesQuery>,
) -> ApiResult<Json<Vec<MessageView>>> {
    let (year, month) = parse_ym(&q.ym)?;
    let (first, last) = month_bounds(year, month);
    Ok(Json(Engine::new(&state.db)?.messages(first, last)?))
}

#[derive(Deserialize)]
struct NewManualMessage {
    employee_id: i64,
    date: String,
    time: String,
    text: String,
}

async fn add_message(
    State(state): State<AppState>,
    Json(body): Json<NewManualMessage>,
) -> ApiResult<Json<Value>> {
    let date = parse_date(&body.date)?;
    let time = parse_time(&body.time, false)
        .ok_or_else(|| ApiError::bad("время указывается как ЧЧ:ММ"))?;
    let text = body.text.trim();
    if text.is_empty() {
        return Err(ApiError::bad("пустое сообщение"));
    }
    let employees = state.db.employees()?;
    let employee = employees
        .iter()
        .find(|e| e.id == body.employee_id)
        .ok_or_else(ApiError::not_found)?;
    let sent_at = date
        .and_hms_opt(time as u32 / 60, time as u32 % 60, 0)
        .ok_or_else(|| ApiError::bad("некорректное время"))?;
    let sender = if employee.tg_name.is_empty() {
        &employee.full_name
    } else {
        &employee.tg_name
    };
    let message = NewMessage {
        source: "manual",
        chat_id: None,
        tg_message_id: None,
        tg_user_id: None,
        sender_name: sender,
        employee_id: Some(employee.id),
        sent_at,
        text,
    };
    let (id, _) = state.db.upsert_message(&message)?;
    state.notify();
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
struct ImportText {
    text: String,
}

async fn import_text(
    State(state): State<AppState>,
    Json(body): Json<ImportText>,
) -> ApiResult<Json<Value>> {
    let (added, skipped) = excel::import_chat_text(&state.db, &body.text)?;
    state.notify();
    Ok(Json(json!({ "added": added, "skipped": skipped })))
}

#[derive(Deserialize)]
struct MessagePatch {
    ignored: Option<bool>,
    /// Привязать сообщение к сотруднику вручную; 0 — снять привязку.
    employee_id: Option<i64>,
}

async fn patch_message(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<MessagePatch>,
) -> ApiResult<Json<Value>> {
    if state.db.message(id)?.is_none() {
        return Err(ApiError::not_found());
    }
    if let Some(ignored) = body.ignored {
        state.db.set_message_ignored(id, ignored)?;
    }
    if let Some(employee_id) = body.employee_id {
        state
            .db
            .set_message_employee(id, (employee_id > 0).then_some(employee_id))?;
    }
    state.notify();
    Ok(Json(json!({ "ok": true })))
}

async fn delete_message(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let message = state.db.message(id)?.ok_or_else(ApiError::not_found)?;
    if message.source == "telegram" {
        return Err(ApiError::bad(
            "сообщения из Telegram не удаляются — отметьте его как игнорируемое",
        ));
    }
    state.db.delete_message(id)?;
    state.notify();
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ParseBody {
    text: String,
    date: Option<String>,
    time: Option<String>,
}

/// Проверка фразы: как бот поймёт сообщение.
async fn parse(
    State(state): State<AppState>,
    Json(body): Json<ParseBody>,
) -> ApiResult<Json<Value>> {
    let now = state.now();
    let date = body
        .date
        .as_deref()
        .map(parse_date)
        .transpose()?
        .unwrap_or(now.date());
    let time = body
        .time
        .as_deref()
        .and_then(|t| parse_time(t, false))
        .unwrap_or(now.format("%H").to_string().parse::<i32>().unwrap_or(9) * 60);
    let sent_at: NaiveDateTime = date
        .and_hms_opt(time as u32 / 60, time as u32 % 60, 0)
        .ok_or_else(|| ApiError::bad("некорректное время"))?;
    let engine = Engine::new(&state.db)?;
    let probe = Message {
        id: 0,
        source: "probe".into(),
        chat_id: None,
        tg_message_id: None,
        tg_user_id: None,
        sender_name: String::new(),
        employee_id: None,
        sent_at,
        text: body.text,
        edited: false,
        ignored: false,
    };
    let (view, parsed) = engine.interpret(&probe);
    let nick = |id: &i64| {
        engine
            .employees
            .iter()
            .find(|e| e.id == *id)
            .map(|e| e.nick.clone())
    };
    Ok(Json(json!({
        "kind": view.kind,
        "summary": view.summary,
        "code": parsed.code,
        "code_guessed": parsed.code_guessed,
        "time": parsed.time.map(fmt_hm),
        "dates": parsed.dates,
        "mentions": parsed.mentions.iter().filter_map(nick).collect::<Vec<_>>(),
        "include_author": parsed.include_author,
    })))
}

// ---------------------------------------------------------------- справочники

async fn employees(State(state): State<AppState>) -> ApiResult<Json<Vec<Employee>>> {
    Ok(Json(state.db.employees()?))
}

fn check_employee(e: &Employee) -> ApiResult<()> {
    if e.full_name.trim().is_empty() || e.nick.trim().is_empty() {
        return Err(ApiError::bad("укажите Ф.И.О. и ник"));
    }
    if let (Some(from), Some(to)) = (e.works_from, e.works_to) {
        if from > to {
            return Err(ApiError::bad("дата начала работы позже даты окончания"));
        }
    }
    Ok(())
}

async fn add_employee(
    State(state): State<AppState>,
    Json(body): Json<Employee>,
) -> ApiResult<Json<Value>> {
    check_employee(&body)?;
    let id = state.db.insert_employee(&body)?;
    state.notify();
    Ok(Json(json!({ "id": id })))
}

async fn update_employee(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(mut body): Json<Employee>,
) -> ApiResult<Json<Value>> {
    check_employee(&body)?;
    body.id = id;
    if !state.db.update_employee(&body)? {
        return Err(ApiError::not_found());
    }
    state.notify();
    Ok(Json(json!({ "ok": true })))
}

async fn delete_employee(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    if !state.db.delete_employee(id)? {
        return Err(ApiError::not_found());
    }
    state.notify();
    Ok(Json(json!({ "ok": true })))
}

async fn codes(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(state.db.codes()?)))
}

#[derive(Deserialize)]
struct CodeBody {
    coef: f64,
    title: Option<String>,
    description: Option<String>,
}

async fn update_code(
    State(state): State<AppState>,
    Path(code): Path<String>,
    Json(body): Json<CodeBody>,
) -> ApiResult<Json<Value>> {
    if !(0.0..=10.0).contains(&body.coef) {
        return Err(ApiError::bad("коэффициент должен быть от 0 до 10"));
    }
    if !state.db.update_code(
        &code,
        body.coef,
        body.title.as_deref(),
        body.description.as_deref(),
    )? {
        return Err(ApiError::not_found());
    }
    state.notify();
    Ok(Json(json!({ "ok": true })))
}

#[derive(Serialize, Deserialize)]
struct SettingsDto {
    work_start: String,
    work_end: String,
    lunch: String,
    limit: f64,
    utc_offset_minutes: i64,
    trip_use_times: bool,
}

async fn settings(State(state): State<AppState>) -> ApiResult<Json<SettingsDto>> {
    let s = state.db.settings()?;
    let minutes = |key: &str, default: i32| {
        fmt_hm(s.get(key).and_then(|v| v.parse().ok()).unwrap_or(default))
    };
    Ok(Json(SettingsDto {
        work_start: minutes("work_start", 540),
        work_end: minutes("work_end", 1080),
        lunch: minutes("lunch", 60),
        limit: s.get("limit").and_then(|v| v.parse().ok()).unwrap_or(1.0),
        utc_offset_minutes: s
            .get("utc_offset_minutes")
            .and_then(|v| v.parse().ok())
            .unwrap_or(300),
        trip_use_times: s.get("trip_use_times").is_some_and(|v| v == "1"),
    }))
}

async fn update_settings(
    State(state): State<AppState>,
    Json(body): Json<SettingsDto>,
) -> ApiResult<Json<Value>> {
    let time = |v: &str, what: &str| {
        parse_time(v, false)
            .ok_or_else(|| ApiError::bad(format!("{what}: укажите время как ЧЧ:ММ")))
    };
    let (start, end, lunch) = (
        time(&body.work_start, "начало дня")?,
        time(&body.work_end, "конец дня")?,
        time(&body.lunch, "обед")?,
    );
    if end - start - lunch <= 0 {
        return Err(ApiError::bad("рабочий день должен быть длиннее обеда"));
    }
    if body.limit.is_nan() || body.limit <= 0.0 || body.utc_offset_minutes.abs() > 14 * 60 {
        return Err(ApiError::bad("проверьте лимит и часовой пояс"));
    }
    let values = [
        ("work_start", start.to_string()),
        ("work_end", end.to_string()),
        ("lunch", lunch.to_string()),
        ("limit", body.limit.to_string()),
        ("utc_offset_minutes", body.utc_offset_minutes.to_string()),
        ("trip_use_times", (body.trip_use_times as u8).to_string()),
    ];
    for (key, value) in values {
        state.db.set_setting(key, &value)?;
    }
    state.notify();
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------- календарь

async fn calendar(State(state): State<AppState>, Path(year): Path<i32>) -> ApiResult<Json<Value>> {
    let (first, last) = (
        NaiveDate::from_ymd_opt(year, 1, 1),
        NaiveDate::from_ymd_opt(year, 12, 31),
    );
    let (Some(first), Some(last)) = (first, last) else {
        return Err(ApiError::bad("некорректный год"));
    };
    let days = state.db.calendar_days(first, last)?;
    let cal = Calendar::new(
        days.iter()
            .filter_map(|d| Special::parse(&d.kind).map(|k| (d.date, k))),
    );
    let day_hours = state.db.rules()?.day_length() as f64 / 60.0;
    let mut months = Vec::new();
    for month in 1..=12u32 {
        let (calc_days, calc_hours) = cal.month_norm(year, month, day_hours);
        let table = state.db.month_norm(year, month)?;
        months.push(json!({
            "month": month,
            "name": month_name(month),
            "hours": table.map_or(calc_hours, |t| t.0),
            "days": table.map_or(calc_days as i64, |t| t.1),
            "source": if table.is_some() { "table" } else { "calendar" },
            "calc_hours": calc_hours,
            "calc_days": calc_days,
        }));
    }
    Ok(Json(
        json!({ "year": year, "months": months, "days": days }),
    ))
}

#[derive(Deserialize)]
struct NormBody {
    hours: f64,
    days: i64,
}

async fn set_norm(
    State(state): State<AppState>,
    Path((year, month)): Path<(i32, u32)>,
    Json(body): Json<NormBody>,
) -> ApiResult<Json<Value>> {
    if !(1..=12).contains(&month)
        || !(body.hours > 0.0 && body.hours <= 744.0)
        || !(1..=31).contains(&body.days)
    {
        return Err(ApiError::bad("проверьте норму часов и дней"));
    }
    state
        .db
        .set_month_norm(year, month, Some((body.hours, body.days)))?;
    state.notify();
    Ok(Json(json!({ "ok": true })))
}

async fn reset_norm(
    State(state): State<AppState>,
    Path((year, month)): Path<(i32, u32)>,
) -> ApiResult<Json<Value>> {
    state.db.set_month_norm(year, month, None)?;
    state.notify();
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct DayBody {
    kind: String,
    #[serde(default)]
    title: String,
}

async fn set_day(
    State(state): State<AppState>,
    Path(date): Path<String>,
    Json(body): Json<DayBody>,
) -> ApiResult<Json<Value>> {
    let kind = Special::parse(&body.kind)
        .ok_or_else(|| ApiError::bad("тип дня: holiday, workday или short"))?;
    state
        .db
        .set_calendar_day(parse_date(&date)?, kind, &body.title)?;
    state.notify();
    Ok(Json(json!({ "ok": true })))
}

async fn delete_day(
    State(state): State<AppState>,
    Path(date): Path<String>,
) -> ApiResult<Json<Value>> {
    state.db.delete_calendar_day(parse_date(&date)?)?;
    state.notify();
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------- периоды отсутствия

#[derive(Deserialize)]
struct PeriodsQuery {
    year: i32,
}

async fn periods(
    State(state): State<AppState>,
    Query(q): Query<PeriodsQuery>,
) -> ApiResult<Json<Vec<Period>>> {
    let (first, last) = (
        NaiveDate::from_ymd_opt(q.year, 1, 1),
        NaiveDate::from_ymd_opt(q.year, 12, 31),
    );
    let (Some(first), Some(last)) = (first, last) else {
        return Err(ApiError::bad("некорректный год"));
    };
    Ok(Json(state.db.periods_between(first, last)?))
}

#[derive(Deserialize)]
struct NewPeriod {
    employee_ids: Vec<i64>,
    code: String,
    date_from: String,
    date_to: String,
    #[serde(default)]
    tariff: bool,
    #[serde(default)]
    note: String,
}

async fn add_period(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<NewPeriod>,
) -> ApiResult<Json<Value>> {
    let (from, to) = (parse_date(&body.date_from)?, parse_date(&body.date_to)?);
    if from > to || (to - from) > Duration::days(366) {
        return Err(ApiError::bad("проверьте даты периода"));
    }
    let code = body.code.trim().to_uppercase();
    let rules = state.db.rules()?;
    if rules.absence_coef(&code).is_none() {
        return Err(ApiError::bad(format!("нет кода «{code}»")));
    }
    if body.tariff && code != "К" {
        return Err(ApiError::bad(
            "тариф К1–К3 применяется только к командировке (К)",
        ));
    }
    if body.employee_ids.is_empty() {
        return Err(ApiError::bad("выберите сотрудников"));
    }
    let known: Vec<i64> = state.db.employees()?.iter().map(|e| e.id).collect();
    let at = state.now().format("%Y-%m-%d %H:%M:%S").to_string();
    let user = user_of(&headers);
    let mut ids = Vec::new();
    for employee_id in body.employee_ids {
        if !known.contains(&employee_id) {
            return Err(ApiError::not_found());
        }
        let period = Period {
            id: 0,
            employee_id,
            code: code.clone(),
            date_from: from,
            date_to: to,
            tariff: body.tariff,
            note: body.note.clone(),
        };
        ids.push(state.db.insert_period(&period)?);
        let text = format!(
            "{code}{} {from} — {to}",
            if body.tariff {
                " (тариф К1–К3)"
            } else {
                ""
            }
        );
        state.db.add_audit(&NewAudit {
            at: &at,
            user: &user,
            employee_id: Some(employee_id),
            day: None,
            field: "period",
            old: None,
            new: Some(&text),
        })?;
    }
    state.notify();
    Ok(Json(json!({ "ids": ids })))
}

async fn delete_period(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let period = state.db.period(id)?.ok_or_else(ApiError::not_found)?;
    state.db.delete_period(id)?;
    let at = state.now().format("%Y-%m-%d %H:%M:%S").to_string();
    let text = format!("{} {} — {}", period.code, period.date_from, period.date_to);
    state.db.add_audit(&NewAudit {
        at: &at,
        user: &user_of(&headers),
        employee_id: Some(period.employee_id),
        day: None,
        field: "period",
        old: Some(&text),
        new: None,
    })?;
    state.notify();
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------- Telegram

async fn tg_users(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let employees = state.db.employees()?;
    let norm = |s: &str| timelist_core::parser::normalize(s);
    let users: Vec<Value> = state
        .db
        .tg_users()?
        .into_iter()
        .map(|u| {
            // подсказка: сотрудник без привязки с таким же именем в Telegram
            let suggestion = employees
                .iter()
                .find(|e| {
                    e.tg_user_id.is_none()
                        && !e.tg_name.is_empty()
                        && norm(&e.tg_name) == norm(&u.name)
                })
                .map(|e| e.id);
            let mut v = json!(u);
            v["suggestion"] = json!(suggestion);
            v
        })
        .collect();
    Ok(Json(json!(users)))
}

#[derive(Deserialize)]
struct LinkBody {
    /// Сотрудник, к которому привязать пользователя; `null` — отвязать.
    employee_id: Option<i64>,
}

async fn link_user(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<LinkBody>,
) -> ApiResult<Json<Value>> {
    let employees = state.db.employees()?;
    match body.employee_id {
        Some(employee_id) => {
            if !employees.iter().any(|e| e.id == employee_id) {
                return Err(ApiError::not_found());
            }
            state.db.link_tg_user(employee_id, Some(id))?;
        }
        None => {
            if let Some(e) = employees.iter().find(|e| e.tg_user_id == Some(id)) {
                state.db.link_tg_user(e.id, None)?;
            }
        }
    }
    state.notify();
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct AuditQuery {
    employee: Option<i64>,
    date: Option<String>,
    limit: Option<i64>,
}

async fn audit(
    State(state): State<AppState>,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<Value>> {
    let date = q.date.as_deref().map(parse_date).transpose()?;
    Ok(Json(json!(state.db.audit(
        q.employee,
        date,
        q.limit.unwrap_or(200).clamp(1, 2000)
    )?)))
}

// ---------------------------------------------------------------- Excel

async fn export(State(state): State<AppState>, Path(ym): Path<String>) -> ApiResult<Response> {
    let (year, month) = parse_ym(ym.trim_end_matches(".xlsx"))?;
    let view = Engine::new(&state.db)?.month(year, month, state.now().date())?;
    let bytes = excel::export_month(&view)?;
    let headers = [
        (
            header::CONTENT_TYPE,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string(),
        ),
        (
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"TimeList-{year}-{month:02}.xlsx\""),
        ),
    ];
    Ok((headers, bytes).into_response())
}

async fn import_xlsx(State(state): State<AppState>, body: Bytes) -> ApiResult<Json<Value>> {
    if body.is_empty() {
        return Err(ApiError::bad("файл пуст"));
    }
    let report = excel::import_workbook(&state.db, body.to_vec())
        .map_err(|e| ApiError::bad(format!("{e:#}")))?;
    state.notify();
    Ok(Json(json!(report)))
}

// ---------------------------------------------------------------- события

async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // при переполнении канала тоже шлём событие — интерфейс просто перечитает данные
    let stream = BroadcastStream::new(state.changes.subscribe())
        .map(|r| Ok(Event::default().event("change").data(r.unwrap_or("change"))));
    Sse::new(stream).keep_alive(KeepAlive::default())
}
