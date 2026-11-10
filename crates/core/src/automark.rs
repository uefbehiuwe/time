//! Автоматическая расстановка отметок дня.
//!
//! То, что раньше делал табельщик вручную, глядя на список сообщений сотрудника:
//! первое «офис» → «Начало», последнее «уход» → «Конец», причина из сообщения →
//! код категории, период отпуска/больничного/командировки → код на весь день.
//! Всё сомнительное не скрывается, а возвращается как [`Issue`] — список «на проверку».

use serde::Serialize;

use crate::model::{Field, Mark, Minutes, Rules};
use crate::parser::Kind;

/// Событие дня одного сотрудника (сообщение уже разобрано и отнесено к дате).
#[derive(Clone, Debug)]
pub struct Event {
    pub kind: Kind,
    pub time: Option<Minutes>,
    pub code: Option<String>,
    pub code_guessed: bool,
}

/// Период отсутствия, покрывающий день.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeriodDay {
    /// Категория: О, У, Б, К, Ф, Г, П.
    pub code: String,
    /// Тариф иногородней командировки на этот день (К1/К2/К3), если он применяется.
    pub day_code: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct DayInput<'a> {
    pub events: &'a [Event],
    pub period: Option<&'a PeriodDay>,
    /// Рабочий день по производственному календарю.
    pub workday: bool,
    /// День уже закончился.
    pub closed: bool,
    /// День ещё не наступил.
    pub future: bool,
    /// Сотрудник не отмечается в Telegram — в рабочие дни ставится полный день.
    pub auto_schedule: bool,
    /// В командировке с тарифом К1–К3 учитывать время из сообщений (иначе только код: 8 ч × тариф).
    pub trip_use_times: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Flag {
    /// «Конец» не отмечен, подставлен конец рабочего дня.
    EndAssumed,
    /// Код причины подставлен по умолчанию.
    CodeGuessed,
    /// Было предупреждение об опоздании, но прихода нет.
    LateNoShow,
    /// День проставлен по графику (сотрудник не отмечается в Telegram).
    Scheduled,
    /// Код взят из периода отсутствия.
    Period,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AutoDay {
    pub mark: Mark,
    pub flags: Vec<Flag>,
}

const TRIP: &str = "К";
const FORCE_MAJEURE: &str = "Ф";

/// Строит отметку дня из событий, периода и графика.
pub fn derive_day(input: &DayInput, rules: &Rules) -> AutoDay {
    let mut m = Mark::default();
    let mut flags = Vec::new();

    let mut events: Vec<&Event> = input.events.iter().collect();
    events.sort_by_key(|e| e.time.unwrap_or(-1));

    let mut start: Option<(Minutes, Option<String>)> = None;
    let mut end: Option<(Minutes, Option<String>)> = None;
    let mut open_away: Option<(Minutes, Option<String>)> = None;
    let mut away_total = 0;
    let mut away_code: Option<String> = None;
    let mut late: Option<Option<String>> = None;
    let mut absent: Option<(Option<String>, bool)> = None;

    for e in events {
        match (e.kind, e.time) {
            (Kind::Late, _) => {
                let code = e.code.clone().or_else(|| late.take().flatten());
                late = Some(code);
            }
            (Kind::Absent, _) => absent = Some((e.code.clone(), e.code_guessed)),
            (Kind::Arrive | Kind::Back, Some(t)) => {
                if start.is_none() {
                    if e.code_guessed {
                        flags.push(Flag::CodeGuessed);
                    }
                    start = Some((t, e.code.clone()));
                } else if let Some((from, code)) = open_away.take() {
                    // вернулся с выезда
                    if t > from {
                        away_total += t - from;
                        away_code = code.or(away_code);
                    }
                } else if let Some((from, code)) = end.take() {
                    // «уход», а потом снова «офис» — это отлучка, а не конец дня
                    if t > from {
                        away_total += t - from;
                        away_code = code.or(away_code);
                    } else {
                        end = Some((from, code));
                    }
                }
            }
            (Kind::Away, Some(t)) => {
                if start.is_none() {
                    // день начался вне офиса
                    start = Some((t, e.code.clone().or_else(|| Some(TRIP.to_string()))));
                } else {
                    open_away = Some((t, e.code.clone()));
                }
            }
            (Kind::Leave, Some(t)) => {
                // ушёл домой прямо с выезда — отлучки нет, уход с кодом выезда
                let code = match open_away.take() {
                    Some((_, away)) => e.code.clone().or(away).or_else(|| Some(TRIP.to_string())),
                    None => e.code.clone(),
                };
                end = Some((t, code));
            }
            _ => {}
        }
    }
    // выехал и не вернулся: рабочий день в офисе закончился на выезде
    if let (Some((t, code)), None) = (open_away, &end) {
        end = Some((t, code.or_else(|| Some(TRIP.to_string()))));
    }

    if let Some((t, code)) = start {
        m.start = Some(t);
        m.start_code = code;
        if m.start_code.is_none() && t > rules.work_start {
            if let Some(Some(code)) = &late {
                m.start_code = Some(code.clone());
            } else if let Some((code, guessed)) = &absent {
                m.start_code = code.clone();
                if *guessed {
                    flags.push(Flag::CodeGuessed);
                }
            }
        }
    }
    if let Some((t, code)) = end {
        if m.start.is_none_or(|s| t > s) {
            m.end = Some(t);
            m.end_code = code;
        }
    }
    if away_total > 0 {
        m.away = Some(away_total);
        m.away_code = away_code;
    }

    if m.start.is_none() && m.end.is_none() && input.workday {
        if let Some((code, guessed)) = absent {
            m.start_code = code.or_else(|| Some(FORCE_MAJEURE.to_string()));
            if guessed {
                flags.push(Flag::CodeGuessed);
            }
        } else if let (Some(code), true) = (late, input.closed) {
            flags.push(Flag::LateNoShow);
            if code.is_none() {
                flags.push(Flag::CodeGuessed);
            }
            m.start_code = code.or_else(|| Some(FORCE_MAJEURE.to_string()));
        }
    }

    if let (Some(s), None, true) = (m.start, m.end, input.closed) {
        if s < rules.work_end {
            m.end = Some(rules.work_end);
            flags.push(Flag::EndAssumed);
        }
    }

    if let Some(p) = input.period {
        flags.push(Flag::Period);
        match &p.day_code {
            Some(day_code) => {
                m.day_code = Some(day_code.clone());
                if input.trip_use_times {
                    // тариф уже учитывает командировку, отдельный код «К» не нужен
                    for code in [&mut m.start_code, &mut m.end_code] {
                        if code.as_deref() == Some(TRIP) {
                            *code = None;
                        }
                    }
                } else {
                    m = Mark {
                        day_code: m.day_code,
                        ..Mark::default()
                    };
                    flags.retain(|f| *f == Flag::Period);
                }
            }
            None => {
                let present = m.start.is_some() || m.end.is_some();
                if present {
                    // работа во время отпуска/больничного: время остаётся, причина — из периода
                    if m.start_code.is_none() {
                        m.start_code = Some(p.code.clone());
                    }
                } else if input.workday {
                    m.start_code = Some(p.code.clone());
                    flags.retain(|f| *f == Flag::Period);
                }
            }
        }
    }

    if m.is_empty()
        && input.auto_schedule
        && input.workday
        && !input.future
        && input.period.is_none()
    {
        m.start = Some(rules.work_start);
        m.end = Some(rules.work_end);
        flags.push(Flag::Scheduled);
    }

    AutoDay { mark: m, flags }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    NoStart,
    NoEnd,
    UnknownCode,
    EndAssumed,
    LateNoShow,
    CodeGuessed,
    Unrecognized,
    NoMarks,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Искажает расчёт — нужно исправить.
    Error,
    /// Автоматика сомневается — достаточно подтвердить или поправить.
    Warn,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Issue {
    pub kind: IssueKind,
    pub severity: Severity,
    pub text: String,
}

pub struct IssueInput<'a> {
    /// Итоговая отметка (автоматика + ручные правки).
    pub effective: &'a Mark,
    pub auto: &'a AutoDay,
    /// Битовая маска полей, заданных вручную.
    pub manual_mask: u8,
    pub workday: bool,
    pub closed: bool,
    /// Табельщик отметил день как проверенный.
    pub reviewed: bool,
    /// Сколько сообщений за день не удалось распознать.
    pub unrecognized: usize,
    /// От сотрудника в этот день ожидаются отметки.
    pub expected: bool,
}

/// Что в дне требует внимания табельщика.
pub fn day_issues(i: &IssueInput, rules: &Rules) -> Vec<Issue> {
    let m = i.effective;
    let mut out = Vec::new();
    let mut push = |kind, severity, text: String| {
        out.push(Issue {
            kind,
            severity,
            text,
        })
    };
    let set = |v: Option<Minutes>| v.is_some_and(|x| x > 0);

    for (column, code) in [
        ("Начало", &m.start_code),
        ("Уходы", &m.away_code),
        ("Конец", &m.end_code),
    ] {
        if let Some(c) = code {
            if rules.absence_coef(c).is_none() {
                push(
                    IssueKind::UnknownCode,
                    Severity::Error,
                    format!("Неизвестный код «{c}» в колонке «{column}»"),
                );
            }
        }
    }
    if let Some(c) = &m.day_code {
        if rules.trip_coef(c).is_none() {
            push(
                IssueKind::UnknownCode,
                Severity::Error,
                format!("Неизвестный тариф командировки «{c}»"),
            );
        }
    }
    if set(m.end) && !set(m.start) {
        push(
            IssueKind::NoStart,
            Severity::Error,
            "Есть время ухода, но нет времени прихода".into(),
        );
    }
    if set(m.start) && !set(m.end) && i.closed {
        push(
            IssueKind::NoEnd,
            Severity::Error,
            "Нет времени ухода".into(),
        );
    }
    if i.reviewed {
        return out;
    }

    let auto = |f: Field| i.manual_mask & f.bit() == 0;
    let flagged = |f: Flag| i.auto.flags.contains(&f);
    if flagged(Flag::EndAssumed) && auto(Field::End) && set(m.end) {
        push(
            IssueKind::EndAssumed,
            Severity::Warn,
            "Уход не отмечен — подставлен конец рабочего дня".into(),
        );
    }
    if auto(Field::StartCode) && m.start_code.is_some() {
        if flagged(Flag::LateNoShow) {
            let text = "Предупредил об опоздании, но не пришёл — проставлено отсутствие";
            push(IssueKind::LateNoShow, Severity::Warn, text.into());
        } else if flagged(Flag::CodeGuessed) {
            push(
                IssueKind::CodeGuessed,
                Severity::Warn,
                "Причина не названа — код подставлен автоматически".into(),
            );
        }
    }
    if i.unrecognized > 0 {
        push(
            IssueKind::Unrecognized,
            Severity::Warn,
            format!("Не распознано сообщений: {}", i.unrecognized),
        );
    }
    if i.workday && i.closed && i.expected && m.is_empty() {
        push(
            IssueKind::NoMarks,
            Severity::Warn,
            "Нет отметок за рабочий день".into(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: Kind, time: Option<Minutes>, code: Option<&str>) -> Event {
        Event {
            kind,
            time,
            code: code.map(str::to_string),
            code_guessed: false,
        }
    }

    fn hm(h: i32, m: i32) -> Option<Minutes> {
        Some(h * 60 + m)
    }

    fn day(events: &[Event]) -> AutoDay {
        derive_day(
            &DayInput {
                events,
                workday: true,
                closed: true,
                ..DayInput::default()
            },
            &Rules::default(),
        )
    }

    fn fields(a: &AutoDay) -> (Option<Minutes>, Option<&str>, Option<Minutes>, Option<&str>) {
        (
            a.mark.start,
            a.mark.start_code.as_deref(),
            a.mark.end,
            a.mark.end_code.as_deref(),
        )
    }

    #[test]
    fn office_and_leave() {
        let a = day(&[
            ev(Kind::Arrive, hm(8, 44), None),
            ev(Kind::Leave, hm(18, 8), None),
        ]);
        assert_eq!(fields(&a), (hm(8, 44), None, hm(18, 8), None));
        assert!(a.flags.is_empty());
    }

    #[test]
    fn late_notice_gives_reason_code() {
        // 9:29 «Задержусь в поликлинике», 10:49 «Офис», 18:09 «Уход»
        let a = day(&[
            ev(Kind::Late, hm(9, 29), Some("Б")),
            ev(Kind::Arrive, hm(10, 49), None),
            ev(Kind::Leave, hm(18, 9), None),
        ]);
        assert_eq!(fields(&a), (hm(10, 49), Some("Б"), hm(18, 9), None));
        // пояснение в самом «офис до …» важнее предупреждения
        let a = day(&[
            ev(Kind::Late, hm(8, 6), Some("Ф")),
            ev(Kind::Arrive, hm(12, 45), Some("К")),
            ev(Kind::Leave, hm(18, 13), None),
        ]);
        assert_eq!(a.mark.start_code.as_deref(), Some("К"));
        // пришёл вовремя — код не нужен
        let a = day(&[
            ev(Kind::Late, hm(8, 0), Some("Ф")),
            ev(Kind::Arrive, hm(8, 55), None),
            ev(Kind::Leave, hm(18, 0), None),
        ]);
        assert_eq!(a.mark.start_code, None);
    }

    #[test]
    fn first_arrival_and_last_leave_win() {
        let a = day(&[
            ev(Kind::Arrive, hm(9, 24), Some("К")),
            ev(Kind::Arrive, hm(11, 52), Some("К")),
            ev(Kind::Leave, hm(15, 0), None),
            ev(Kind::Leave, hm(18, 13), None),
        ]);
        assert_eq!(fields(&a), (hm(9, 24), Some("К"), hm(18, 13), None));
    }

    #[test]
    fn whole_day_absence() {
        let a = day(&[ev(Kind::Absent, hm(9, 4), Some("Б"))]);
        assert_eq!(fields(&a), (None, Some("Б"), None, None));
        // в выходной сообщение «болею» ничего не ставит
        let a = derive_day(
            &DayInput {
                events: &[ev(Kind::Absent, hm(9, 4), Some("Б"))],
                closed: true,
                ..DayInput::default()
            },
            &Rules::default(),
        );
        assert!(a.mark.is_empty());
        // отсутствовал утром, потом пришёл
        let a = day(&[
            ev(Kind::Absent, hm(8, 0), Some("Б")),
            ev(Kind::Arrive, hm(14, 0), None),
            ev(Kind::Leave, hm(18, 0), None),
        ]);
        assert_eq!(fields(&a), (hm(14, 0), Some("Б"), hm(18, 0), None));
    }

    #[test]
    fn late_notice_without_arrival() {
        let events = [ev(Kind::Late, hm(7, 51), Some("Б"))];
        let a = day(&events);
        assert_eq!(fields(&a), (None, Some("Б"), None, None));
        assert_eq!(a.flags, vec![Flag::LateNoShow]);
        // пока день идёт, ничего не ставим
        let a = derive_day(
            &DayInput {
                events: &events,
                workday: true,
                ..DayInput::default()
            },
            &Rules::default(),
        );
        assert!(a.mark.is_empty());
    }

    #[test]
    fn missing_leave_is_assumed_after_day_end() {
        let events = [ev(Kind::Arrive, hm(9, 47), None)];
        let a = day(&events);
        assert_eq!(fields(&a), (hm(9, 47), None, hm(18, 0), None));
        assert_eq!(a.flags, vec![Flag::EndAssumed]);
        let today = derive_day(
            &DayInput {
                events: &events,
                workday: true,
                ..DayInput::default()
            },
            &Rules::default(),
        );
        assert_eq!(today.mark.end, None);
    }

    #[test]
    fn away_and_back() {
        // 9:00 офис, 14:30 выезд на объект, 16:00 вернулся, 18:30 уход
        let a = day(&[
            ev(Kind::Arrive, hm(9, 0), None),
            ev(Kind::Away, hm(14, 30), Some("К")),
            ev(Kind::Back, hm(16, 0), None),
            ev(Kind::Leave, hm(18, 30), None),
        ]);
        assert_eq!(
            (a.mark.away, a.mark.away_code.as_deref()),
            (Some(90), Some("К"))
        );
        assert_eq!(fields(&a), (hm(9, 0), None, hm(18, 30), None));
        // убыл прямо с выезда
        let a = day(&[
            ev(Kind::Arrive, hm(9, 0), None),
            ev(Kind::Away, hm(14, 30), Some("К")),
            ev(Kind::Leave, hm(17, 40), None),
        ]);
        assert_eq!(
            (a.mark.away, a.mark.end, a.mark.end_code.as_deref()),
            (None, hm(17, 40), Some("К"))
        );
        // выехал и больше не писал: конец дня в офисе — время выезда
        let a = day(&[
            ev(Kind::Arrive, hm(9, 11), None),
            ev(Kind::Away, hm(14, 15), Some("К")),
        ]);
        assert_eq!(fields(&a), (hm(9, 11), None, hm(14, 15), Some("К")));
        // день начался с выезда
        let a = day(&[ev(Kind::Away, hm(9, 2), Some("К"))]);
        assert_eq!(fields(&a), (hm(9, 2), Some("К"), hm(18, 0), None));
        // «уход» в обед и снова «офис»
        let a = day(&[
            ev(Kind::Arrive, hm(9, 0), None),
            ev(Kind::Leave, hm(13, 0), Some("Б")),
            ev(Kind::Arrive, hm(15, 0), None),
            ev(Kind::Leave, hm(18, 0), None),
        ]);
        assert_eq!(
            (a.mark.away, a.mark.away_code.as_deref(), a.mark.end),
            (Some(120), Some("Б"), hm(18, 0))
        );
    }

    #[test]
    fn periods() {
        let rules = Rules::default();
        let vacation = PeriodDay {
            code: "О".into(),
            day_code: None,
        };
        let base = DayInput {
            workday: true,
            closed: true,
            ..DayInput::default()
        };
        let a = derive_day(
            &DayInput {
                period: Some(&vacation),
                ..base.clone()
            },
            &rules,
        );
        assert_eq!(fields(&a), (None, Some("О"), None, None));
        // выходной внутри отпуска не помечается
        let a = derive_day(
            &DayInput {
                period: Some(&vacation),
                workday: false,
                ..base.clone()
            },
            &rules,
        );
        assert!(a.mark.is_empty());
        // работа во время отпуска: время остаётся
        let events = [
            ev(Kind::Arrive, hm(10, 0), None),
            ev(Kind::Leave, hm(15, 0), None),
        ];
        let a = derive_day(
            &DayInput {
                period: Some(&vacation),
                events: &events,
                ..base.clone()
            },
            &rules,
        );
        assert_eq!(fields(&a), (hm(10, 0), Some("О"), hm(15, 0), None));

        // иногородняя командировка: только тариф, время из сообщений не учитывается
        let trip = PeriodDay {
            code: "К".into(),
            day_code: Some("К3".into()),
        };
        let events = [
            ev(Kind::Arrive, hm(7, 47), Some("К")),
            ev(Kind::Leave, hm(21, 34), Some("К")),
        ];
        let a = derive_day(
            &DayInput {
                period: Some(&trip),
                events: &events,
                workday: false,
                ..base.clone()
            },
            &rules,
        );
        assert_eq!(
            a.mark,
            Mark {
                day_code: Some("К3".into()),
                ..Mark::default()
            }
        );
        // …или учитывается, если так настроено
        let a = derive_day(
            &DayInput {
                period: Some(&trip),
                events: &events,
                trip_use_times: true,
                ..base.clone()
            },
            &rules,
        );
        assert_eq!(
            (
                a.mark.start,
                a.mark.start_code.clone(),
                a.mark.end,
                a.mark.day_code.as_deref()
            ),
            (hm(7, 47), None, hm(21, 34), Some("К3"))
        );
    }

    #[test]
    fn schedule_for_people_without_telegram() {
        let rules = Rules::default();
        let base = DayInput {
            workday: true,
            auto_schedule: true,
            ..DayInput::default()
        };
        let a = derive_day(&base, &rules);
        assert_eq!(fields(&a), (hm(9, 0), None, hm(18, 0), None));
        assert!(derive_day(
            &DayInput {
                workday: false,
                ..base.clone()
            },
            &rules
        )
        .mark
        .is_empty());
        assert!(derive_day(
            &DayInput {
                future: true,
                ..base.clone()
            },
            &rules
        )
        .mark
        .is_empty());
    }

    #[test]
    fn issues() {
        let rules = Rules::default();
        let check = |effective: &Mark,
                     auto: &AutoDay,
                     manual_mask: u8,
                     reviewed: bool,
                     unrecognized: usize| {
            let input = IssueInput {
                effective,
                auto,
                manual_mask,
                workday: true,
                closed: true,
                reviewed,
                unrecognized,
                expected: true,
            };
            day_issues(&input, &rules)
                .into_iter()
                .map(|i| i.kind)
                .collect::<Vec<_>>()
        };
        let empty = AutoDay::default();
        assert_eq!(
            check(&Mark::default(), &empty, 0, false, 0),
            vec![IssueKind::NoMarks]
        );
        assert_eq!(check(&Mark::default(), &empty, 0, true, 0), vec![]);
        assert_eq!(
            check(&Mark::default(), &empty, 0, false, 2),
            vec![IssueKind::Unrecognized, IssueKind::NoMarks]
        );

        let only_end = Mark {
            end: hm(18, 20),
            ..Mark::default()
        };
        assert_eq!(
            check(&only_end, &empty, 0, true, 0),
            vec![IssueKind::NoStart]
        );

        let assumed = day(&[ev(Kind::Arrive, hm(9, 47), None)]);
        assert_eq!(
            check(&assumed.mark, &assumed, 0, false, 0),
            vec![IssueKind::EndAssumed]
        );
        // табельщик подтвердил/исправил время ухода вручную
        assert_eq!(
            check(&assumed.mark, &assumed, Field::End.bit(), false, 0),
            vec![]
        );

        let bad = Mark {
            start_code: Some("Я".into()),
            ..Mark::default()
        };
        assert_eq!(
            check(&bad, &empty, Field::StartCode.bit(), true, 0),
            vec![IssueKind::UnknownCode]
        );
    }
}
