//! Разбор текста, скопированного из чата Telegram (как на листе «Telegram» исходной книги).
//!
//! Поддерживаются оба формата, в которых Telegram Desktop копирует сообщения:
//!
//! ```text
//! [02.10.2026 8:44] Имя Фамилия: Офис
//! ```
//!
//! ```text
//! Имя Фамилия, [02.10.2026 8:44]
//! Офис
//! ```

use chrono::{NaiveDate, NaiveDateTime};

#[derive(Debug, PartialEq)]
pub struct ChatLine {
    pub sent_at: NaiveDateTime,
    pub sender: String,
    pub text: String,
}

/// `02.10.2026 8:44`, `02.10.26 8:44`, `02.10.2026 8:44:05`.
fn parse_stamp(s: &str) -> Option<NaiveDateTime> {
    let (date, time) = s.trim().split_once(' ')?;
    let mut d = date.split('.').map(|p| p.parse::<u32>().ok());
    let (day, month, year) = (d.next()??, d.next()??, d.next()??);
    let year = if year < 100 { 2000 + year } else { year } as i32;
    let mut t = time.trim().split(':').map(|p| p.parse::<u32>().ok());
    let (h, m) = (t.next()??, t.next()??);
    let sec = t.next().flatten().unwrap_or(0);
    NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(h, m, sec)
}

/// Возвращает сообщения и число строк, которые не удалось разобрать.
pub fn parse_chat(text: &str) -> (Vec<ChatLine>, usize) {
    let mut out: Vec<ChatLine> = Vec::new();
    let mut skipped = 0;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // [дата время] Имя: текст
        if let Some(rest) = line.strip_prefix('[') {
            if let Some((stamp, tail)) = rest.split_once(']') {
                if let (Some(sent_at), Some((sender, body))) =
                    (parse_stamp(stamp), tail.split_once(':'))
                {
                    out.push(ChatLine {
                        sent_at,
                        sender: sender.trim().to_string(),
                        text: body.trim().to_string(),
                    });
                    continue;
                }
            }
        }
        // Имя, [дата время]
        if let Some((sender, stamp)) = line.strip_suffix(']').and_then(|l| l.rsplit_once(", [")) {
            if let Some(sent_at) = parse_stamp(stamp) {
                out.push(ChatLine {
                    sent_at,
                    sender: sender.trim().to_string(),
                    text: String::new(),
                });
                continue;
            }
        }
        // продолжение предыдущего сообщения
        match out.last_mut() {
            Some(last) => {
                if !last.text.is_empty() {
                    last.text.push('\n');
                }
                last.text.push_str(line);
            }
            None => skipped += 1,
        }
    }
    let before = out.len();
    out.retain(|l| !l.text.is_empty() && !l.sender.is_empty());
    let dropped = before - out.len();
    (out, skipped + dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    #[test]
    fn bracket_first_format() {
        let (lines, skipped) = parse_chat(
            "ИМПОРТ сообщений\n[02.10.2026 8:44] Олег Сомов: Офис\n[02.10.26 18:09] Тимур Раевский: Уход: согласовано\nдо завтра\n",
        );
        assert_eq!(skipped, 1);
        assert_eq!(
            lines[0],
            ChatLine {
                sent_at: at(2, 8, 44),
                sender: "Олег Сомов".into(),
                text: "Офис".into()
            }
        );
        assert_eq!(
            lines[1],
            ChatLine {
                sent_at: at(2, 18, 9),
                sender: "Тимур Раевский".into(),
                text: "Уход: согласовано\nдо завтра".into()
            }
        );
    }

    #[test]
    fn name_first_format() {
        let (lines, skipped) = parse_chat("Олег Сомов, [05.10.2026 9:07]\nЗадержусь по семейным\n\nТимур Раевский, [05.10.2026 9:08]\nОфис\n");
        assert_eq!(skipped, 0);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            (
                lines[0].sender.as_str(),
                lines[0].text.as_str(),
                lines[0].sent_at
            ),
            ("Олег Сомов", "Задержусь по семейным", at(5, 9, 7))
        );
        assert_eq!(lines[1].text, "Офис");
    }
}
