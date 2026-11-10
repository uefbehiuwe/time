//! Сверка расчёта дня с исходными Excel-книгами.
//!
//! Фикстура — обезличенный набор уникальных дневных отметок из трёх месяцев книги
//! (без имён, дат и привязки к сотруднику) и значения колонок E, F, O…T листа
//! «Табель», которые для этих отметок посчитал Excel. Ядро обязано получить те же цифры.

use std::collections::BTreeMap;

use serde::Deserialize;
use timelist_core::calc::calc_day;
use timelist_core::{Mark, Rules};

#[derive(Deserialize)]
struct Fixture {
    rules: FixtureRules,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct FixtureRules {
    work_start: i32,
    work_end: i32,
    lunch: i32,
    codes: BTreeMap<String, f64>,
    trip_codes: BTreeMap<String, f64>,
}

/// Начало, код, Уходы, код, Конец, код, Код
type MarkFields = (
    Option<i32>,
    Option<String>,
    Option<i32>,
    Option<String>,
    Option<i32>,
    Option<String>,
    Option<String>,
);

#[derive(Deserialize)]
struct Case {
    m: MarkFields,
    /// E, F, O, P, Q, R, S, T листа «Табель» в минутах
    x: [f64; 8],
}

#[test]
fn day_calculation_matches_excel() {
    let fx: Fixture = serde_json::from_str(include_str!("fixtures/excel-day-cases.json")).unwrap();
    let rules = Rules {
        work_start: fx.rules.work_start,
        work_end: fx.rules.work_end,
        lunch: fx.rules.lunch,
        absence: fx.rules.codes,
        trip: fx.rules.trip_codes,
    };
    assert!(
        fx.cases.len() > 300,
        "фикстура неожиданно мала: {}",
        fx.cases.len()
    );

    let mut kinds = [0usize; 4];
    for case in fx.cases {
        let (start, start_code, away, away_code, end, end_code, day_code) = case.m;
        let mark = Mark {
            start,
            start_code,
            away,
            away_code,
            end,
            end_code,
            day_code,
        };
        let c = calc_day(&mark, &rules);
        let got = [
            c.worked,
            c.calc,
            c.late,
            c.late_calc,
            c.away,
            c.away_calc,
            c.early,
            c.early_calc,
        ];
        for (i, column) in ["E", "F", "O", "P", "Q", "R", "S", "T"].iter().enumerate() {
            assert!(
                (got[i] - case.x[i]).abs() < 1e-3,
                "{mark:?}, колонка {column}: получено {}, в Excel {}",
                got[i],
                case.x[i]
            );
        }
        // фикстура должна покрывать все ветки формулы F
        kinds[0] += (mark.start.is_some() && mark.start_code.is_some()) as usize;
        kinds[1] += (mark.start.is_none() && mark.start_code.is_some()) as usize;
        kinds[2] += (mark.start.is_some() && mark.day_code.is_some()) as usize;
        kinds[3] += (mark.start.is_none() && mark.day_code.is_some()) as usize;
    }
    assert!(
        kinds.iter().all(|n| *n > 0),
        "не все виды отметок есть в фикстуре: {kinds:?}"
    );
}
