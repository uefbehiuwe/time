//! Обмен с Excel: импорт исходной книги «TimeList-ГГГГ-М.xlsm» и выгрузка месяца в .xlsx.

use std::io::Cursor;

use anyhow::{anyhow, bail, Context, Result};
use calamine::{open_workbook_auto_from_rs, Data, Range, Reader};
use chrono::{Datelike, Duration, NaiveDate};
use rust_xlsxwriter::{Color, Format, FormatAlign, FormatBorder, Workbook, Worksheet};
use serde::Serialize;
use timelist_core::timefmt::parse_time;
use timelist_core::{Mark, Minutes};

use crate::db::{Db, Employee, NewMessage};
use crate::service::{import_manual_day, MonthView};
use crate::tgtext::parse_chat;

#[derive(Debug, Default, Serialize)]
pub struct ImportReport {
    /// Месяц книги, `ГГГГ-ММ`.
    pub month: Option<String>,
    pub employees_added: usize,
    pub employees_total: usize,
    pub mark_days: usize,
    pub messages_added: usize,
    pub messages_skipped: usize,
    pub notes: Vec<String>,
}

fn cell(range: &Range<Data>, row: u32, col: u32) -> &Data {
    range.get_value((row, col)).unwrap_or(&Data::Empty)
}

fn text(v: &Data) -> Option<String> {
    match v {
        Data::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        _ => None,
    }
}

fn number(v: &Data) -> Option<f64> {
    match v {
        Data::Float(f) => Some(*f),
        Data::Int(i) => Some(*i as f64),
        Data::DateTime(dt) => Some(dt.as_f64()),
        _ => None,
    }
}

/// Время суток или длительность из ячейки Excel (доля суток) либо из текста «9:30».
fn minutes(v: &Data) -> Option<Minutes> {
    match v {
        Data::String(s) => parse_time(s, false),
        other => number(other)
            .map(|f| (f.fract() * 1440.0).round() as Minutes)
            .filter(|m| *m > 0),
    }
}

fn excel_date(v: &Data) -> Option<NaiveDate> {
    let serial = number(v)?;
    if !(1.0..200_000.0).contains(&serial) {
        return None;
    }
    NaiveDate::from_ymd_opt(1899, 12, 30).map(|epoch| epoch + Duration::days(serial.floor() as i64))
}

/// Импортирует книгу: сотрудников, коэффициенты, нормы, отметки месяца (как ручные) и сообщения.
pub fn import_workbook(db: &Db, bytes: Vec<u8>) -> Result<ImportReport> {
    let mut book =
        open_workbook_auto_from_rs(Cursor::new(bytes)).context("не удалось открыть книгу Excel")?;
    let mut report = ImportReport::default();
    let mut sheet = |name: &str| -> Option<Range<Data>> { book.worksheet_range(name).ok() };

    // --- TimeList: месяц и отметки
    let timelist = sheet("TimeList").ok_or_else(|| anyhow!("в книге нет листа «TimeList»"))?;
    let first = excel_date(cell(&timelist, 1, 7))
        .ok_or_else(|| anyhow!("в ячейке TimeList!H2 нет даты начала месяца"))?;
    let (year, month) = (first.year(), first.month());
    report.month = Some(format!("{year}-{month:02}"));

    // --- Список: сотрудники, коды, нормы
    let mut names: Vec<(String, String, String)> = Vec::new();
    if let Some(list) = sheet("Список") {
        for row in 2..=17 {
            if let (Some(full_name), Some(nick)) =
                (text(cell(&list, row, 2)), text(cell(&list, row, 3)))
            {
                names.push((
                    nick,
                    full_name,
                    text(cell(&list, row, 4)).unwrap_or_default(),
                ));
            }
        }
        for row in (20..=26).chain(29..=31) {
            if let (Some(code), Some(coef)) =
                (text(cell(&list, row, 1)), number(cell(&list, row, 2)))
            {
                if !db.update_code(&code, coef, None, None)? {
                    report
                        .notes
                        .push(format!("код «{code}» из книги не найден в справочнике"));
                }
            }
        }
        for row in 4..=15 {
            let month_no = number(cell(&list, row, 10)).map(|m| m as u32);
            if let (Some(m), Some(hours), Some(days)) = (
                month_no,
                number(cell(&list, row, 12)),
                number(cell(&list, row, 13)),
            ) {
                if (1..=12).contains(&m) && hours > 0.0 {
                    db.set_month_norm(year, m, Some((hours, days as i64)))?;
                }
            }
        }
    } else {
        report
            .notes
            .push("нет листа «Список» — сотрудники взяты из заголовков тайм-листа".into());
    }
    if let Some(svodka) = sheet("Сводка") {
        if let Some(limit) = number(cell(&svodka, 1, 4)).filter(|l| *l > 0.0) {
            db.set_month_limit(year, month, limit)?;
        }
    }

    // --- блоки сотрудников: 8 колонок на человека, начиная с J
    for block in 0..16u32 {
        let c0 = 9 + block * 8;
        let Some(nick) = text(cell(&timelist, 1, c0)) else {
            continue;
        };
        let known = names.iter().find(|(n, _, _)| *n == nick);
        let employee_id = match db.employee_by_nick(&nick)? {
            Some(e) => e.id,
            None => {
                report.employees_added += 1;
                db.insert_employee(&Employee {
                    id: 0,
                    full_name: known.map(|k| k.1.clone()).unwrap_or_else(|| nick.clone()),
                    nick: nick.clone(),
                    tg_name: known.map(|k| k.2.clone()).unwrap_or_default(),
                    tg_user_id: None,
                    aliases: String::new(),
                    auto_schedule: false,
                    active: true,
                    sort: 0,
                    works_from: None,
                    works_to: None,
                })?
            }
        };
        report.employees_total += 1;
        for row in 3..=37 {
            let Some(date) = excel_date(cell(&timelist, row, 7)) else {
                continue;
            };
            if date.year() != year || date.month() != month {
                continue;
            }
            let mark = Mark {
                start: minutes(cell(&timelist, row, c0)),
                start_code: text(cell(&timelist, row, c0 + 1)),
                away: minutes(cell(&timelist, row, c0 + 2)),
                away_code: text(cell(&timelist, row, c0 + 3)),
                end: minutes(cell(&timelist, row, c0 + 4)),
                end_code: text(cell(&timelist, row, c0 + 5)),
                day_code: text(cell(&timelist, row, c0 + 6)),
            };
            if !mark.is_empty() {
                report.mark_days += 1;
            }
            // весь месяц фиксируется как ручной ввод: цифры должны совпасть с книгой
            import_manual_day(db, employee_id, date, mark)?;
        }
    }
    if report.employees_total == 0 {
        bail!("в строке 2 листа «TimeList» не найдено ни одного ника сотрудника");
    }

    // --- Telegram: скопированный чат в колонке B
    if let Some(chat) = sheet("Telegram") {
        let mut pasted = String::new();
        let rows = chat.end().map(|(r, _)| r).unwrap_or(0);
        for row in 3..=rows {
            if let Some(line) = text(cell(&chat, row, 1)) {
                pasted.push_str(&line);
                pasted.push('\n');
            }
        }
        let (added, skipped) = import_chat_text(db, &pasted)?;
        report.messages_added = added;
        report.messages_skipped = skipped;
    }
    Ok(report)
}

/// Сохраняет сообщения из скопированного чата. Возвращает (добавлено, пропущено).
pub fn import_chat_text(db: &Db, pasted: &str) -> Result<(usize, usize)> {
    let (lines, mut skipped) = parse_chat(pasted);
    let mut added = 0;
    for l in lines {
        let m = NewMessage {
            source: "import",
            chat_id: None,
            tg_message_id: None,
            tg_user_id: None,
            sender_name: &l.sender,
            employee_id: None,
            sent_at: l.sent_at,
            text: &l.text,
        };
        if db.upsert_message(&m)?.1 {
            added += 1;
        } else {
            skipped += 1;
        }
    }
    Ok((added, skipped))
}

// ---------------------------------------------------------------- выгрузка

struct Styles {
    title: Format,
    head: Format,
    label: Format,
    text: Format,
    time: Format,
    total: Format,
    percent: Format,
    int: Format,
    weekend: Format,
    holiday: Format,
}

fn styles() -> Styles {
    let base = || {
        Format::new()
            .set_border(FormatBorder::Thin)
            .set_border_color(Color::RGB(0xBFC8D6))
    };
    Styles {
        title: Format::new().set_bold().set_font_size(14),
        head: base()
            .set_bold()
            .set_align(FormatAlign::Center)
            .set_background_color(Color::RGB(0xDCE8FA)),
        label: base().set_align(FormatAlign::Left),
        text: base().set_align(FormatAlign::Center),
        time: base().set_align(FormatAlign::Center).set_num_format("h:mm"),
        total: base()
            .set_align(FormatAlign::Center)
            .set_num_format("[h]:mm"),
        percent: base().set_align(FormatAlign::Center).set_num_format("0.0%"),
        int: base().set_align(FormatAlign::Center).set_num_format("0"),
        weekend: base()
            .set_align(FormatAlign::Center)
            .set_background_color(Color::RGB(0xFDE3E3)),
        holiday: base()
            .set_align(FormatAlign::Center)
            .set_background_color(Color::RGB(0xF9C7C7)),
    }
}

fn days(minutes: f64) -> f64 {
    minutes / 1440.0
}

fn write_time(
    ws: &mut Worksheet,
    row: u32,
    col: u16,
    v: Option<Minutes>,
    f: &Format,
) -> Result<()> {
    match v {
        Some(m) => ws.write_number_with_format(row, col, days(m as f64), f)?,
        None => ws.write_blank(row, col, f)?,
    };
    Ok(())
}

fn write_code(
    ws: &mut Worksheet,
    row: u32,
    col: u16,
    v: &Option<String>,
    f: &Format,
) -> Result<()> {
    ws.write_string_with_format(row, col, v.as_deref().unwrap_or(""), f)?;
    Ok(())
}

fn write_day_columns(
    ws: &mut Worksheet,
    view: &MonthView,
    st: &Styles,
    first_row: u32,
) -> Result<()> {
    for (i, day) in view.days.iter().enumerate() {
        let row = first_row + i as u32;
        let f = match day.kind {
            timelist_core::calendar::DayKind::Holiday => &st.holiday,
            timelist_core::calendar::DayKind::Weekend => &st.weekend,
            _ => &st.text,
        };
        ws.write_number_with_format(row, 0, i as f64 + 1.0, &st.int)?;
        ws.write_string_with_format(row, 1, day.dow, f)?;
        ws.write_string_with_format(row, 2, day.date.format("%d.%m.%Y").to_string(), f)?;
    }
    Ok(())
}

/// Книга с листами «TimeList», «Табель» и «Сводка» — те же колонки, значения вместо формул.
pub fn export_month(view: &MonthView) -> Result<Vec<u8>> {
    let st = styles();
    let mut book = Workbook::new();

    // --- TimeList
    let ws = book.add_worksheet();
    ws.set_name("TimeList")?;
    ws.write_string_with_format(0, 0, format!("ТАЙМ-ЛИСТ · {}", view.title), &st.title)?;
    for (c, h) in ["№", "Д.нед.", "Дата"].iter().enumerate() {
        ws.merge_range(1, c as u16, 2, c as u16, h, &st.head)?;
    }
    ws.set_column_width(0, 4)?;
    ws.set_column_width(1, 6)?;
    ws.set_column_width(2, 11)?;
    write_day_columns(ws, view, &st, 3)?;
    for (n, e) in view.employees.iter().enumerate() {
        let c0 = 3 + n as u16 * 8;
        ws.merge_range(1, c0, 1, c0 + 7, &e.nick, &st.head)?;
        for (k, h) in ["Начало", "Уходы", "Конец", "Код"].iter().enumerate() {
            ws.merge_range(2, c0 + k as u16 * 2, 2, c0 + k as u16 * 2 + 1, h, &st.head)?;
        }
        for k in 0..8 {
            ws.set_column_width(c0 + k, if k % 2 == 0 { 7.0 } else { 4.0 })?;
        }
        ws.set_column_width(c0 + 7, 18)?;
        for (i, c) in e.cells.iter().enumerate() {
            let row = 3 + i as u32;
            write_time(ws, row, c0, c.m.start, &st.time)?;
            write_code(ws, row, c0 + 1, &c.m.start_code, &st.text)?;
            write_time(ws, row, c0 + 2, c.m.away, &st.time)?;
            write_code(ws, row, c0 + 3, &c.m.away_code, &st.text)?;
            write_time(ws, row, c0 + 4, c.m.end, &st.time)?;
            write_code(ws, row, c0 + 5, &c.m.end_code, &st.text)?;
            write_code(ws, row, c0 + 6, &c.m.day_code, &st.text)?;
            ws.write_string_with_format(row, c0 + 7, &c.note, &st.label)?;
        }
    }
    ws.set_freeze_panes(3, 3)?;

    // --- Табель
    let ws = book.add_worksheet();
    ws.set_name("Табель")?;
    ws.write_string_with_format(0, 0, format!("ТАБЕЛЬ · {}", view.title), &st.title)?;
    ws.write_string(
        0,
        4,
        format!("Норма: {} ч / {} дн.", view.norm_hours, view.norm_days),
    )?;
    let metrics = [
        "Процент испол.норм.раб.вр.",
        "Учетное время по Т-листу",
        "Расчетное время с компенс.",
        "Макс.время, с учет.перераб.",
        "Кол-во Опозданий (т-лист)",
        "Кол-во Опозданий (расч.)",
        "Время Опозданий (т-лист)",
        "Время Опозданий (расч.)",
        "Кол-во Отлучений (т-лист)",
        "Кол-во Отлучений (расч.)",
        "Время Отлучений (т-лист)",
        "Время Отлучений (расч.)",
        "Кол-во Ранних уходов (т-лист)",
        "Кол-во Ранних уходов (расч.)",
        "Время Ранних уходов (т-лист)",
        "Время Ранних уходов (расч.)",
    ];
    for (i, m) in metrics.iter().enumerate() {
        ws.merge_range(2 + i as u32, 0, 2 + i as u32, 2, m, &st.label)?;
    }
    let head_row = 2 + metrics.len() as u32 + 1;
    for (c, h) in ["№", "Д.нед.", "Дата"].iter().enumerate() {
        ws.merge_range(head_row, c as u16, head_row + 1, c as u16, h, &st.head)?;
    }
    ws.set_column_width(0, 4)?;
    ws.set_column_width(1, 6)?;
    ws.set_column_width(2, 11)?;
    write_day_columns(ws, view, &st, head_row + 2)?;
    let groups = [
        ("Отраб.врем.", 2u16),
        ("Т-лист", 7),
        ("Опоздания", 2),
        ("Отсутствия", 2),
        ("Ранний уход", 2),
    ];
    let subs = [
        "т-лист",
        "расч.",
        "Начало",
        "код",
        "Уходы",
        "код",
        "Конец",
        "код",
        "Код",
        "т-лист",
        "расч.",
        "т-лист",
        "расч.",
        "т-лист",
        "расч.",
    ];
    for (n, e) in view.employees.iter().enumerate() {
        let c0 = 3 + n as u16 * 15;
        ws.merge_range(1, c0, 1, c0 + 14, &e.nick, &st.head)?;
        let t = &e.tabel;
        let total = |ws: &mut Worksheet, i: u32, v: f64| {
            ws.write_number_with_format(2 + i, c0, days(v), &st.total)
                .map(|_| ())
        };
        let count = |ws: &mut Worksheet, i: u32, v: u32| {
            ws.write_number_with_format(2 + i, c0, v as f64, &st.int)
                .map(|_| ())
        };
        ws.write_number_with_format(2, c0, t.usage, &st.percent)?;
        total(ws, 1, t.worked)?;
        total(ws, 2, t.calc)?;
        total(ws, 3, t.max)?;
        count(ws, 4, t.late_n)?;
        count(ws, 5, t.late_n_calc)?;
        total(ws, 6, t.late_t)?;
        total(ws, 7, t.late_t_calc)?;
        count(ws, 8, t.away_n)?;
        count(ws, 9, t.away_n_calc)?;
        total(ws, 10, t.away_t)?;
        total(ws, 11, t.away_t_calc)?;
        count(ws, 12, t.early_n)?;
        count(ws, 13, t.early_n_calc)?;
        total(ws, 14, t.early_t)?;
        total(ws, 15, t.early_t_calc)?;

        let mut c = c0;
        for (title, width) in groups {
            ws.merge_range(head_row, c, head_row, c + width - 1, title, &st.head)?;
            c += width;
        }
        for (k, s) in subs.iter().enumerate() {
            ws.write_string_with_format(head_row + 1, c0 + k as u16, *s, &st.head)?;
            ws.set_column_width(
                c0 + k as u16,
                if matches!(k, 3 | 5 | 7 | 8) { 4.5 } else { 7.5 },
            )?;
        }
        for (i, cell) in e.cells.iter().enumerate() {
            let row = head_row + 2 + i as u32;
            let m = &cell.m;
            let x = &cell.calc;
            ws.write_number_with_format(row, c0, days(x.worked), &st.time)?;
            ws.write_number_with_format(row, c0 + 1, days(x.calc), &st.total)?;
            write_time(ws, row, c0 + 2, m.start, &st.time)?;
            write_code(ws, row, c0 + 3, &m.start_code, &st.text)?;
            write_time(ws, row, c0 + 4, m.away, &st.time)?;
            write_code(ws, row, c0 + 5, &m.away_code, &st.text)?;
            write_time(ws, row, c0 + 6, m.end, &st.time)?;
            write_code(ws, row, c0 + 7, &m.end_code, &st.text)?;
            write_code(ws, row, c0 + 8, &m.day_code, &st.text)?;
            for (k, v) in [
                x.late,
                x.late_calc,
                x.away,
                x.away_calc,
                x.early,
                x.early_calc,
            ]
            .into_iter()
            .enumerate()
            {
                ws.write_number_with_format(row, c0 + 9 + k as u16, days(v), &st.time)?;
            }
        }
    }
    ws.set_freeze_panes(head_row + 2, 3)?;

    // --- Сводка
    let ws = book.add_worksheet();
    ws.set_name("Сводка")?;
    ws.write_string_with_format(0, 0, format!("СВОДКА КТУ (%) · {}", view.title), &st.title)?;
    ws.write_string(1, 0, "Лимит раб.врем., в т.ч. при задержке ухода")?;
    ws.write_number_with_format(1, 3, view.limit, &st.percent)?;
    for (c, h) in ["№", "Ник", "Ф.И.О.", "КТУ"].iter().enumerate() {
        ws.write_string_with_format(3, c as u16, *h, &st.head)?;
    }
    ws.set_column_width(0, 5)?;
    ws.set_column_width(1, 44)?;
    ws.set_column_width(2, 26)?;
    for (n, e) in view.employees.iter().enumerate() {
        let row = 4 + n as u32;
        ws.write_number_with_format(row, 0, n as f64 + 1.0, &st.int)?;
        ws.write_string_with_format(row, 1, &e.nick, &st.label)?;
        ws.write_string_with_format(row, 2, &e.full_name, &st.label)?;
        ws.write_number_with_format(row, 3, e.svodka.ktu, &st.percent)?;
    }
    let top = 6 + view.employees.len() as u32;
    ws.write_string_with_format(top, 0, "№", &st.head)?;
    ws.write_string_with_format(top, 1, "Показатель", &st.head)?;
    let rows = [
        ("1", "Норматив рабочего времени (час)"),
        ("2", "Процент использования раб.времени"),
        ("2.1", "Время, отмеченное в Тайм-листе"),
        ("2.2", "Время, учитываемое в Табеле"),
        ("2.3", "Всего пропущенного времени по Тайм-листу"),
        ("2.4", "Всего не отработанного времени по Табелю"),
        ("2.5", "Всего переработанного времени"),
        ("3", "Кол-во опозданий и задержек нач.раб.дня"),
        ("3.1", "Всего в месяц время опозданий в табель"),
        ("4", "Кол-во отлучений из офиса в теч.раб.дня"),
        ("4.1", "Всего в месяц время отлучений в табель"),
        ("5", "Кол-во ранних уходов до конца раб.дня"),
        ("5.1", "Всего в месяц время сокращения раб.дня в табель"),
    ];
    for (i, (no, title)) in rows.iter().enumerate() {
        ws.write_string_with_format(top + 1 + i as u32, 0, *no, &st.text)?;
        ws.write_string_with_format(top + 1 + i as u32, 1, *title, &st.label)?;
    }
    for (n, e) in view.employees.iter().enumerate() {
        let col = 2 + n as u16;
        let s = &e.svodka;
        ws.write_string_with_format(top, col, &e.nick, &st.head)?;
        let values: [(f64, &Format); 13] = [
            (days(s.norm), &st.total),
            (s.usage, &st.percent),
            (days(s.tl_time), &st.total),
            (days(s.tab_time), &st.total),
            (days(s.missed_tl), &st.total),
            (days(s.missed_tab), &st.total),
            (days(s.overtime), &st.total),
            (s.late_n as f64, &st.int),
            (days(s.late_t), &st.total),
            (s.away_n as f64, &st.int),
            (days(s.away_t), &st.total),
            (s.early_n as f64, &st.int),
            (days(s.early_t), &st.total),
        ];
        for (i, (v, f)) in values.into_iter().enumerate() {
            ws.write_number_with_format(top + 1 + i as u32, col, v, f)?;
        }
        if n > 0 {
            ws.set_column_width(col, 10)?;
        }
    }
    Ok(book.save_to_buffer()?)
}
