//! Сборка месяца: сообщения + периоды + ручные правки → отметки, табель, сводка.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use chrono::{Duration, NaiveDate, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};
use timelist_core::automark::{
    day_issues, derive_day, AutoDay, DayInput, Event, Flag, Issue, IssueInput, PeriodDay, Severity,
};
use timelist_core::calc::{calc_day, calc_svodka, calc_tabel, DayCalc, Svodka, Tabel};
use timelist_core::calendar::{month_days, month_name, weekday_short, Calendar, DayKind, Special};
use timelist_core::model::Field;
use timelist_core::parser::{normalize, parse_message, Kind, Parsed, Person};
use timelist_core::timefmt::{fmt_hm, parse_time};
use timelist_core::{Mark, Minutes, Rules};

use crate::db::{
    month_bounds, AuditEntry, Db, Employee, Lock, MarkRow, Message, NewAudit, Period, ALL_FIELDS,
};

/// Насколько далеко от месяца ищем сообщения, которые могут к нему относиться
/// («17.09 уход 18:00» на следующий день, «отпуск с 12.11 по 20.11» заранее).
const MESSAGE_WINDOW_DAYS: i64 = 62;

#[derive(Debug)]
pub enum EditError {
    Locked,
    NotFound,
    Invalid(String),
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for EditError {
    fn from(e: E) -> Self {
        EditError::Internal(e.into())
    }
}

#[derive(Serialize)]
pub struct DayInfo {
    pub date: NaiveDate,
    pub dow: &'static str,
    pub kind: DayKind,
    pub title: String,
}

#[derive(Serialize)]
pub struct Cell {
    /// Итоговая отметка.
    pub m: Mark,
    /// Какие поля заданы вручную (биты в порядке колонок).
    pub mask: u8,
    /// Что предлагает автоматика, если отличается от итоговой отметки.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto: Option<Mark>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<Flag>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<Issue>,
    pub calc: DayCalc,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
    pub reviewed: bool,
    /// Сколько сообщений относится к этому дню.
    pub msgs: u32,
}

#[derive(Serialize)]
pub struct EmployeeMonth {
    pub id: i64,
    pub nick: String,
    pub full_name: String,
    pub auto_schedule: bool,
    pub cells: Vec<Cell>,
    pub tabel: Tabel,
    pub svodka: Svodka,
    pub issues: usize,
}

#[derive(Serialize)]
pub struct MonthView {
    pub year: i32,
    pub month: u32,
    pub title: String,
    pub today: NaiveDate,
    pub norm_hours: f64,
    pub norm_days: i64,
    /// `table` — норма задана в справочнике, `calendar` — рассчитана по календарю.
    pub norm_source: &'static str,
    pub limit: f64,
    pub lock: Option<Lock>,
    pub rules: Rules,
    pub days: Vec<DayInfo>,
    pub employees: Vec<EmployeeMonth>,
    pub issues: usize,
}

/// Сообщение с результатом разбора.
#[derive(Serialize)]
pub struct MessageView {
    pub id: i64,
    pub source: String,
    pub sent_at: NaiveDateTime,
    pub sender_name: String,
    pub tg_user_id: Option<i64>,
    /// Сотрудник-автор (по Telegram ID, имени или ручной привязке).
    pub author_id: Option<i64>,
    pub text: String,
    pub kind: Kind,
    pub code: Option<String>,
    /// Человекочитаемое толкование: «Приход 10:49 · К».
    pub summary: String,
    /// К кому и к каким дням отнесено.
    pub targets: Vec<Target>,
    pub recognized: bool,
    pub ignored: bool,
    pub edited: bool,
}

#[derive(Clone, Serialize)]
pub struct Target {
    pub employee_id: i64,
    pub date: NaiveDate,
    pub time: Option<Minutes>,
}

#[derive(Serialize)]
pub struct DayDetail {
    pub employee_id: i64,
    pub date: NaiveDate,
    pub auto: Mark,
    pub messages: Vec<MessageView>,
    pub periods: Vec<Period>,
    pub audit: Vec<AuditEntry>,
}

/// Правка отметки из интерфейса.
#[derive(Debug, Default, Deserialize)]
pub struct MarkEdit {
    /// Поля, заданные вручную: имя → значение («9:30», «Б») или null для пустого.
    #[serde(default)]
    pub set: BTreeMap<String, Option<String>>,
    /// Поля, которые нужно вернуть автоматике.
    #[serde(default)]
    pub reset: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub reviewed: Option<bool>,
}

pub struct Engine<'a> {
    db: &'a Db,
    pub employees: Vec<Employee>,
    people: Vec<Person>,
    pub rules: Rules,
    by_tg: HashMap<i64, i64>,
    names: Vec<(String, i64, bool)>,
    trip_use_times: bool,
    default_limit: f64,
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + (ca != cb) as usize).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

fn minutes_of(dt: NaiveDateTime) -> Minutes {
    (dt.hour() * 60 + dt.minute()) as Minutes
}

fn describe(p: &Parsed, time: Option<Minutes>) -> String {
    let label = match p.kind {
        Kind::Arrive => "Приход",
        Kind::Leave => "Уход",
        Kind::Away => "Выезд",
        Kind::Back => "Возврат",
        Kind::Late => "Опоздание",
        Kind::Absent => "Отсутствие",
        Kind::Info => "Без отметки",
        Kind::Unknown => "Не распознано",
    };
    let mut out = label.to_string();
    if let (Some(t), true) = (
        time,
        matches!(p.kind, Kind::Arrive | Kind::Leave | Kind::Away | Kind::Back),
    ) {
        out.push_str(&format!(" {}", fmt_hm(t)));
    }
    if let Some(code) = &p.code {
        out.push_str(&format!(" · {code}"));
    }
    out
}

impl<'a> Engine<'a> {
    pub fn new(db: &'a Db) -> Result<Self> {
        let employees = db.employees()?;
        let settings = db.settings()?;
        let people = employees
            .iter()
            .map(|e| Person {
                id: e.id,
                aliases: e.alias_list(),
            })
            .collect();
        let by_tg = employees
            .iter()
            .filter_map(|e| e.tg_user_id.map(|uid| (uid, e.id)))
            .collect();
        let names = employees
            .iter()
            .filter(|e| !e.tg_name.trim().is_empty())
            .map(|e| (normalize(&e.tg_name), e.id, e.tg_user_id.is_some()))
            .collect();
        Ok(Engine {
            db,
            employees,
            people,
            rules: db.rules()?,
            by_tg,
            names,
            trip_use_times: settings.get("trip_use_times").is_some_and(|v| v == "1"),
            default_limit: settings
                .get("limit")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1.0),
        })
    }

    /// Кому принадлежит сообщение: ручная привязка → Telegram ID → имя отправителя.
    pub fn author_of(&self, m: &Message) -> Option<i64> {
        if m.employee_id.is_some() {
            return m.employee_id;
        }
        if let Some(id) = m.tg_user_id.and_then(|uid| self.by_tg.get(&uid)) {
            return Some(*id);
        }
        // Сопоставление по имени: для импорта из текста всегда, для бота — только пока
        // у сотрудника не привязан Telegram ID (чужое имя не подменит привязанного).
        let name = normalize(m.sender_name.split(" в ответ ").next().unwrap_or(""));
        if name.is_empty() {
            return None;
        }
        let allowed = |linked: bool| m.tg_user_id.is_none() || !linked;
        if let Some((_, id, _)) = self
            .names
            .iter()
            .find(|(n, _, linked)| *n == name && allowed(*linked))
        {
            return Some(*id);
        }
        if m.tg_user_id.is_some() {
            return None;
        }
        // «Наталия Волкова» вместо «Наталья Волкова» — имя в чате записано чуть иначе, чем в справочнике
        let mut close = self
            .names
            .iter()
            .filter(|(n, _, _)| edit_distance(n, &name) <= 2);
        match (close.next(), close.next()) {
            (Some((_, id, _)), None) => Some(*id),
            _ => None,
        }
    }

    pub fn interpret(&self, m: &Message) -> (MessageView, Parsed) {
        let sent_date = m.sent_at.date();
        let sent_time = minutes_of(m.sent_at);
        let parsed = parse_message(&m.text, sent_date, sent_time, &self.people);
        let author = self.author_of(m);

        let mut who: Vec<i64> = Vec::new();
        if parsed.include_author {
            who.extend(author);
        }
        for id in &parsed.mentions {
            if !who.contains(id) {
                who.push(*id);
            }
        }

        // «Уход» после полуночи относится к предыдущему дню
        let night = parsed.dates.is_empty()
            && parsed.kind == Kind::Leave
            && parsed.time.is_none()
            && sent_time < 5 * 60;
        let dates = match (parsed.dates.is_empty(), night) {
            (false, _) => parsed.dates.clone(),
            (true, true) => vec![sent_date - Duration::days(1)],
            (true, false) => vec![sent_date],
        };
        let needs_time = matches!(
            parsed.kind,
            Kind::Arrive | Kind::Leave | Kind::Away | Kind::Back
        );
        // (день, время события); отметка задним числом без времени в тексте ни к чему не привязывается
        let mut slots: Vec<(NaiveDate, Option<Minutes>)> = Vec::new();
        for date in &dates {
            let time = parsed.time.or(if night {
                Some(23 * 60 + 59)
            } else if *date == sent_date {
                Some(sent_time)
            } else {
                None
            });
            if (needs_time && time.is_none()) || matches!(parsed.kind, Kind::Info | Kind::Unknown) {
                continue;
            }
            slots.push((*date, time));
        }
        let targets: Vec<Target> = slots
            .iter()
            .flat_map(|(date, time)| {
                who.iter().map(|id| Target {
                    employee_id: *id,
                    date: *date,
                    time: *time,
                })
            })
            .collect();
        let recognized = parsed.kind == Kind::Info || !targets.is_empty();
        let view = MessageView {
            id: m.id,
            source: m.source.clone(),
            sent_at: m.sent_at,
            sender_name: m.sender_name.clone(),
            tg_user_id: m.tg_user_id,
            author_id: author,
            text: m.text.clone(),
            kind: parsed.kind,
            code: parsed.code.clone(),
            summary: describe(&parsed, slots.first().and_then(|s| s.1)),
            targets,
            recognized,
            ignored: m.ignored,
            edited: m.edited,
        };
        (view, parsed)
    }

    fn calendar(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<(Calendar, HashMap<NaiveDate, String>)> {
        let days = self.db.calendar_days(from, to)?;
        let titles = days.iter().map(|d| (d.date, d.title.clone())).collect();
        let calendar = Calendar::new(
            days.iter()
                .filter_map(|d| Special::parse(&d.kind).map(|k| (d.date, k))),
        );
        Ok((calendar, titles))
    }

    /// Норма месяца: из справочника либо расчётная по календарю.
    pub fn norm(
        &self,
        year: i32,
        month: u32,
        calendar: &Calendar,
    ) -> Result<(f64, i64, &'static str)> {
        if let Some((hours, days)) = self.db.month_norm(year, month)? {
            return Ok((hours, days, "table"));
        }
        let (days, hours) = calendar.month_norm(year, month, self.rules.day_length() as f64 / 60.0);
        Ok((hours, days as i64, "calendar"))
    }

    pub fn messages(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<MessageView>> {
        Ok(self
            .db
            .messages_between(from, to)?
            .iter()
            .map(|m| self.interpret(m).0)
            .collect())
    }

    pub fn month(&self, year: i32, month: u32, today: NaiveDate) -> Result<MonthView> {
        self.month_inner(year, month, today, false)
    }

    fn month_inner(
        &self,
        year: i32,
        month: u32,
        today: NaiveDate,
        ignore_lock: bool,
    ) -> Result<MonthView> {
        let (first, last) = month_bounds(year, month);
        let dates = month_days(year, month);
        let (calendar, titles) = self.calendar(first, last)?;
        let (norm_hours, norm_days, norm_source) = self.norm(year, month, &calendar)?;
        let limit = self
            .db
            .month_limit(year, month)?
            .unwrap_or(self.default_limit);
        let lock = if ignore_lock {
            None
        } else {
            self.db.month_lock(year, month)?
        };

        // события и нераспознанные сообщения по (сотрудник, день)
        let window = Duration::days(MESSAGE_WINDOW_DAYS);
        let mut events: HashMap<(i64, NaiveDate), Vec<Event>> = HashMap::new();
        let mut unrecognized: HashMap<(i64, NaiveDate), usize> = HashMap::new();
        let mut counts: HashMap<(i64, NaiveDate), u32> = HashMap::new();
        let mut first_message: Option<NaiveDate> = None;
        for m in self.db.messages_between(first - window, last + window)? {
            let sent = m.sent_at.date();
            first_message = Some(first_message.map_or(sent, |d: NaiveDate| d.min(sent)));
            if m.ignored {
                continue;
            }
            let (view, parsed) = self.interpret(&m);
            for t in &view.targets {
                let key = (t.employee_id, t.date);
                *counts.entry(key).or_default() += 1;
                events.entry(key).or_default().push(Event {
                    kind: parsed.kind,
                    time: t.time,
                    code: parsed.code.clone(),
                    code_guessed: parsed.code_guessed,
                });
            }
            if let (false, Some(author)) = (view.recognized, view.author_id) {
                *unrecognized.entry((author, sent)).or_default() += 1;
                *counts.entry((author, sent)).or_default() += 1;
            }
        }
        let first_message = first_message.or(self.db.first_message_date()?);

        let periods = self.db.periods_between(first, last)?;
        let manual = self.db.marks_between(first, last)?;
        let snapshots = if lock.is_some() {
            self.db.snapshots_between(first, last)?
        } else {
            BTreeMap::new()
        };
        let trip_codes: Vec<&String> = self.rules.trip.keys().collect();

        let days: Vec<DayInfo> = dates
            .iter()
            .map(|d| DayInfo {
                date: *d,
                dow: weekday_short(*d),
                kind: calendar.kind(*d),
                title: titles.get(d).cloned().unwrap_or_default(),
            })
            .collect();

        let mut employees = Vec::new();
        let mut issues_total = 0;
        for e in &self.employees {
            let has_data = |id: i64| {
                manual.keys().any(|(emp, _)| *emp == id)
                    || snapshots.keys().any(|(emp, _)| *emp == id)
                    || events
                        .keys()
                        .any(|(emp, d)| *emp == id && *d >= first && *d <= last)
            };
            let employed =
                e.works_from.is_none_or(|d| d <= last) && e.works_to.is_none_or(|d| d >= first);
            if !(e.active && employed) && !has_data(e.id) {
                continue;
            }

            let mut cells = Vec::new();
            let mut calcs = Vec::new();
            let mut issues_count = 0;
            for day in &days {
                let key = (e.id, day.date);
                let row = manual.get(&key).cloned().unwrap_or_default();
                let workday = day.kind.is_workday();
                let closed = day.date < today;
                let cell = if lock.is_some() {
                    let m = snapshots.get(&key).cloned().unwrap_or_default();
                    let calc = calc_day(&m, &self.rules);
                    Cell {
                        m,
                        mask: 0,
                        auto: None,
                        flags: vec![],
                        issues: vec![],
                        calc,
                        note: row.note,
                        reviewed: row.reviewed,
                        msgs: counts.get(&key).copied().unwrap_or(0),
                    }
                } else {
                    let period = periods
                        .iter()
                        .filter(|p| {
                            p.employee_id == e.id
                                && p.date_from <= day.date
                                && day.date <= p.date_to
                        })
                        .max_by_key(|p| p.id)
                        .map(|p| PeriodDay {
                            code: p.code.clone(),
                            day_code: (p.tariff && trip_codes.len() >= 3).then(|| {
                                let i = if day.date == p.date_from || day.date == p.date_to {
                                    0
                                } else if workday {
                                    1
                                } else {
                                    2
                                };
                                trip_codes[i].clone()
                            }),
                        });
                    let input = DayInput {
                        events: events.get(&key).map(Vec::as_slice).unwrap_or(&[]),
                        period: period.as_ref(),
                        workday,
                        closed,
                        future: day.date > today,
                        auto_schedule: e.auto_schedule,
                        trip_use_times: self.trip_use_times,
                    };
                    let auto: AutoDay = derive_day(&input, &self.rules);
                    let m = auto.mark.overlay(&row.mark, row.manual_mask);
                    let expected = e.active
                        && first_message.is_some_and(|d| d <= day.date)
                        && e.works_from.is_none_or(|d| d <= day.date)
                        && e.works_to.is_none_or(|d| d >= day.date);
                    let issues = day_issues(
                        &IssueInput {
                            effective: &m,
                            auto: &auto,
                            manual_mask: row.manual_mask,
                            workday,
                            closed,
                            reviewed: row.reviewed,
                            unrecognized: unrecognized.get(&key).copied().unwrap_or(0),
                            expected,
                        },
                        &self.rules,
                    );
                    let calc = calc_day(&m, &self.rules);
                    Cell {
                        auto: (auto.mark != m).then_some(auto.mark),
                        m,
                        mask: row.manual_mask,
                        flags: auto.flags,
                        issues,
                        calc,
                        note: row.note,
                        reviewed: row.reviewed,
                        msgs: counts.get(&key).copied().unwrap_or(0),
                    }
                };
                issues_count += cell.issues.len();
                calcs.push(cell.calc);
                cells.push(cell);
            }
            let tabel = calc_tabel(&calcs, norm_hours);
            let svodka = calc_svodka(&tabel, norm_hours, limit);
            issues_total += issues_count;
            employees.push(EmployeeMonth {
                id: e.id,
                nick: e.nick.clone(),
                full_name: e.full_name.clone(),
                auto_schedule: e.auto_schedule,
                cells,
                tabel,
                svodka,
                issues: issues_count,
            });
        }

        Ok(MonthView {
            year,
            month,
            title: format!("{} {}", month_name(month), year),
            today,
            norm_hours,
            norm_days,
            norm_source,
            limit,
            lock,
            rules: self.rules.clone(),
            days,
            employees,
            issues: issues_total,
        })
    }

    pub fn day_detail(
        &self,
        employee_id: i64,
        date: NaiveDate,
        today: NaiveDate,
    ) -> Result<DayDetail> {
        use chrono::Datelike;
        let window = Duration::days(MESSAGE_WINDOW_DAYS);
        let messages = self
            .messages(date - window, date + window)?
            .into_iter()
            .filter(|m| {
                m.targets
                    .iter()
                    .any(|t| t.employee_id == employee_id && t.date == date)
                    || (m.author_id == Some(employee_id) && m.sent_at.date() == date)
            })
            .collect();
        // автоматическая отметка без учёта ручных правок и закрытия месяца
        let view = self.month_inner(date.year(), date.month(), today, true)?;
        let auto = view
            .employees
            .into_iter()
            .find(|e| e.id == employee_id)
            .and_then(|e| e.cells.into_iter().nth(date.day0() as usize))
            .map(|c| c.auto.unwrap_or(c.m))
            .unwrap_or_default();
        Ok(DayDetail {
            employee_id,
            date,
            auto,
            messages,
            periods: self
                .db
                .periods_between(date, date)?
                .into_iter()
                .filter(|p| p.employee_id == employee_id)
                .collect(),
            audit: self.db.audit(Some(employee_id), Some(date), 50)?,
        })
    }

    fn effective(&self, employee_id: i64, date: NaiveDate, today: NaiveDate) -> Result<Mark> {
        use chrono::Datelike;
        let view = self.month(date.year(), date.month(), today)?;
        Ok(view
            .employees
            .into_iter()
            .find(|e| e.id == employee_id)
            .and_then(|e| e.cells.into_iter().nth(date.day0() as usize))
            .map(|c| c.m)
            .unwrap_or_default())
    }

    /// Применяет ручную правку и пишет журнал изменений.
    pub fn edit_mark(
        &self,
        employee_id: i64,
        date: NaiveDate,
        edit: &MarkEdit,
        user: &str,
        now: NaiveDateTime,
    ) -> Result<(), EditError> {
        use chrono::Datelike;
        if !self.employees.iter().any(|e| e.id == employee_id) {
            return Err(EditError::NotFound);
        }
        if self.db.month_lock(date.year(), date.month())?.is_some() {
            return Err(EditError::Locked);
        }
        let today = now.date();
        let before = self.effective(employee_id, date, today)?;
        let mut row: MarkRow = self.db.mark_row(employee_id, date)?;
        let old_note = row.note.clone();
        let old_reviewed = row.reviewed;

        for (name, raw) in &edit.set {
            let field = Field::from_name(name)
                .ok_or_else(|| EditError::Invalid(format!("неизвестное поле «{name}»")))?;
            let raw = raw.as_deref().map(str::trim).filter(|v| !v.is_empty());
            if field.is_time() {
                let value = match raw {
                    None => None,
                    Some(v) => Some(parse_time(v, field == Field::Away).ok_or_else(|| {
                        EditError::Invalid(format!("«{v}» — не время; введите, например, 9:30"))
                    })?),
                };
                match field {
                    Field::Start => row.mark.start = value,
                    Field::Away => row.mark.away = value,
                    _ => row.mark.end = value,
                }
            } else {
                let value = raw.map(|v| v.to_uppercase());
                if let Some(code) = &value {
                    let known = if field == Field::DayCode {
                        self.rules.trip_coef(code).is_some()
                    } else {
                        self.rules.absence_coef(code).is_some()
                    };
                    if !known {
                        let allowed: Vec<&str> = if field == Field::DayCode {
                            self.rules.trip.keys().map(String::as_str).collect()
                        } else {
                            self.rules.absence.keys().map(String::as_str).collect()
                        };
                        return Err(EditError::Invalid(format!(
                            "нет кода «{code}»; допустимые: {}",
                            allowed.join(", ")
                        )));
                    }
                }
                match field {
                    Field::StartCode => row.mark.start_code = value,
                    Field::AwayCode => row.mark.away_code = value,
                    Field::EndCode => row.mark.end_code = value,
                    _ => row.mark.day_code = value,
                }
            }
            row.manual_mask |= field.bit();
        }
        for name in &edit.reset {
            if name == "all" {
                row.manual_mask = 0;
                continue;
            }
            let field = Field::from_name(name)
                .ok_or_else(|| EditError::Invalid(format!("неизвестное поле «{name}»")))?;
            row.manual_mask &= !field.bit();
        }
        if let Some(note) = &edit.note {
            row.note = note.trim().to_string();
        }
        if let Some(reviewed) = edit.reviewed {
            row.reviewed = reviewed;
        }
        self.db.save_mark_row(employee_id, date, &row)?;

        let after = self.effective(employee_id, date, today)?;
        let at = now.format("%Y-%m-%d %H:%M:%S").to_string();
        let show = |m: &Mark, f: Field| -> Option<String> {
            match f {
                Field::Start => m.start.map(fmt_hm),
                Field::Away => m.away.map(fmt_hm),
                Field::End => m.end.map(fmt_hm),
                Field::StartCode => m.start_code.clone(),
                Field::AwayCode => m.away_code.clone(),
                Field::EndCode => m.end_code.clone(),
                Field::DayCode => m.day_code.clone(),
            }
        };
        for f in Field::ALL {
            let (old, new) = (show(&before, f), show(&after, f));
            if old != new {
                self.db.add_audit(&NewAudit {
                    at: &at,
                    user,
                    employee_id: Some(employee_id),
                    day: Some(date),
                    field: f.name(),
                    old: old.as_deref(),
                    new: new.as_deref(),
                })?;
            }
        }
        if old_note != row.note {
            self.db.add_audit(&NewAudit {
                at: &at,
                user,
                employee_id: Some(employee_id),
                day: Some(date),
                field: "note",
                old: Some(&old_note),
                new: Some(&row.note),
            })?;
        }
        if old_reviewed != row.reviewed {
            let text = |v: bool| {
                if v {
                    "проверено"
                } else {
                    "не проверено"
                }
            };
            self.db.add_audit(&NewAudit {
                at: &at,
                user,
                employee_id: Some(employee_id),
                day: Some(date),
                field: "reviewed",
                old: Some(text(old_reviewed)),
                new: Some(text(row.reviewed)),
            })?;
        }
        Ok(())
    }

    /// Закрывает месяц, замораживая итоговые отметки. Ошибки расчёта нужно исправить до закрытия.
    pub fn lock_month(
        &self,
        year: i32,
        month: u32,
        user: &str,
        now: NaiveDateTime,
    ) -> Result<(), EditError> {
        let view = self.month(year, month, now.date())?;
        if view.lock.is_some() {
            return Ok(());
        }
        let errors: usize = view
            .employees
            .iter()
            .flat_map(|e| e.cells.iter())
            .map(|c| {
                c.issues
                    .iter()
                    .filter(|i| i.severity == Severity::Error)
                    .count()
            })
            .sum();
        if errors > 0 {
            return Err(EditError::Invalid(format!(
                "в месяце есть ошибки ({errors}) — исправьте их перед закрытием"
            )));
        }
        let mut frozen = Vec::new();
        for e in &view.employees {
            for (cell, day) in e.cells.iter().zip(&view.days) {
                if !cell.m.is_empty() {
                    frozen.push((e.id, day.date, cell.m.clone()));
                }
            }
        }
        let at = now.format("%Y-%m-%d %H:%M:%S").to_string();
        self.db.lock_month(year, month, &at, user, &frozen)?;
        self.db.add_audit(&NewAudit {
            at: &at,
            user,
            employee_id: None,
            day: None,
            field: "month_lock",
            old: None,
            new: Some(&format!("{year}-{month:02} закрыт")),
        })?;
        Ok(())
    }
}

/// Записывает отметки как заданные вручную целиком (импорт из Excel).
pub fn import_manual_day(db: &Db, employee_id: i64, date: NaiveDate, mark: Mark) -> Result<()> {
    let mut row = db.mark_row(employee_id, date)?;
    row.mark = mark;
    row.manual_mask = ALL_FIELDS;
    db.save_mark_row(employee_id, date, &row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::NewMessage;

    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }

    fn at(m: u32, day: u32, h: u32, min: u32) -> NaiveDateTime {
        d(m, day).and_hms_opt(h, min, 0).unwrap()
    }

    fn employee(nick: &str, tg_name: &str) -> Employee {
        Employee {
            id: 0,
            full_name: format!("{tg_name} (тест)"),
            nick: nick.into(),
            tg_name: tg_name.into(),
            tg_user_id: None,
            aliases: String::new(),
            auto_schedule: false,
            active: true,
            sort: 0,
            works_from: None,
            works_to: None,
        }
    }

    fn say(db: &Db, name: &str, when: NaiveDateTime, text: &str) {
        let m = NewMessage {
            source: "import",
            chat_id: None,
            tg_message_id: None,
            tg_user_id: None,
            sender_name: name,
            employee_id: None,
            sent_at: when,
            text,
        };
        db.upsert_message(&m).unwrap();
    }

    fn setup() -> (Db, i64, i64) {
        let db = Db::in_memory().unwrap();
        let a = db.insert_employee(&employee("ОлС", "Олег Сомов")).unwrap();
        let b = db
            .insert_employee(&employee("ТиР", "Тимур Раевский"))
            .unwrap();
        (db, a, b)
    }

    fn cell(view: &MonthView, employee_id: i64, day: u32) -> &Cell {
        &view
            .employees
            .iter()
            .find(|e| e.id == employee_id)
            .unwrap()
            .cells[day as usize - 1]
    }

    #[test]
    fn messages_become_marks_and_totals() {
        let (db, a, b) = setup();
        say(
            &db,
            "Олег Сомов",
            at(10, 2, 9, 29),
            "Задержусь в поликлинике",
        );
        say(&db, "Олег Сомов", at(10, 2, 10, 49), "Офис");
        say(&db, "Олег Сомов", at(10, 2, 18, 9), "Уход");
        say(&db, "Тимур Раевский", at(10, 2, 8, 50), "Офис");
        say(
            &db,
            "Тимур Раевский",
            at(10, 2, 17, 30),
            "Уход из аэропорта я и Олег",
        );
        let view = Engine::new(&db).unwrap().month(2026, 10, d(10, 5)).unwrap();
        assert_eq!(
            (view.norm_hours, view.norm_days, view.norm_source),
            (168.0, 21, "table")
        );
        assert_eq!(view.days[0].kind, DayKind::Holiday);

        let c = cell(&view, a, 2);
        // «Уход … я и Олег» от коллеги в 17:30 раньше собственного «Уход» в 18:09 — берётся последний
        assert_eq!(
            (c.m.start, c.m.start_code.as_deref(), c.m.end),
            (Some(649), Some("Б"), Some(1089))
        );
        assert_eq!(c.calc.worked, 380.0);
        assert!((c.calc.calc - 467.2).abs() < 1e-9);
        let c = cell(&view, b, 2);
        assert_eq!(
            (c.m.start, c.m.end, c.m.end_code.as_deref()),
            (Some(530), Some(1050), Some("К"))
        );
        assert_eq!(c.calc.early_calc, 0.0);
        let total = &view.employees.iter().find(|e| e.id == a).unwrap().tabel;
        assert_eq!((total.late_n, total.late_n_calc), (1, 1));
    }

    #[test]
    fn manual_edit_overrides_auto_and_is_audited() {
        let (db, a, _) = setup();
        say(&db, "Олег Сомов", at(10, 6, 9, 40), "Офис");
        let engine = Engine::new(&db).unwrap();
        let now = at(10, 8, 12, 0);
        let view = engine.month(2026, 10, now.date()).unwrap();
        let c = cell(&view, a, 6);
        assert_eq!(c.m.end, Some(1080));
        assert_eq!(c.issues.len(), 1);

        let edit = MarkEdit {
            set: [
                ("end".to_string(), Some("17.45".to_string())),
                ("end_code".to_string(), Some("ф".to_string())),
            ]
            .into(),
            ..MarkEdit::default()
        };
        engine
            .edit_mark(a, d(10, 6), &edit, "Табельщик", now)
            .unwrap();
        let view = engine.month(2026, 10, now.date()).unwrap();
        let c = cell(&view, a, 6);
        assert_eq!(
            (c.m.end, c.m.end_code.as_deref(), c.mask),
            (
                Some(1065),
                Some("Ф"),
                Field::End.bit() | Field::EndCode.bit()
            )
        );
        assert!(c.issues.is_empty());
        assert_eq!(c.auto.as_ref().unwrap().end, Some(1080));
        let audit = db.audit(Some(a), Some(d(10, 6)), 10).unwrap();
        assert_eq!(audit.len(), 2);
        assert_eq!(audit[0].user, "Табельщик");

        // неверный код отклоняется
        let bad = MarkEdit {
            set: [("start_code".to_string(), Some("Я".to_string()))].into(),
            ..MarkEdit::default()
        };
        assert!(matches!(
            engine.edit_mark(a, d(10, 6), &bad, "", now),
            Err(EditError::Invalid(_))
        ));

        // возврат к автоматике
        let reset = MarkEdit {
            reset: vec!["all".into()],
            ..MarkEdit::default()
        };
        engine
            .edit_mark(a, d(10, 6), &reset, "Табельщик", now)
            .unwrap();
        let view = engine.month(2026, 10, now.date()).unwrap();
        assert_eq!(
            (cell(&view, a, 6).m.end, cell(&view, a, 6).mask),
            (Some(1080), 0)
        );
    }

    #[test]
    fn periods_fill_working_days() {
        let (db, a, b) = setup();
        say(&db, "Тимур Раевский", at(10, 1, 9, 0), "Офис");
        let period = |employee_id, code: &str, from, to, tariff| Period {
            id: 0,
            employee_id,
            code: code.into(),
            date_from: from,
            date_to: to,
            tariff,
            note: String::new(),
        };
        db.insert_period(&period(a, "О", d(10, 5), d(10, 11), false))
            .unwrap();
        db.insert_period(&period(b, "К", d(10, 8), d(10, 12), true))
            .unwrap();
        let view = Engine::new(&db)
            .unwrap()
            .month(2026, 10, d(10, 20))
            .unwrap();
        // отпуск: Пн–Пт помечены, выходные нет
        assert_eq!(cell(&view, a, 5).m.start_code.as_deref(), Some("О"));
        assert_eq!(cell(&view, a, 9).m.start_code.as_deref(), Some("О"));
        assert!(cell(&view, a, 10).m.is_empty());
        assert_eq!(cell(&view, a, 9).calc.calc, 480.0);
        // командировка с тарифом: К1 в дни дороги, К2 в рабочие, К3 в выходные
        let codes: Vec<_> = (8..=12)
            .map(|day| cell(&view, b, day).m.day_code.clone().unwrap())
            .collect();
        assert_eq!(codes, ["К1", "К2", "К3", "К3", "К1"]);
        assert_eq!(cell(&view, b, 9).calc.calc, 600.0);
    }

    #[test]
    fn locked_month_is_frozen() {
        let (db, a, _) = setup();
        say(&db, "Олег Сомов", at(10, 6, 9, 0), "Офис");
        say(&db, "Олег Сомов", at(10, 6, 18, 0), "Уход");
        let engine = Engine::new(&db).unwrap();
        let now = at(11, 2, 10, 0);
        engine.lock_month(2026, 10, "Табельщик", now).unwrap();
        // запоздалое сообщение и правки закрытый месяц не меняют
        say(&db, "Олег Сомов", at(11, 2, 10, 5), "06.10 уход 19:30");
        let view = engine.month(2026, 10, now.date()).unwrap();
        assert!(view.lock.is_some());
        assert_eq!(cell(&view, a, 6).m.end, Some(1080));
        let edit = MarkEdit {
            set: [("end".to_string(), Some("17:00".to_string()))].into(),
            ..MarkEdit::default()
        };
        assert!(matches!(
            engine.edit_mark(a, d(10, 6), &edit, "", now),
            Err(EditError::Locked)
        ));
        db.unlock_month(2026, 10).unwrap();
        assert_eq!(
            cell(&engine.month(2026, 10, now.date()).unwrap(), a, 6)
                .m
                .end,
            Some(1170)
        );
    }

    #[test]
    fn sender_matching() {
        let (db, a, _) = setup();
        let engine = Engine::new(&db).unwrap();
        let msg = |name: &str, uid: Option<i64>| Message {
            id: 1,
            source: "import".into(),
            chat_id: None,
            tg_message_id: None,
            tg_user_id: uid,
            sender_name: name.into(),
            employee_id: None,
            sent_at: at(10, 2, 9, 0),
            text: "Офис".into(),
            edited: false,
            ignored: false,
        };
        assert_eq!(engine.author_of(&msg("Олег Сомов", None)), Some(a));
        assert_eq!(
            engine.author_of(&msg("Олег Сомов в ответ Олег Сомов", None)),
            Some(a)
        );
        assert_eq!(engine.author_of(&msg("Олег Сoмов", None)), Some(a)); // латинская «o» — опечатка
        assert_eq!(engine.author_of(&msg("Кто-то Другой", None)), None);
        // сообщение без сотрудника разбирается, но ни к кому не относится
        let (view, _) = engine.interpret(&msg("Кто-то Другой", None));
        assert_eq!(
            (view.summary.as_str(), view.recognized, view.targets.len()),
            ("Приход 9:00", false, 0)
        );
        // бот: пока ID не привязан, сопоставляем по имени
        assert_eq!(engine.author_of(&msg("Олег Сомов", Some(77))), Some(a));
        db.link_tg_user(a, Some(555)).unwrap();
        let engine = Engine::new(&db).unwrap();
        assert_eq!(engine.author_of(&msg("Любое имя", Some(555))), Some(a));
        // чужой аккаунт с тем же именем привязанного сотрудника не подменяет
        assert_eq!(engine.author_of(&msg("Олег Сомов", Some(77))), None);
    }
}
