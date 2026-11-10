//! Ввод и вывод времени в формате «ч:мм», как в Excel.

use crate::model::Minutes;

/// Разбирает ввод пользователя: `9`, `930`, `9:30`, `9.30`, `9,30`, `09-30`, `9 30`.
///
/// Для времени суток допустимо 0:00–23:59, для длительности (`duration`) — до 23:59,
/// при этом голое число больше 23 читается как минуты (`45` → 0:45).
pub fn parse_time(input: &str, duration: bool) -> Option<Minutes> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let (h, m) = if let Some(pos) = s.find(|c: char| !c.is_ascii_digit()) {
        let (a, rest) = s.split_at(pos);
        let b = rest.trim_start_matches([':', '.', ',', ';', '-', ' ', 'ч']);
        if a.is_empty() || b.is_empty() || !b.chars().all(|c| c.is_ascii_digit()) || b.len() > 2 {
            return None;
        }
        let mut minutes: i32 = b.parse().ok()?;
        // «9.3» в Excel-привычке означает 9:30
        if b.len() == 1 && rest.starts_with(['.', ',']) {
            minutes *= 10;
        }
        (a.parse::<i32>().ok()?, minutes)
    } else {
        match s.len() {
            1 | 2 => {
                let v: i32 = s.parse().ok()?;
                if duration && v > 23 {
                    (0, v)
                } else {
                    (v, 0)
                }
            }
            3 => (s[..1].parse().ok()?, s[1..].parse().ok()?),
            4 => (s[..2].parse().ok()?, s[2..].parse().ok()?),
            _ => return None,
        }
    };
    if !(0..=23).contains(&h) || !(0..=59).contains(&m) {
        return None;
    }
    Some(h * 60 + m)
}

/// `545` → `9:05`.
pub fn fmt_hm(minutes: Minutes) -> String {
    format!("{}:{:02}", minutes / 60, minutes % 60)
}

/// Длительность в минутах (дробная) → `[ч]:мм`; секунды отбрасываются, как в формате Excel.
pub fn fmt_duration(minutes: f64) -> String {
    let total = (minutes + 1e-6).floor() as i64;
    format!("{}:{:02}", total / 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_inputs() {
        assert_eq!(parse_time("9", false), Some(540));
        assert_eq!(parse_time("930", false), Some(570));
        assert_eq!(parse_time("0930", false), Some(570));
        assert_eq!(parse_time("9:30", false), Some(570));
        assert_eq!(parse_time("9.30", false), Some(570));
        assert_eq!(parse_time("9,30", false), Some(570));
        assert_eq!(parse_time("9.3", false), Some(570));
        assert_eq!(parse_time("18-05", false), Some(1085));
        assert_eq!(parse_time(" 18 05 ", false), Some(1085));
        assert_eq!(parse_time("24:00", false), None);
        assert_eq!(parse_time("9:75", false), None);
        assert_eq!(parse_time("abc", false), None);
        assert_eq!(parse_time("45", false), None);
        assert_eq!(parse_time("45", true), Some(45));
        assert_eq!(parse_time("1:30", true), Some(90));
    }

    #[test]
    fn formats() {
        assert_eq!(fmt_hm(545), "9:05");
        assert_eq!(fmt_duration(10_080.0), "168:00");
        assert_eq!(fmt_duration(21.8), "0:21");
    }
}
