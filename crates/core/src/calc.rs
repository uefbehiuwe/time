//! Формулы листов «Табель» и «Сводка».
//!
//! Все длительности — в минутах (`f64`, потому что коэффициенты дают дробные значения).
//! Имена полей повторяют колонки Excel; в комментариях указаны исходные ячейки
//! первого блока сотрудника (колонки E…T листа «Табель»).

use serde::{Deserialize, Serialize};

use crate::model::{Mark, Rules};

const EPS: f64 = 1e-9;

/// Расчёт одного дня — колонки E, F, O…T листа «Табель».
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DayCalc {
    /// E «Отраб.врем. т-лист»: Конец − Начало − Уходы − обед.
    pub worked: f64,
    /// F «Отраб.врем. расч.»: время с компенсацией по кодам причин.
    pub calc: f64,
    /// O «Опоздания т-лист».
    pub late: f64,
    /// P «Опоздания расч.» — за вычетом доли, покрытой кодом причины.
    pub late_calc: f64,
    /// Q «Отсутствия т-лист».
    pub away: f64,
    /// R «Отсутствия расч.».
    pub away_calc: f64,
    /// S «Ранний уход т-лист».
    pub early: f64,
    /// T «Ранний уход расч.».
    pub early_calc: f64,
}

fn code(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// Считает день так же, как строка листа «Табель».
///
/// Отличия от Excel, оба сделаны намеренно:
/// * если есть «Конец», но нет «Начала», Excel считает отработанным `Конец − обед`
///   (заведомо неверные ~17 часов); здесь такой день даёт 0 и помечается ошибкой в UI;
/// * продолжительность дня берётся как `конец − начало − обед`, а не `$A$1-$D$1`
///   (в книге это те же 8 часов только при графике 9:00–18:00).
pub fn calc_day(m: &Mark, r: &Rules) -> DayCalc {
    let g = m.start.filter(|v| *v > 0).unwrap_or(0) as f64;
    let i = m.away.filter(|v| *v > 0).unwrap_or(0) as f64;
    let k = m.end.filter(|v| *v > 0).unwrap_or(0) as f64;
    let (h, j, l, day_code) = (
        code(&m.start_code),
        code(&m.away_code),
        code(&m.end_code),
        code(&m.day_code),
    );
    let coef = |c: &str| r.absence_coef(c).unwrap_or(0.0);
    let (ws, we, lunch) = (r.work_start as f64, r.work_end as f64, r.lunch as f64);
    let day_len = r.day_length() as f64;

    // E: =IF(K-G-I-$D$1<=0,0,K-G-I-$D$1)
    let worked = if g > 0.0 && k > 0.0 {
        (k - g - i - lunch).max(0.0)
    } else {
        0.0
    };
    // O: =IF(G-$A$1<=0,0,G-$A$1)
    let late = if g > 0.0 { (g - ws).max(0.0) } else { 0.0 };
    // P: =IF(AND(G>0,H>0),(1-VLOOKUP(H,коды))*O,O)
    let late_calc = match h {
        Some(c) if g > 0.0 => (1.0 - coef(c)) * late,
        _ => late,
    };
    // Q: =I
    let away = i;
    // R: =IF(AND(G>0,K>0,J>0),(1-VLOOKUP(J,коды))*Q,Q)
    let away_calc = match j {
        Some(c) if g > 0.0 && k > 0.0 => (1.0 - coef(c)) * away,
        _ => away,
    };
    // S: =IF(AND(G>0,K>0),IF($C$1-K<=0,0,$C$1-K),0)
    let early = if g > 0.0 && k > 0.0 {
        (we - k).max(0.0)
    } else {
        0.0
    };
    // T: =IF(AND(K>0,L>0),(1-VLOOKUP(L,коды))*S,S)
    let early_calc = match l {
        Some(c) if k > 0.0 => (1.0 - coef(c)) * early,
        _ => early,
    };

    // F: (E+(O-P)+(Q-R)+(S-T))*тариф + 8ч*коэф(H при G=0) + 8ч*тариф(M без времени)
    let trip = day_code.and_then(|c| r.trip_coef(c));
    let mut calc = (worked + (late - late_calc) + (away - away_calc) + (early - early_calc))
        * trip.unwrap_or(1.0);
    if let Some(c) = h {
        if g == 0.0 {
            calc += day_len * coef(c);
        }
    }
    if let Some(t) = trip {
        if g == 0.0 && i == 0.0 && k == 0.0 {
            calc += day_len * t;
        }
    }

    DayCalc {
        worked,
        calc,
        late,
        late_calc,
        away,
        away_calc,
        early,
        early_calc,
    }
}

/// Итоги сотрудника за месяц — строки 3…18 блока листа «Табель».
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tabel {
    /// Стр. 3 «Процент испол.норм.раб.вр.»
    pub usage: f64,
    /// Стр. 4 «Учетное время по Т-листу»
    pub worked: f64,
    /// Стр. 5 «Расчетное время с компенс.»
    pub calc: f64,
    /// Стр. 6 «Макс.время, с учет.перераб.»
    pub max: f64,
    /// Стр. 7–10 опоздания: количество и время (т-лист / расч.)
    pub late_n: u32,
    pub late_n_calc: u32,
    pub late_t: f64,
    pub late_t_calc: f64,
    /// Стр. 11–14 отлучения
    pub away_n: u32,
    pub away_n_calc: u32,
    pub away_t: f64,
    pub away_t_calc: f64,
    /// Стр. 15–18 ранние уходы
    pub early_n: u32,
    pub early_n_calc: u32,
    pub early_t: f64,
    pub early_t_calc: f64,
}

pub fn calc_tabel(days: &[DayCalc], norm_hours: f64) -> Tabel {
    let mut t = Tabel::default();
    for d in days {
        t.worked += d.worked;
        t.calc += d.calc;
        t.late_t += d.late;
        t.late_t_calc += d.late_calc;
        t.away_t += d.away;
        t.away_t_calc += d.away_calc;
        t.early_t += d.early;
        t.early_t_calc += d.early_calc;
        t.late_n += (d.late > EPS) as u32;
        t.late_n_calc += (d.late_calc > EPS) as u32;
        t.away_n += (d.away > EPS) as u32;
        t.away_n_calc += (d.away_calc > EPS) as u32;
        t.early_n += (d.early > EPS) as u32;
        t.early_n_calc += (d.early_calc > EPS) as u32;
    }
    t.max = t.worked.max(t.calc);
    t.usage = if norm_hours > 0.0 {
        t.max / 60.0 / norm_hours
    } else {
        0.0
    };
    t
}

/// Колонка сотрудника на листе «Сводка» (строки 22…39) и его КТУ.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Svodka {
    /// 1. Норматив рабочего времени
    pub norm: f64,
    /// 2. Процент использования раб.времени
    pub usage: f64,
    /// 2.1. Время, отмеченное в Тайм-листе
    pub tl_time: f64,
    /// 2.2. Время, учитываемое в Табеле
    pub tab_time: f64,
    /// 2.3. Всего пропущенного времени по Тайм-листу
    pub missed_tl: f64,
    /// 2.4. Всего не отработанного времени по Табелю
    pub missed_tab: f64,
    /// 2.5. Всего переработанного времени
    pub overtime: f64,
    /// 3. Кол-во опозданий и задержек нач.раб.дня / 3.1. их время
    pub late_n: u32,
    pub late_t: f64,
    /// 4. Кол-во отлучений из офиса / 4.1. их время
    pub away_n: u32,
    pub away_t: f64,
    /// 5. Кол-во ранних уходов / 5.1. время сокращения раб.дня
    pub early_n: u32,
    pub early_t: f64,
    /// КТУ: процент использования, ограниченный лимитом (Сводка!E2).
    pub ktu: f64,
}

pub fn calc_svodka(t: &Tabel, norm_hours: f64, limit: f64) -> Svodka {
    let norm = norm_hours * 60.0;
    let shortfall = |v: f64| if v <= EPS || v >= norm { 0.0 } else { norm - v };
    Svodka {
        norm,
        usage: t.usage,
        tl_time: t.worked,
        tab_time: t.max,
        missed_tl: shortfall(t.worked),
        missed_tab: shortfall(t.max),
        overtime: if t.max <= norm { 0.0 } else { t.max - norm },
        late_n: t.late_n_calc,
        late_t: t.late_t_calc,
        away_n: t.away_n_calc,
        away_t: t.away_t_calc,
        early_n: t.early_n_calc,
        early_t: t.early_t_calc,
        ktu: if t.usage > limit { limit } else { t.usage },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(start: Option<i32>, sc: &str, end: Option<i32>, ec: &str, dc: &str) -> Mark {
        let s = |v: &str| (!v.is_empty()).then(|| v.to_string());
        Mark {
            start,
            start_code: s(sc),
            end,
            end_code: s(ec),
            day_code: s(dc),
            ..Mark::default()
        }
    }

    #[test]
    fn plain_day() {
        let d = calc_day(
            &mark(Some(9 * 60), "", Some(18 * 60), "", ""),
            &Rules::default(),
        );
        assert_eq!(d.worked, 480.0);
        assert_eq!(d.calc, 480.0);
        assert_eq!((d.late, d.early), (0.0, 0.0));
    }

    #[test]
    fn late_with_sick_code_is_partly_compensated() {
        // 10:49 Б … 18:09 — пример из исходной книги
        let d = calc_day(
            &mark(Some(10 * 60 + 49), "Б", Some(18 * 60 + 9), "", ""),
            &Rules::default(),
        );
        assert_eq!(d.worked, 380.0);
        assert_eq!(d.late, 109.0);
        assert!((d.late_calc - 21.8).abs() < 1e-9);
        assert!((d.calc - (380.0 + 109.0 - 21.8)).abs() < 1e-9);
    }

    #[test]
    fn whole_day_codes() {
        let r = Rules::default();
        assert_eq!(calc_day(&mark(None, "О", None, "", ""), &r).calc, 480.0);
        assert_eq!(calc_day(&mark(None, "Б", None, "", ""), &r).calc, 384.0);
        assert_eq!(calc_day(&mark(None, "Ф", None, "", ""), &r).calc, 240.0);
        assert_eq!(calc_day(&mark(None, "Г", None, "", ""), &r).calc, 0.0);
        assert_eq!(calc_day(&mark(None, "", None, "", "К2"), &r).calc, 600.0);
        assert_eq!(calc_day(&mark(None, "", None, "", "К3"), &r).calc, 720.0);
    }

    #[test]
    fn trip_tariff_multiplies_actual_time() {
        // 8:00–19:46 с К2: (10:46) × 1,25
        let d = calc_day(
            &mark(Some(480), "", Some(19 * 60 + 46), "", "К2"),
            &Rules::default(),
        );
        assert_eq!(d.worked, 646.0);
        assert_eq!(d.calc, 807.5);
    }

    #[test]
    fn early_leave_with_trip_code_is_forgiven() {
        let d = calc_day(
            &mark(Some(540), "", Some(17 * 60), "К", ""),
            &Rules::default(),
        );
        assert_eq!(d.early, 60.0);
        assert_eq!(d.early_calc, 0.0);
        assert_eq!(d.calc, 480.0);
    }

    #[test]
    fn end_without_start_counts_nothing() {
        let d = calc_day(&mark(None, "", Some(18 * 60), "", ""), &Rules::default());
        assert_eq!(d.worked, 0.0);
        assert_eq!(d.calc, 0.0);
    }

    #[test]
    fn month_totals_like_tabel_and_svodka() {
        let r = Rules::default();
        let days: Vec<DayCalc> = [
            mark(Some(10 * 60 + 49), "Б", Some(18 * 60 + 9), "", ""), // опоздание по болезни
            mark(Some(9 * 60), "", Some(18 * 60), "", ""),            // обычный день
            mark(None, "Б", None, "", ""),                            // больничный
            mark(Some(9 * 60), "", Some(17 * 60), "К", ""),           // ранний уход на встречу
            Mark {
                away: Some(90),
                ..mark(Some(9 * 60), "", Some(18 * 60), "", "")
            }, // отлучка без причины
            mark(Some(9 * 60 + 30), "", Some(18 * 60), "", ""),       // опоздание без причины
        ]
        .iter()
        .map(|m| calc_day(m, &r))
        .collect();

        let t = calc_tabel(&days, 48.0);
        let close = |a: f64, b: f64| assert!((a - b).abs() < 1e-9, "{a} != {b}");
        close(t.worked, 380.0 + 480.0 + 0.0 + 420.0 + 390.0 + 450.0);
        close(t.calc, 467.2 + 480.0 + 384.0 + 480.0 + 390.0 + 450.0);
        close(t.max, t.calc);
        close(t.usage, 2651.2 / 60.0 / 48.0);
        assert_eq!((t.late_n, t.late_n_calc), (2, 2));
        close(t.late_t, 139.0);
        close(t.late_t_calc, 21.8 + 30.0);
        assert_eq!((t.away_n, t.away_n_calc), (1, 1));
        close(t.away_t_calc, 90.0);
        // ранний уход с кодом «К» в табель не идёт
        assert_eq!((t.early_n, t.early_n_calc), (1, 0));
        close(t.early_t, 60.0);
        close(t.early_t_calc, 0.0);

        let s = calc_svodka(&t, 48.0, 1.0);
        close(s.norm, 2880.0);
        close(s.tl_time, 2120.0);
        close(s.tab_time, 2651.2);
        close(s.missed_tl, 760.0);
        close(s.missed_tab, 228.8);
        close(s.overtime, 0.0);
        assert_eq!((s.late_n, s.away_n, s.early_n), (2, 1, 0));
        close(s.late_t, 51.8);
        close(s.ktu, t.usage);
        // сотрудник без единой отметки: пропущенное время не считается (как в книге)
        let empty = calc_svodka(&calc_tabel(&[], 48.0), 48.0, 1.0);
        assert_eq!(
            (empty.missed_tl, empty.missed_tab, empty.ktu),
            (0.0, 0.0, 0.0)
        );
    }

    #[test]
    fn svodka_limits_ktu() {
        let days = vec![
            DayCalc {
                worked: 600.0,
                calc: 600.0,
                ..DayCalc::default()
            };
            21
        ];
        let t = calc_tabel(&days, 168.0);
        let s = calc_svodka(&t, 168.0, 1.0);
        assert!(s.usage > 1.0);
        assert_eq!(s.ktu, 1.0);
        assert_eq!(s.overtime, 21.0 * 600.0 - 168.0 * 60.0);
        assert_eq!(s.missed_tab, 0.0);
    }
}
