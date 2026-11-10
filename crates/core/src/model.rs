use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Минуты: время суток (от полуночи) либо длительность.
pub type Minutes = i32;

/// Отметка сотрудника за день — те же колонки, что в блоке листа «TimeList»:
/// Начало | код | Уходы | код | Конец | код | Код.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    /// Начало рабочего дня (время суток).
    pub start: Option<Minutes>,
    /// Категория причины опоздания либо отсутствия весь день (О, У, К, Б, Ф, Г, П).
    pub start_code: Option<String>,
    /// «Уходы» — суммарная длительность отлучек в течение дня.
    pub away: Option<Minutes>,
    pub away_code: Option<String>,
    /// Конец рабочего дня (время суток).
    pub end: Option<Minutes>,
    pub end_code: Option<String>,
    /// Тариф иногородней командировки (К1, К2, К3).
    pub day_code: Option<String>,
}

impl Mark {
    pub fn is_empty(&self) -> bool {
        *self == Mark::default()
    }
}

/// Поля отметки; порядок соответствует битам маски ручных правок.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Start,
    StartCode,
    Away,
    AwayCode,
    End,
    EndCode,
    DayCode,
}

impl Field {
    pub const ALL: [Field; 7] = [
        Field::Start,
        Field::StartCode,
        Field::Away,
        Field::AwayCode,
        Field::End,
        Field::EndCode,
        Field::DayCode,
    ];

    pub fn bit(self) -> u8 {
        1 << (self as u8)
    }

    pub fn name(self) -> &'static str {
        match self {
            Field::Start => "start",
            Field::StartCode => "start_code",
            Field::Away => "away",
            Field::AwayCode => "away_code",
            Field::End => "end",
            Field::EndCode => "end_code",
            Field::DayCode => "day_code",
        }
    }

    pub fn from_name(name: &str) -> Option<Field> {
        Field::ALL.into_iter().find(|f| f.name() == name)
    }

    pub fn is_time(self) -> bool {
        matches!(self, Field::Start | Field::Away | Field::End)
    }
}

impl Mark {
    /// Берёт из `manual` поля, отмеченные в маске, остальные — из `self`.
    pub fn overlay(&self, manual: &Mark, mask: u8) -> Mark {
        let mut out = self.clone();
        if mask & Field::Start.bit() != 0 {
            out.start = manual.start;
        }
        if mask & Field::StartCode.bit() != 0 {
            out.start_code = manual.start_code.clone();
        }
        if mask & Field::Away.bit() != 0 {
            out.away = manual.away;
        }
        if mask & Field::AwayCode.bit() != 0 {
            out.away_code = manual.away_code.clone();
        }
        if mask & Field::End.bit() != 0 {
            out.end = manual.end;
        }
        if mask & Field::EndCode.bit() != 0 {
            out.end_code = manual.end_code.clone();
        }
        if mask & Field::DayCode.bit() != 0 {
            out.day_code = manual.day_code.clone();
        }
        out
    }
}

/// Параметры расчёта: ячейки A1/C1/D1 листа «Табель» и коэффициенты листа «Список».
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rules {
    /// Начало рабочего дня (Табель!A1), по умолчанию 9:00.
    pub work_start: Minutes,
    /// Конец рабочего дня (Табель!C1), по умолчанию 18:00.
    pub work_end: Minutes,
    /// Перерыв на обед (Табель!D1), по умолчанию 1:00.
    pub lunch: Minutes,
    /// Коэффициенты учёта причин отсутствия (Список!B21:C27).
    pub absence: BTreeMap<String, f64>,
    /// Тарифы иногородней командировки (Список!B30:C32).
    pub trip: BTreeMap<String, f64>,
}

impl Default for Rules {
    fn default() -> Self {
        let absence = [
            ("О", 1.0),
            ("У", 1.0),
            ("К", 1.0),
            ("Б", 0.8),
            ("Ф", 0.5),
            ("Г", 0.0),
            ("П", 0.0),
        ];
        let trip = [("К1", 1.0), ("К2", 1.25), ("К3", 1.5)];
        Rules {
            work_start: 9 * 60,
            work_end: 18 * 60,
            lunch: 60,
            absence: absence
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            trip: trip.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        }
    }
}

impl Rules {
    /// Длительность рабочего дня без обеда (8 часов при графике 9–18).
    pub fn day_length(&self) -> Minutes {
        (self.work_end - self.work_start - self.lunch).max(0)
    }

    pub fn absence_coef(&self, code: &str) -> Option<f64> {
        self.absence.get(code).copied()
    }

    pub fn trip_coef(&self, code: &str) -> Option<f64> {
        self.trip.get(code).copied()
    }
}
