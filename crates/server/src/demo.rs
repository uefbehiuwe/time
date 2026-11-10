//! Демонстрационные данные: вымышленные сотрудники и месяц переписки.
//!
//! Нужны, чтобы посмотреть интерфейс и автоматику, не подключая бота к настоящей группе.

use anyhow::{bail, Result};
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime};
use timelist_core::calendar::{month_days, Calendar, Special};

use crate::db::{month_bounds, Db, Employee, NewMessage, Period};

const PEOPLE: &[(&str, &str, &str, &str, bool)] = &[
    ("Соколов А.В.", "АнС", "Андрей Соколов", "", true),
    ("Белова М.С.", "МаБ", "Марина Белова", "", false),
    ("Каримов Т.Р.", "ТиК", "Тимур Каримов", "", false),
    ("Орлова И.Д.", "ИрО", "Ирина Орлова", "Ира", false),
    ("Громов Д.И.", "ДмГ", "Дмитрий Громов", "Дима", false),
    ("Юсупов Р.А.", "РуЮ", "Рустам Юсупов", "", false),
    ("Волкова Н.П.", "НаВ", "Наталья Волкова", "Наташа", false),
    ("Пак В.Л.", "ВиП", "Виктор Пак", "Витя", false),
];

/// Детерминированный генератор: демо выглядит одинаково при каждом запуске.
struct Rng(u64);

impl Rng {
    fn next(&mut self, bound: u32) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound as u64) as u32
    }
}

fn at(date: NaiveDate, minutes: u32) -> NaiveDateTime {
    date.and_hms_opt(minutes / 60, minutes % 60, 0)
        .expect("valid time")
}

/// Заполняет пустую базу: сотрудники, сообщения с начала месяца по `today`, периоды отсутствия.
pub fn seed(db: &Db, today: NaiveDate) -> Result<String> {
    if !db.employees()?.is_empty() {
        bail!("в базе уже есть сотрудники — демо-данные добавляются только в пустую базу");
    }
    let mut ids = Vec::new();
    for (i, (full_name, nick, tg_name, aliases, auto_schedule)) in PEOPLE.iter().enumerate() {
        ids.push(db.insert_employee(&Employee {
            id: 0,
            full_name: full_name.to_string(),
            nick: nick.to_string(),
            tg_name: tg_name.to_string(),
            tg_user_id: None,
            aliases: aliases.to_string(),
            auto_schedule: *auto_schedule,
            active: true,
            sort: i as i64 + 1,
            works_from: None,
            works_to: None,
        })?);
    }

    let (year, month) = (today.year(), today.month());
    let (first, last) = month_bounds(year, month);
    let special = db.calendar_days(first, last)?;
    let calendar = Calendar::new(
        special
            .iter()
            .filter_map(|d| Special::parse(&d.kind).map(|k| (d.date, k))),
    );
    let workdays: Vec<NaiveDate> = month_days(year, month)
        .into_iter()
        .filter(|d| *d <= today && calendar.is_workday(*d))
        .collect();

    // периоды: отпуск, больничный, иногородняя командировка
    let period =
        |employee: usize, code: &str, from: NaiveDate, to: NaiveDate, tariff: bool, note: &str| {
            Period {
                id: 0,
                employee_id: ids[employee],
                code: code.into(),
                date_from: from,
                date_to: to,
                tariff,
                note: note.into(),
            }
        };
    let day = |n: usize| workdays.get(n).copied();
    let mut covered: Vec<(usize, NaiveDate, NaiveDate)> = Vec::new();
    if let (Some(from), Some(to)) = (day(1), day(5)) {
        db.insert_period(&period(
            3,
            "О",
            from,
            to,
            false,
            "ежегодный отпуск по графику",
        ))?;
        covered.push((3, from, to));
    }
    if let (Some(from), Some(to)) = (day(2), day(3)) {
        db.insert_period(&period(6, "Б", from, to, false, "больничный лист"))?;
        covered.push((6, from, to));
    }
    if let Some(from) = day(3) {
        let to = (from + Duration::days(4)).min(today);
        for who in [4, 5] {
            db.insert_period(&period(
                who,
                "К",
                from,
                to,
                true,
                "монтаж на объекте в Самарканде",
            ))?;
            covered.push((who, from, to));
        }
    }

    let mut rng = Rng(0x5EED_2026);
    let mut count = 0;
    let mut say = |who: usize, when: NaiveDateTime, text: &str| -> Result<()> {
        let m = NewMessage {
            source: "demo",
            chat_id: None,
            tg_message_id: None,
            tg_user_id: None,
            sender_name: PEOPLE[who].2,
            employee_id: None,
            sent_at: when,
            text,
        };
        db.upsert_message(&m)?;
        count += 1;
        Ok(())
    };

    for (n, date) in workdays.iter().enumerate() {
        let is_today = *date == today;
        for who in 1..PEOPLE.len() {
            if covered
                .iter()
                .any(|(w, from, to)| *w == who && from <= date && date <= to)
            {
                continue;
            }
            let arrive = 8 * 60 + 38 + rng.next(45);
            let leave = 18 * 60 + rng.next(32);
            // сценарий дня: в основном обычный, изредка — то, что раньше разбирали вручную;
            // несколько случаев заданы явно, чтобы в демо всегда было что проверить
            let roll = rng.next(12);
            let scenario = match (who, n) {
                (1, 0) => 5,
                (2, 1) => 12,
                (7, 2) => 13,
                (1, 4) => 8,
                _ => roll,
            };
            match (scenario, is_today) {
                (0, _) => {
                    say(
                        who,
                        at(*date, 8 * 60 + 5 + rng.next(30)),
                        "Задержусь у врача",
                    )?;
                    say(who, at(*date, 10 * 60 + 20 + rng.next(60)), "Офис")?;
                    say(who, at(*date, leave), "Уход")?;
                }
                (1, _) => {
                    say(
                        who,
                        at(*date, 8 * 60 + 10),
                        "Задерживаюсь по семейным обстоятельствам",
                    )?;
                    say(who, at(*date, 9 * 60 + 40 + rng.next(40)), "Офис")?;
                    say(who, at(*date, leave), "Уход")?;
                }
                (2, _) => {
                    say(
                        who,
                        at(*date, 10 * 60 + 30 + rng.next(50)),
                        "Офис, до банк и налоговая",
                    )?;
                    say(who, at(*date, leave), "Уход")?;
                }
                (3, false) => {
                    say(who, at(*date, arrive), "Офис")?;
                    say(
                        who,
                        at(*date, 16 * 60 + 40 + rng.next(40)),
                        "Уход с объекта",
                    )?;
                }
                (4, false) => say(who, at(*date, 8 * 60 + 20), "Болею, на связи")?,
                (5, false) => say(who, at(*date, arrive), "Офис")?, // забыл отметить уход
                (6, false) if n + 1 < workdays.len() => {
                    say(who, at(*date, arrive), "В офисе")?;
                    let next = workdays[n + 1];
                    say(
                        who,
                        at(next, 8 * 60 + 50),
                        &format!("{} уход 18:10", date.format("%d.%m")),
                    )?;
                }
                (7, false) => {
                    say(who, at(*date, arrive), "Офис")?;
                    say(
                        who,
                        at(*date, 14 * 60 + 30),
                        "Выезд на объект, закуп материалов",
                    )?;
                    say(who, at(*date, 16 * 60 + 10), "Вернулся в офис")?;
                    say(who, at(*date, leave), "Уход")?;
                }
                (8, false) => {
                    say(who, at(*date, arrive), "Офис")?;
                    say(
                        who,
                        at(*date, 11 * 60 + 5),
                        "Коллеги, планёрка переносится на 15:00",
                    )?;
                    say(who, at(*date, leave), "Уход")?;
                }
                (9, false) => {
                    say(who, at(*date, 9 * 60 + 30 + rng.next(20)), "Офис 9:05")?;
                    say(who, at(*date, 17 * 60 + 10), "Уход. Согласовано")?;
                }
                (12, false) => say(who, at(*date, 8 * 60 + 30), "Отсутствую, на связи")?,
                (13, false) => say(who, at(*date, leave), "Уход")?, // забыл отметить приход
                _ => {
                    say(who, at(*date, arrive), "Офис")?;
                    if !is_today {
                        say(who, at(*date, leave), "Уход")?;
                    }
                }
            }
        }
    }
    // сообщения из командировки: расчёт идёт по тарифу, но переписка остаётся видна
    for (who, from, to) in covered.iter().filter(|(w, _, _)| *w == 4) {
        for date in from.iter_days().take_while(|d| d <= to).skip(1) {
            say(*who, at(date, 8 * 60 + 5), "Я и Рустам на объекте")?;
            say(
                *who,
                at(date, 19 * 60 + 20),
                "Я и Рустам на сегодня закончили",
            )?;
        }
    }
    Ok(format!(
        "демо-данные: сотрудников {}, сообщений {count}, месяц {year}-{month:02}",
        PEOPLE.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::Engine;

    #[test]
    fn demo_month_is_filled_by_automation() {
        let db = Db::in_memory().unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        seed(&db, today).unwrap();
        assert!(
            seed(&db, today).is_err(),
            "повторный запуск не должен дублировать данные"
        );

        let view = Engine::new(&db).unwrap().month(2026, 11, today).unwrap();
        assert_eq!(view.employees.len(), PEOPLE.len());
        // директор не пишет в группу — ему ставится полный день
        let director = &view.employees[0];
        assert!(director
            .cells
            .iter()
            .zip(&view.days)
            .all(|(c, d)| !d.kind.is_workday() || d.date > today || c.m.start == Some(540)));
        // отпуск и больничный проставлены кодами, командировка — тарифами
        assert!(view.employees[3]
            .cells
            .iter()
            .any(|c| c.m.start_code.as_deref() == Some("О")));
        assert!(view.employees[6]
            .cells
            .iter()
            .any(|c| c.m.start_code.as_deref() == Some("Б")));
        assert!(view.employees[4]
            .cells
            .iter()
            .any(|c| c.m.day_code.as_deref() == Some("К2")));
        // все отметки автоматические, часть дней ждёт проверки
        assert!(view
            .employees
            .iter()
            .all(|e| e.cells.iter().all(|c| c.mask == 0)));
        assert!(view.issues > 0);
        assert!(view.employees.iter().skip(1).all(|e| e.tabel.max > 0.0));
    }
}
