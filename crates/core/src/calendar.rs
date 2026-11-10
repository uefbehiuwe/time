//! Производственный календарь: рабочие и нерабочие дни, расчётная норма месяца.

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate, Weekday};
use serde::{Deserialize, Serialize};

/// Особый день календаря (исключение из правила «Пн–Пт рабочие»).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Special {
    /// Праздник или дополнительный/перенесённый выходной.
    Holiday,
    /// Рабочая суббота/воскресенье (перенос).
    Workday,
    /// Сокращённый предпраздничный рабочий день (на 1 час короче).
    Short,
}

impl Special {
    pub fn as_str(self) -> &'static str {
        match self {
            Special::Holiday => "holiday",
            Special::Workday => "workday",
            Special::Short => "short",
        }
    }

    pub fn parse(s: &str) -> Option<Special> {
        match s {
            "holiday" => Some(Special::Holiday),
            "workday" => Some(Special::Workday),
            "short" => Some(Special::Short),
            _ => None,
        }
    }
}

/// Тип дня для сетки тайм-листа.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DayKind {
    Work,
    Short,
    Weekend,
    Holiday,
}

impl DayKind {
    pub fn is_workday(self) -> bool {
        matches!(self, DayKind::Work | DayKind::Short)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Calendar {
    special: BTreeMap<NaiveDate, Special>,
}

impl Calendar {
    pub fn new(special: impl IntoIterator<Item = (NaiveDate, Special)>) -> Self {
        Calendar {
            special: special.into_iter().collect(),
        }
    }

    pub fn kind(&self, date: NaiveDate) -> DayKind {
        match self.special.get(&date) {
            Some(Special::Holiday) => DayKind::Holiday,
            Some(Special::Workday) => DayKind::Work,
            Some(Special::Short) => DayKind::Short,
            None if matches!(date.weekday(), Weekday::Sat | Weekday::Sun) => DayKind::Weekend,
            None => DayKind::Work,
        }
    }

    pub fn is_workday(&self, date: NaiveDate) -> bool {
        self.kind(date).is_workday()
    }

    /// Расчётная норма месяца: (рабочих дней, часов) при 8-часовом дне.
    /// Используется, когда норма не задана вручную в справочнике.
    pub fn month_norm(&self, year: i32, month: u32, day_hours: f64) -> (u32, f64) {
        let mut days = 0;
        let mut hours = 0.0;
        for date in month_days(year, month) {
            match self.kind(date) {
                DayKind::Work => {
                    days += 1;
                    hours += day_hours;
                }
                DayKind::Short => {
                    days += 1;
                    hours += day_hours - 1.0;
                }
                _ => {}
            }
        }
        (days, hours)
    }
}

/// Все даты месяца по порядку.
pub fn month_days(year: i32, month: u32) -> Vec<NaiveDate> {
    let Some(first) = NaiveDate::from_ymd_opt(year, month, 1) else {
        return Vec::new();
    };
    first
        .iter_days()
        .take_while(|d| d.month() == month)
        .collect()
}

/// Двухбуквенное название дня недели, как в книге (Список!O19:P25).
pub fn weekday_short(date: NaiveDate) -> &'static str {
    ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"][date.weekday().num_days_from_monday() as usize]
}

pub fn month_name(month: u32) -> &'static str {
    [
        "Январь",
        "Февраль",
        "Март",
        "Апрель",
        "Май",
        "Июнь",
        "Июль",
        "Август",
        "Сентябрь",
        "Октябрь",
        "Ноябрь",
        "Декабрь",
    ]
    .get(month as usize - 1)
    .copied()
    .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn computes_norm_from_special_days() {
        // Сентябрь 2026: праздник 1-го, сокращённый день 30-го → 21 день, 167 часов
        let cal = Calendar::new([
            (d(2026, 9, 1), Special::Holiday),
            (d(2026, 9, 30), Special::Short),
        ]);
        assert_eq!(cal.month_norm(2026, 9, 8.0), (21, 167.0));
        // Декабрь 2026: рабочая суббота 12-го, выходные 8-го и 31-го, сокращённый 7-го
        let cal = Calendar::new([
            (d(2026, 12, 8), Special::Holiday),
            (d(2026, 12, 31), Special::Holiday),
            (d(2026, 12, 12), Special::Workday),
            (d(2026, 12, 7), Special::Short),
        ]);
        assert_eq!(cal.month_norm(2026, 12, 8.0), (22, 175.0));
        assert_eq!(cal.kind(d(2026, 12, 12)), DayKind::Work);
        assert_eq!(cal.kind(d(2026, 12, 13)), DayKind::Weekend);
    }

    #[test]
    fn weekday_names() {
        assert_eq!(weekday_short(d(2026, 10, 1)), "Чт");
        assert_eq!(weekday_short(d(2026, 10, 4)), "Вс");
        assert_eq!(month_days(2026, 2).len(), 28);
        assert_eq!(month_name(10), "Октябрь");
    }
}
