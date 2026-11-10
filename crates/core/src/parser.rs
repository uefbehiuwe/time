//! Разбор сообщений из группы Telegram.
//!
//! Правила группы (лист «Список» исходной книги): в начале дня пишут «офис»,
//! при выезде — «выезд (куда)», в конце дня — «убыл»/«уход». Время события —
//! время отправки сообщения, если в тексте не указано другое. На практике люди
//! пишут свободнее: «офис 9,30», «14.08 9.30 офис», «задержусь у врача»,
//! «я и Игорь на объекте», «уход из аэропорта». Этот модуль превращает такой
//! текст в структурированное событие; всё, что понять не удалось, помечается
//! [`Kind::Unknown`] и попадает табельщику на проверку.

use chrono::{Datelike, Duration, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::model::Minutes;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Приход: «офис», «в офисе», «на объекте».
    Arrive,
    /// Конец рабочего дня: «уход», «убыл».
    Leave,
    /// Выезд из офиса в течение дня: «выезд на объект».
    Away,
    /// Возврат в офис после выезда.
    Back,
    /// Предупреждение об опоздании: «задержусь», «буду позже».
    Late,
    /// Отсутствие весь день: «болею», «отгул», «отсутствую».
    Absent,
    /// Сообщение без влияния на отметки: «на связи», «прибыли в Ташкент».
    Info,
    /// Не распознано.
    Unknown,
}

/// Сотрудник, которого можно упомянуть в сообщении («я и Игорь на объекте»).
#[derive(Clone, Debug)]
pub struct Person {
    pub id: i64,
    /// Имена и их уменьшительные формы в нижнем регистре.
    pub aliases: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Parsed {
    pub kind: Kind,
    /// Время, явно указанное в тексте («офис 9,30»).
    pub time: Option<Minutes>,
    /// Даты, явно указанные в тексте («14.08 9.30 офис», «вчера»). Пусто — день отправки.
    pub dates: Vec<NaiveDate>,
    /// Категория причины: Б, Ф, К, Г, О, У.
    pub code: Option<String>,
    /// Код подставлен по умолчанию, а не выведен из текста.
    pub code_guessed: bool,
    /// Упомянутые сотрудники.
    pub mentions: Vec<i64>,
    /// Относится ли сообщение к самому автору.
    pub include_author: bool,
}

const LEAVE: &[&str] = &[
    "уход",
    "ухожу",
    "ушел",
    "ушла",
    "ушли",
    "убыл",
    "закончил",
    "домой",
    "в гостиниц",
];
const AWAY: &[&str] = &[
    "выезд",
    "выехал",
    "выезжа",
    "отъеду",
    "отъехал",
    "отлуч",
    "отойду",
    "отошел",
    "отошла",
    "уехал",
    "поехал",
    "еду в ",
    "еду на ",
    "едем в ",
    "едем на ",
];
const BACK: &[&str] = &["вернулся", "вернулась", "вернулись", "возврат", "возвращ"];
const LATE: &[&str] = &[
    "задерж",
    "опозда",
    "опазд",
    "подъеду",
    "подойду",
    "буду позже",
    "позже буду",
    "буду к ",
    "буду через",
    "буду после",
    "приеду к ",
    "приду к ",
];
const FUTURE: &[&str] = &["буду", "приду", "приеду", "подъеду", "подойду"];
const ABSENT: &[&str] = &[
    "отсутств",
    "болею",
    "заболел",
    "больничн",
    "отгул",
    "отпуск",
    "не выйду",
    "не приду",
    "не смогу прийти",
    "не буду",
    "за свой счет",
    "беру день",
];
const OFFICE: &[&str] = &["офис", "на месте", "на работе"];
const SITE: &[&str] = &["объект", "работаем", "работаю"];
const INFO: &[&str] = &[
    "на связи",
    "прибыл",
    "приехал",
    "прилетел",
    "долетел",
    "добрался",
    "добрались",
];
const LOCATION_START: &[&str] = &[
    "в ",
    "во ",
    "на ",
    "у ",
    "с утра",
    "проеду",
    "заеду",
    "заехал",
];

const HEALTH: &[&str] = &[
    "болею",
    "болен",
    "больн",
    "болезн",
    "заболе",
    "болит",
    "здоров",
    "самочувств",
    "чувству",
    "температур",
    "врач",
    "доктор",
    "поликлиник",
    "стоматолог",
    "медосмотр",
    "мед осмотр",
    "мед. осмотр",
    "медицин",
    "лечени",
    "лекарств",
    "простуд",
    "грипп",
    "травм",
    "анализ",
    "давлени",
    "отрави",
    "недомог",
    "госпитал",
    "скорая",
    "скорую",
];
const FAMILY: &[&str] = &[
    "семейн",
    "домашн",
    "обстоятельств",
    "садик",
    "школ",
    "похорон",
    "авари",
    "дтп",
    "затопи",
    "форс",
    "сем обст",
    "сем. обст",
    "дома с",
    "с ребенком",
    "с детьми",
    "с сыном",
    "с дочк",
    "с дочер",
    "с мамой",
    "с папой",
    "с женой",
    "с мужем",
    "с родител",
];
const BUSINESS: &[&str] = &[
    "объект",
    "аэропорт",
    "встреч",
    "банк",
    "налогов",
    "заказчик",
    "клиент",
    "командировк",
    "склад",
    "закуп",
    "таможн",
    "министерств",
    "тендер",
    "выезд",
];
const DAY_OFF: &[&str] = &["отгул", "за свой счет", "без содержания"];
const VACATION: &[&str] = &["отпуск"];
const STUDY: &[&str] = &["учебн", "учебу", "учебе", "сесси", "экзамен"];
/// Слова, которые не считаются пояснением к «офис».
const FILLER: &[&str] = &[
    "я",
    "и",
    "в",
    "во",
    "на",
    "уже",
    "сегодня",
    "всем",
    "привет",
    "доброе",
    "добрый",
    "утро",
    "день",
    "коллеги",
    "с",
    "к",
    "по",
    "мы",
    "тоже",
    "снова",
];
const MONTHS: &[(&str, u32)] = &[
    ("январ", 1),
    ("феврал", 2),
    ("март", 3),
    ("апрел", 4),
    ("мая", 5),
    ("май", 5),
    ("июн", 6),
    ("июл", 7),
    ("август", 8),
    ("сентябр", 9),
    ("октябр", 10),
    ("ноябр", 11),
    ("декабр", 12),
];

/// Нижний регистр, «ё» → «е», одиночные пробелы.
pub fn normalize(text: &str) -> String {
    let lowered = text.to_lowercase().replace('ё', "е");
    lowered.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn words(text: &str) -> Vec<&str> {
    text.split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Ключ с пробелом ищется как фраза, без пробела — как начало слова.
fn has(text: &str, ws: &[&str], keys: &[&str]) -> bool {
    keys.iter().any(|k| {
        if k.contains(' ') || k.contains('.') {
            text.contains(k)
        } else {
            ws.iter().any(|w| w.starts_with(k))
        }
    })
}

#[derive(Debug)]
struct NumTok {
    a: u32,
    b: u32,
    sep: char,
    year: Option<i32>,
    /// Позиция первого символа токена.
    pos: usize,
    /// После токена стоит точка и продолжается текст («30.09. уход»).
    dot_then_text: bool,
}

fn digits(cs: &[char]) -> u32 {
    cs.iter()
        .fold(0, |acc, c| acc * 10 + c.to_digit(10).unwrap_or(0))
}

/// Ищет пары вида `9.30`, `18:05`, `14.08`, `08.09.2026`.
fn scan_numbers(cs: &[char]) -> Vec<NumTok> {
    let n = cs.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if !cs[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let s = i;
        while i < n && cs[i].is_ascii_digit() {
            i += 1;
        }
        if i - s > 2
            || i + 1 >= n
            || !matches!(cs[i], '.' | ':' | ';' | ',')
            || !cs[i + 1].is_ascii_digit()
        {
            continue;
        }
        let sep = cs[i];
        let s2 = i + 1;
        let mut j = s2;
        while j < n && cs[j].is_ascii_digit() {
            j += 1;
        }
        if j - s2 > 2 {
            i = j;
            continue;
        }
        let (a, b) = (digits(&cs[s..i]), digits(&cs[s2..j]));
        let mut year = None;
        let mut end = j;
        if sep == '.' && j + 1 < n && cs[j] == '.' && cs[j + 1].is_ascii_digit() {
            let mut k = j + 1;
            while k < n && cs[k].is_ascii_digit() {
                k += 1;
            }
            match k - (j + 1) {
                4 => year = Some(digits(&cs[j + 1..k]) as i32),
                2 => year = Some(2000 + digits(&cs[j + 1..k]) as i32),
                _ => {
                    i = k;
                    continue;
                }
            }
            end = k;
        }
        let dot_then_text =
            end < n && cs[end] == '.' && cs[end + 1..].iter().any(|c| c.is_alphanumeric());
        out.push(NumTok {
            a,
            b,
            sep,
            year,
            pos: s,
            dot_then_text,
        });
        i = end;
    }
    out
}

/// Дата без года: ближайшая к дню отправки в окне ±60 дней.
fn resolve_date(day: u32, month: u32, year: Option<i32>, sent: NaiveDate) -> Option<NaiveDate> {
    if let Some(y) = year {
        return NaiveDate::from_ymd_opt(y, month, day);
    }
    [sent.year(), sent.year() - 1, sent.year() + 1]
        .into_iter()
        .filter_map(|y| NaiveDate::from_ymd_opt(y, month, day))
        .find(|d| (*d - sent).num_days().abs() <= 60)
}

struct Numbers {
    times: Vec<(usize, Minutes)>,
    dates: Vec<(usize, NaiveDate)>,
}

fn classify_numbers(
    cs: &[char],
    sent_date: NaiveDate,
    sent_time: Minutes,
    prefer_dates: bool,
) -> Numbers {
    let toks = scan_numbers(cs);
    let mut res = Numbers {
        times: Vec::new(),
        dates: Vec::new(),
    };
    let as_time = |t: &NumTok| {
        (t.year.is_none() && t.a <= 23 && t.b <= 59).then(|| (t.a * 60 + t.b) as Minutes)
    };
    let as_date = |t: &NumTok| {
        (t.sep == '.' && (1..=31).contains(&t.a) && (1..=12).contains(&t.b))
            .then(|| resolve_date(t.a, t.b, t.year, sent_date))
            .flatten()
    };
    // «Точно время»: разделитель не точка либо пара не может быть датой.
    let sure_time = |t: &NumTok| as_time(t).is_some() && (t.sep != '.' || as_date(t).is_none());
    for (idx, t) in toks.iter().enumerate() {
        let (time, date) = (as_time(t), as_date(t));
        let is_date = match (time, date) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(tm), Some(d)) => {
                // «17.09 уход 18:00»: если дальше в тексте есть явное время, пара с точкой — дата
                let time_follows = toks.iter().skip(idx + 1).any(sure_time);
                let yesterday_or_earlier = d < sent_date && (sent_date - d).num_days() <= 31;
                t.year.is_some()
                    || t.sep == '.'
                        && (prefer_dates
                            || t.dot_then_text
                            || time_follows
                            // «уход 18.10» утром 19-го: как время это ещё не наступило, значит дата
                            || yesterday_or_earlier && tm > sent_time + 30)
            }
        };
        if is_date {
            let d = date.expect("checked above");
            res.dates.push((t.pos, d));
            // «17, 18.09» — перечисление дней перед датой
            let mut p = t.pos;
            loop {
                let mut q = p;
                while q > 0 && cs[q - 1] == ' ' {
                    q -= 1;
                }
                if q == 0 || cs[q - 1] != ',' {
                    break;
                }
                q -= 1;
                while q > 0 && cs[q - 1] == ' ' {
                    q -= 1;
                }
                let e = q;
                while q > 0 && cs[q - 1].is_ascii_digit() {
                    q -= 1;
                }
                if e == q || e - q > 2 || (q > 0 && matches!(cs[q - 1], '.' | ':' | ',' | ';')) {
                    break;
                }
                match resolve_date(digits(&cs[q..e]), d.month(), Some(d.year()), sent_date) {
                    Some(extra) => res.dates.push((q, extra)),
                    None => break,
                }
                p = q;
            }
        } else if let Some(tm) = time {
            res.times.push((t.pos, tm));
        }
    }
    // «7 августа»
    let mut i = 0;
    while i < cs.len() {
        if cs[i].is_ascii_digit()
            && (i == 0
                || !(cs[i - 1].is_ascii_digit() || matches!(cs[i - 1], '.' | ':' | ',' | ';')))
        {
            let s = i;
            while i < cs.len() && cs[i].is_ascii_digit() {
                i += 1;
            }
            if i - s <= 2 && i + 1 < cs.len() && cs[i] == ' ' {
                let rest: String = cs[i + 1..].iter().take(10).collect();
                if let Some((_, m)) = MONTHS.iter().find(|(stem, _)| rest.starts_with(stem)) {
                    if let Some(d) = resolve_date(digits(&cs[s..i]), *m, None, sent_date) {
                        res.dates.push((s, d));
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    res.dates.sort();
    res.dates.dedup_by_key(|(_, d)| *d);
    res
}

/// Позиция (в символах) отдельного слова «до» — после него идёт пояснение, где был до офиса.
fn errand_clause_start(cs: &[char]) -> Option<usize> {
    let is_alpha = |c: char| c.is_alphabetic();
    (0..cs.len().saturating_sub(1)).find(|&i| {
        cs[i] == 'д'
            && cs[i + 1] == 'о'
            && (i == 0 || !is_alpha(cs[i - 1]))
            && (i + 2 >= cs.len() || !is_alpha(cs[i + 2]))
    })
}

/// Разбирает одно сообщение. `sent_date`/`sent_time` — местные дата и время отправки.
pub fn parse_message(
    text: &str,
    sent_date: NaiveDate,
    sent_time: Minutes,
    people: &[Person],
) -> Parsed {
    let t = normalize(text);
    let ws = words(&t);
    let cs: Vec<char> = t.chars().collect();
    let mut out = Parsed {
        kind: Kind::Unknown,
        time: None,
        dates: Vec::new(),
        code: None,
        code_guessed: false,
        mentions: Vec::new(),
        include_author: true,
    };
    if t.is_empty() {
        return out;
    }
    if t.starts_with('/') {
        out.kind = Kind::Info;
        return out;
    }

    // --- упоминания
    for p in people {
        if p.aliases.iter().any(|a| ws.iter().any(|w| w == a)) {
            out.mentions.push(p.id);
        }
    }
    out.include_author = out.mentions.is_empty() || ws.contains(&"я");

    // --- вид события
    let health = has(&t, &ws, HEALTH);
    let family = has(&t, &ws, FAMILY);
    let office = has(&t, &ws, OFFICE);
    let site = has(&t, &ws, SITE);
    let care = t.contains("уход за ");
    let leave = has(&t, &ws, LEAVE) && !care;
    let future = has(&t, &ws, FUTURE);
    let location_start = LOCATION_START.iter().any(|p| t.starts_with(p));
    out.kind = if leave {
        Kind::Leave
    } else if has(&t, &ws, AWAY) {
        Kind::Away
    } else if has(&t, &ws, LATE) {
        if office && !future {
            Kind::Arrive
        } else {
            Kind::Late
        }
    } else if has(&t, &ws, ABSENT) || care {
        Kind::Absent
    } else if has(&t, &ws, BACK) {
        if office || ws.len() <= 3 {
            Kind::Back
        } else {
            Kind::Info
        }
    } else if office || site {
        Kind::Arrive
    } else if has(&t, &ws, INFO) {
        Kind::Info
    } else if location_start {
        // «в поликлинике» — предупреждение, «в аэропорту» — работа вне офиса
        if health || family {
            Kind::Late
        } else {
            Kind::Arrive
        }
    } else {
        Kind::Unknown
    };

    // --- даты и время
    let nums = classify_numbers(&cs, sent_date, sent_time, out.kind == Kind::Absent);
    out.dates = nums.dates.iter().map(|(_, d)| *d).collect();
    if !t.contains("до завтра") && ws.contains(&"завтра") {
        out.dates.push(sent_date + Duration::days(1));
    }
    if ws.contains(&"вчера") {
        out.dates.push(sent_date - Duration::days(1));
    }
    if out.kind == Kind::Absent
        && out.dates.len() == 2
        && (ws.contains(&"по") || t.contains('-') || t.contains('–'))
    {
        // «отпуск с 12.11 по 20.11» — разворачиваем диапазон
        let (from, to) = (
            out.dates[0].min(out.dates[1]),
            out.dates[0].max(out.dates[1]),
        );
        if (to - from).num_days() <= 62 {
            out.dates = from.iter_days().take_while(|d| *d <= to).collect();
        }
    }
    out.dates.sort();
    out.dates.dedup();
    let errand = errand_clause_start(&cs);
    if matches!(
        out.kind,
        Kind::Arrive | Kind::Leave | Kind::Away | Kind::Back
    ) {
        out.time = nums
            .times
            .iter()
            .find(|(pos, _)| out.kind != Kind::Arrive || errand.is_none_or(|e| *pos < e))
            .map(|(_, tm)| *tm);
    }

    // --- категория причины
    let code = |c: &str| Some(c.to_string());
    match out.kind {
        Kind::Arrive => {
            let named: Vec<&str> = people
                .iter()
                .flat_map(|p| p.aliases.iter().map(String::as_str))
                .collect();
            let explained = ws.iter().any(|w| {
                !OFFICE.iter().any(|o| !o.contains(' ') && w.starts_with(o))
                    && !FILLER.contains(w)
                    && !named.contains(w)
            });
            if health {
                out.code = code("Б");
            } else if family {
                out.code = code("Ф");
            } else if !office {
                // работа вне офиса: «на объекте», «в аэропорту»
                out.code = code("К");
                out.code_guessed = !site && !has(&t, &ws, BUSINESS);
            } else if explained {
                // «офис до аэропорт», «офис, со встречи в банке»
                out.code = code("К");
            }
        }
        Kind::Leave => {
            let from_place = ["уход с ", "уход со ", "уход из ", "убыл с ", "убыл из "]
                .iter()
                .any(|p| {
                    t.find(p).is_some_and(|i| {
                        !t[i + p.len()..].starts_with("офис")
                            && !t[i + p.len()..].starts_with("работ")
                    })
                });
            if has(&t, &ws, BUSINESS) || from_place || site {
                out.code = code("К");
            } else if health {
                out.code = code("Б");
            } else if family || t.contains("согласован") {
                out.code = code("Ф");
            }
        }
        Kind::Late => {
            if health {
                out.code = code("Б");
            } else if family {
                out.code = code("Ф");
            } else if has(&t, &ws, BUSINESS) {
                out.code = code("К");
            }
        }
        Kind::Absent => {
            if has(&t, &ws, DAY_OFF) {
                out.code = code("Г");
            } else if has(&t, &ws, VACATION) {
                out.code = code("О");
            } else if has(&t, &ws, STUDY) {
                out.code = code("У");
            } else if health || care {
                out.code = code("Б");
            } else if family {
                out.code = code("Ф");
            } else if has(&t, &ws, BUSINESS) {
                out.code = code("К");
            } else {
                out.code = code("Ф");
                out.code_guessed = true;
            }
        }
        Kind::Away => {
            out.code = if health {
                code("Б")
            } else if family {
                code("Ф")
            } else {
                code("К")
            };
        }
        Kind::Back | Kind::Info | Kind::Unknown => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }

    fn people() -> Vec<Person> {
        let p = |id, a: &[&str]| Person {
            id,
            aliases: a.iter().map(|s| s.to_string()).collect(),
        };
        vec![
            p(1, &["олег"]),
            p(2, &["тимур"]),
            p(3, &["дмитрий", "дима"]),
            p(4, &["игорь"]),
        ]
    }

    /// Разбор сообщения, отправленного 15.10.2026 (чт) в 10:00.
    fn p(text: &str) -> Parsed {
        parse_message(text, d(10, 15), 600, &people())
    }

    fn at(text: &str, date: NaiveDate, time: Minutes) -> Parsed {
        parse_message(text, date, time, &people())
    }

    #[test]
    fn plain_marks() {
        assert_eq!(normalize("  Пришёл   В ОФИС "), "пришел в офис");
        assert_eq!(p("Офис").kind, Kind::Arrive);
        assert_eq!(p("В офисе").kind, Kind::Arrive);
        assert_eq!(p("офис").code, None);
        assert_eq!(p("Доброе утро, в офисе").code, None);
        assert_eq!(p("Уход").kind, Kind::Leave);
        assert_eq!(p("Убыл").kind, Kind::Leave);
        assert_eq!(p("Уход из офиса").code, None);
        assert_eq!(p("На связи").kind, Kind::Info);
        assert_eq!(p("Спасибо!").kind, Kind::Unknown);
    }

    #[test]
    fn explicit_time_in_text() {
        assert_eq!(p("Офис 9,30").time, Some(570));
        assert_eq!(p("Офис 9:10").time, Some(550));
        assert_eq!(p("офис 10.20").time, Some(620));
        assert_eq!(p("В офисе 8:50").time, Some(530));
        assert_eq!(p("Уход 17.30").time, Some(1050));
        assert_eq!(p("Уход 18,00").time, Some(1080));
        assert_eq!(p("Уход в 18:23").time, Some(1103));
        assert_eq!(p("Уход 19.0").time, Some(1140));
        assert_eq!(p("Уход 18.13").time, Some(1093));
        assert!(p("Уход 17.30").dates.is_empty());
        // время внутри пояснения «до …» не является временем прихода
        let m = p("Я и Игорь в офисе. До с 9:00 в министерстве");
        assert_eq!(
            (m.kind, m.time, m.code.as_deref()),
            (Kind::Arrive, None, Some("К"))
        );
        let m = p("Офис 11:00 до в поликлинике");
        assert_eq!((m.time, m.code.as_deref()), (Some(660), Some("Б")));
        let m = p("Офис 11;00 до ездил за оборудованием");
        assert_eq!((m.time, m.code.as_deref()), (Some(660), Some("К")));
    }

    #[test]
    fn backdated_marks() {
        let m = at("14.08 9.30 офис", d(8, 15), 700);
        assert_eq!(
            (m.kind, m.time, m.dates.clone()),
            (Kind::Arrive, Some(570), vec![d(8, 14)])
        );
        let m = at("17.08 уход 18,00", d(8, 18), 560);
        assert_eq!(
            (m.kind, m.time, m.dates.clone()),
            (Kind::Leave, Some(1080), vec![d(8, 17)])
        );
        let m = at("Уход 24.08 в 18:00", d(8, 25), 560);
        assert_eq!((m.time, m.dates.clone()), (Some(1080), vec![d(8, 24)]));
        let m = at("08.09.2026 уход 17.30", d(9, 9), 560);
        assert_eq!((m.time, m.dates.clone()), (Some(1050), vec![d(9, 8)]));
        let m = at("30.09. уход 17.30", d(10, 1), 560);
        assert_eq!((m.time, m.dates.clone()), (Some(1050), vec![d(9, 30)]));
        let m = at("05.10. уход 18,00", d(10, 6), 560);
        assert_eq!((m.time, m.dates.clone()), (Some(1080), vec![d(10, 5)]));
        let m = at("7.10 офис 12:00", d(10, 8), 813);
        assert_eq!(
            (m.kind, m.time, m.dates.clone()),
            (Kind::Arrive, Some(720), vec![d(10, 7)])
        );
        let m = at(
            "Уход 03.09 в 16.50 поехала с папой в больницу",
            d(9, 4),
            560,
        );
        assert_eq!(
            (m.kind, m.time, m.dates.clone()),
            (Kind::Leave, Some(1010), vec![d(9, 3)])
        );
        // дата того же дня
        let m = at("17.09 уход 18:00", d(9, 17), 1137);
        assert_eq!((m.time, m.dates.clone()), (Some(1080), vec![d(9, 17)]));
        // перечисление дней
        let m = at("17, 18.09 офис 10.00", d(9, 18), 1186);
        assert_eq!(
            (m.kind, m.time, m.dates.clone()),
            (Kind::Arrive, Some(600), vec![d(9, 17), d(9, 18)])
        );
        // «18.10» вечером 19-го — это время, утром — вчерашняя дата
        assert_eq!(at("Уход 18.10", d(10, 19), 1092).time, Some(1090));
        assert_eq!(at("Уход 18.10", d(10, 19), 545).dates, vec![d(10, 18)]);
        assert_eq!(
            at("Вчера уход 19:40", d(10, 16), 545).dates,
            vec![d(10, 15)]
        );
        // давняя дата и два времени в одном сообщении
        let m = at("17.09 уход 18:00", d(11, 11), 600);
        assert_eq!((m.time, m.dates.clone()), (Some(1080), vec![d(9, 17)]));
        // время перед «18.10» — значит, это тоже время, а не дата
        assert!(at("Офис 9.30, уход 18.10", d(10, 19), 1095)
            .dates
            .is_empty());
        assert!(p("Уход. До завтра").dates.is_empty());
    }

    #[test]
    fn late_and_absent() {
        let m = p("Задержусь в поликлинике");
        assert_eq!((m.kind, m.code.as_deref()), (Kind::Late, Some("Б")));
        let m = p("Задержусь по семейным обстоятельствам");
        assert_eq!((m.kind, m.code.as_deref()), (Kind::Late, Some("Ф")));
        let m = p("Буду позже, плохо себя чувствую");
        assert_eq!((m.kind, m.code.as_deref()), (Kind::Late, Some("Б")));
        assert_eq!(p("Задержусь").code, None);
        assert_eq!(p("Задержусь, буду в офисе к 11").kind, Kind::Late);
        let m = p("Задержусь по сем обст- офис");
        assert_eq!((m.kind, m.code.as_deref()), (Kind::Arrive, Some("Ф")));
        let m = p("Отсутствие по состоянию здоровья (на связи)");
        assert_eq!(
            (m.kind, m.code.as_deref(), m.code_guessed),
            (Kind::Absent, Some("Б"), false)
        );
        let m = p("Отсутствие, на связи");
        assert_eq!(
            (m.kind, m.code.as_deref(), m.code_guessed),
            (Kind::Absent, Some("Ф"), true)
        );
        assert_eq!(p("Болею").code.as_deref(), Some("Б"));
        assert_eq!(p("Сегодня отгул").code.as_deref(), Some("Г"));
        let m = at(
            "7 августа отсутствовал. Был в больнице с мамой",
            d(8, 10),
            540,
        );
        assert_eq!(
            (m.kind, m.code.as_deref(), m.dates.clone()),
            (Kind::Absent, Some("Б"), vec![d(8, 7)])
        );
        let m = at("Завтра не выйду, отгул", d(10, 15), 1000);
        assert_eq!(
            (m.kind, m.code.as_deref(), m.dates.clone()),
            (Kind::Absent, Some("Г"), vec![d(10, 16)])
        );
        let m = at("Отпуск с 12.11 по 14.11", d(11, 10), 600);
        assert_eq!(
            (m.code.as_deref(), m.dates.clone()),
            (Some("О"), vec![d(11, 12), d(11, 13), d(11, 14)])
        );
        assert_eq!(p("В поликлинике, буду позже").kind, Kind::Late);
    }

    #[test]
    fn work_outside_office() {
        let m = p("Офис до аэропорт");
        assert_eq!((m.kind, m.code.as_deref()), (Kind::Arrive, Some("К")));
        let m = p("Офис. По пути склад");
        assert_eq!(m.code.as_deref(), Some("К"));
        let m = p("В аэропорту");
        assert_eq!(
            (m.kind, m.code.as_deref(), m.code_guessed),
            (Kind::Arrive, Some("К"), false)
        );
        let m = p("Проеду через завод");
        assert_eq!(
            (m.kind, m.code.as_deref(), m.code_guessed),
            (Kind::Arrive, Some("К"), true)
        );
        let m = p("Уход с объекта");
        assert_eq!((m.kind, m.code.as_deref()), (Kind::Leave, Some("К")));
        assert_eq!(p("Уход из аэропорта").code.as_deref(), Some("К"));
        assert_eq!(p("Уход с гос резерва").code.as_deref(), Some("К"));
        assert_eq!(p("Уход. Согласовано").code.as_deref(), Some("Ф"));
        assert_eq!(p("Уход по состоянию здоровья").code.as_deref(), Some("Б"));
        let m = p("14:30 выезд на объект");
        assert_eq!(
            (m.kind, m.time, m.code.as_deref()),
            (Kind::Away, Some(870), Some("К"))
        );
        assert_eq!(p("Вернулся в офис").kind, Kind::Back);
        assert_eq!(p("Вернулись в Ташкент поздно ночью").kind, Kind::Info);
        assert_eq!(p("Прибыли в Ташкент").kind, Kind::Info);
    }

    #[test]
    fn mentions() {
        let m = p("Я, Дима и Игорь на объекте с 8:30");
        assert_eq!(
            (m.kind, m.time, m.code.as_deref()),
            (Kind::Arrive, Some(510), Some("К"))
        );
        assert_eq!((m.mentions.clone(), m.include_author), (vec![3, 4], true));
        let m = p("Уход из аэропорта я и Тимур");
        assert_eq!(
            (m.kind, m.mentions.clone(), m.include_author),
            (Kind::Leave, vec![2], true)
        );
        let m = p("Дима и Игорь на объекте");
        assert_eq!((m.mentions.clone(), m.include_author), (vec![3, 4], false));
        let m = p("Я, Дима и Игорь выехали в Самарканд в 14:15");
        assert_eq!((m.kind, m.time), (Kind::Away, Some(855)));
        let m = p("Я и Игорь, на сегодня работу закончили");
        assert_eq!((m.kind, m.mentions.clone()), (Kind::Leave, vec![4]));
        // имя в косвенном падеже упоминанием не считается
        assert!(p("Уход. Согласовано с Олегом").mentions.is_empty());
    }
}
