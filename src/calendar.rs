//! Calendar export compatible with the original iCalendar endpoint.
use crate::{
    auth,
    profile::{ProfileError, Profiles},
};
use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension;

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace("\r\n", "\\n")
        .replace('\n', "\\n")
}
fn fold(s: &str) -> String {
    let mut out = String::new();
    let mut bytes = 0usize;
    let mut limit = 75;
    for ch in s.chars() {
        let width = ch.len_utf8();
        if bytes > 0 && bytes + width > limit {
            out.push_str("\r\n ");
            bytes = 0;
            limit = 74;
        }
        out.push(ch);
        bytes += width;
    }
    out
}
fn format_time(t: i64) -> Result<String, ProfileError> {
    Ok(DateTime::<Utc>::from_timestamp(t, 0)
        .ok_or(ProfileError::Storage)?
        .format("%Y%m%dT%H%M%SZ")
        .to_string())
}
/// Authenticate exactly one credential, with scoped tokens taking precedence.
pub fn generate(
    p: &Profiles,
    id: i64,
    token: &str,
    header: &str,
    query_init: &str,
    allow_dev: bool,
    now: i64,
) -> Result<String, ProfileError> {
    if !token.is_empty() {
        if auth::verify_scoped_token(&p.bot_token, token, "ics", id, now).is_none() {
            return Err(ProfileError::Unauthorized("Требуется авторизация."));
        }
    } else {
        let init = if !header.is_empty() {
            header
        } else if allow_dev {
            query_init
        } else {
            ""
        };
        let (user_id, user) = p
            .user_id(init, now)
            .map_err(|_| ProfileError::Unauthorized("Требуется авторизация."))?;
        let db = p.db.lock().map_err(|_| ProfileError::Storage)?;
        p.register(&db, user_id, &user, now)?;
    }
    let db = p.db.lock().map_err(|_| ProfileError::Storage)?;
    let row:Option<(i64,i64,i64,String,Option<i64>,i64)>=db.query_row("SELECT t.start_at,t.duration_min,t.capacity,t.training_type,t.deleted_at,(SELECT count(*) FROM bookings b WHERE b.training_id=t.id AND b.status='booked') FROM trainings t WHERE t.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional().map_err(|_|ProfileError::Storage)?;
    let (start, duration, capacity, typ, deleted, count) =
        row.ok_or(ProfileError::NotFound("Тренировка не найдена."))?;
    if deleted.unwrap_or(0) != 0 {
        return Err(ProfileError::NotFound("Тренировка не найдена."));
    }
    let summary = escape(if typ.is_empty() {
        "Сквош-тренировка"
    } else {
        &typ
    });
    let description = escape(&format!(
        "Сквош-тренировка · {duration} мин · записано {count} из {capacity}"
    ));
    let end = start
        .checked_add(duration.checked_mul(60).ok_or(ProfileError::Storage)?)
        .ok_or(ProfileError::Storage)?;
    let lines = vec![
        "BEGIN:VCALENDAR".to_owned(),
        "VERSION:2.0".into(),
        "PRODID:-//SquashBot//RU".into(),
        "CALSCALE:GREGORIAN".into(),
        "METHOD:PUBLISH".into(),
        "BEGIN:VEVENT".into(),
        format!("UID:squash-training-{id}@squashbot"),
        format!("DTSTAMP:{}", format_time(now)?),
        format!("DTSTART:{}", format_time(start)?),
        format!("DTEND:{}", format_time(end)?),
        format!("SUMMARY:{summary}"),
        format!("DESCRIPTION:{description}"),
        "BEGIN:VALARM".into(),
        "TRIGGER:-PT30M".into(),
        "ACTION:DISPLAY".into(),
        format!("DESCRIPTION:{summary}"),
        "END:VALARM".into(),
        "END:VEVENT".into(),
        "END:VCALENDAR".into(),
    ];
    Ok(lines
        .iter()
        .map(|line| format!("{}\r\n", fold(line)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folding_preserves_unicode_and_octet_limit() {
        let line = format!("SUMMARY:{}", "абв,;\\".repeat(30));
        let folded = fold(&line);
        assert_eq!(folded.replace("\r\n ", ""), line);
        assert!(folded.split("\r\n").all(|line| line.len() <= 75));
        assert_eq!(escape("а\\б;в,г\r\nд\nе"), "а\\\\б\\;в\\,г\\nд\\nе");
    }
}
