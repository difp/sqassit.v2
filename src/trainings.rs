//! Player-facing training schedule, including privacy-preserving identities.
use crate::{
    auth, identity,
    profile::{ProfileError, Profiles},
};
use chrono::{DateTime, Days, LocalResult, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

fn end_of_window(now: i64, days: i64, timezone: Tz) -> Result<i64, ProfileError> {
    let start = DateTime::<Utc>::from_timestamp(now, 0)
        .ok_or(ProfileError::Storage)?
        .with_timezone(&timezone);
    let target = start
        .naive_local()
        .checked_add_days(Days::new(days.clamp(1, 31) as u64))
        .ok_or(ProfileError::Storage)?;
    let end = match timezone.from_local_datetime(&target) {
        LocalResult::Single(t) | LocalResult::Ambiguous(t, _) => t.timestamp(),
        // Python's zoneinfo uses the offset preceding a nonexistent local time.
        LocalResult::None => {
            let before = timezone
                .from_local_datetime(&(target - chrono::Duration::hours(3)))
                .earliest()
                .ok_or(ProfileError::Storage)?;
            target.and_utc().timestamp() - i64::from(before.offset().fix().local_minus_utc())
        }
    };
    Ok(end)
}

pub fn list(
    profiles: &Profiles,
    init: &str,
    now: i64,
    days: i64,
    timezone: Tz,
) -> Result<Value, ProfileError> {
    let (viewer, user) = profiles.user_id(init, now)?;
    let db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    profiles.register(&db, viewer, &user, now)?;
    let end = end_of_window(now, days, timezone)?;
    let mut stmt = db.prepare("SELECT t.id,t.title,t.training_type,t.start_at,t.duration_min,t.capacity,t.level,t.reminder_enabled,t.reminder_offset_min,t.created_by, (SELECT COUNT(*) FROM bookings b WHERE b.training_id=t.id AND b.status='booked') FROM trainings t WHERE t.start_at>=?1 AND t.start_at<=?2 AND t.deleted_at IS NULL ORDER BY t.start_at ASC").map_err(|_| ProfileError::Storage)?;
    let rows = stmt
        .query_map(params![now, end], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, bool>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, i64>(10)?,
            ))
        })
        .map_err(|_| ProfileError::Storage)?;
    let mut trainings = Vec::new();
    for row in rows {
        let (
            id,
            title,
            training_type,
            start,
            duration,
            capacity,
            level,
            reminder,
            offset,
            creator,
            count,
        ) = row.map_err(|_| ProfileError::Storage)?;
        let local = DateTime::<Utc>::from_timestamp(start, 0)
            .ok_or(ProfileError::Storage)?
            .with_timezone(&timezone);
        let my_status: Option<String> = db
            .query_row(
                "SELECT status FROM bookings WHERE training_id=?1 AND user_id=?2",
                params![id, viewer],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| ProfileError::Storage)?;
        let coach = match creator {
            Some(coach_id) => db.query_row("SELECT first_name,avatar_file_id,bio FROM users WHERE telegram_id=?1",[coach_id],|r| {
                let name: Option<String>=r.get(0)?; let file: Option<String>=r.get(1)?; let bio: String=r.get(2)?;
                Ok(json!({"telegram_id":coach_id,"first_name":name.filter(|s|!s.is_empty()).unwrap_or_else(||"Тренер".to_owned()),"avatar_url":profiles.avatar_url(coach_id,file.as_deref(),true,now),"bio":bio}))
            }).optional().map_err(|_|ProfileError::Storage)?,
            None => None,
        };
        let mut participants = Vec::new();
        let mut waitlist_count = 0;
        let mut attendees=db.prepare("SELECT b.status,u.telegram_id,u.first_name,u.avatar_file_id,u.display_mode FROM bookings b JOIN users u ON u.telegram_id=b.user_id WHERE b.training_id=?1 AND b.status IN ('pending','booked','waitlist') ORDER BY CASE b.status WHEN 'booked' THEN 0 ELSE 1 END,b.position ASC").map_err(|_|ProfileError::Storage)?;
        let people = attendees
            .query_map([id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(|_| ProfileError::Storage)?;
        for person in people {
            let (status, person_id, name, file, mode) =
                person.map_err(|_| ProfileError::Storage)?;
            if status == "waitlist" {
                waitlist_count += 1;
            }
            if status != "booked" {
                continue;
            }
            let anonymous = mode == "animal";
            let (emoji, animal_name) = identity::animal_identity(person_id);
            let (display, avatar, avatar_emoji) = if anonymous {
                (animal_name.to_owned(), String::new(), emoji)
            } else {
                (
                    name.filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "Игрок".to_owned()),
                    profiles.avatar_url(person_id, file.as_deref(), false, now),
                    "",
                )
            };
            let token =
                auth::issue_scoped_token(&profiles.bot_token, "profile", 0, person_id, now, 86_400);
            participants.push(json!({"first_name":display,"avatar_url":avatar,"avatar_emoji":avatar_emoji,"anonymous":anonymous,"profile_token":token,"status":status,"is_me":person_id==viewer}));
        }
        let seconds = start - now;
        trainings.push(json!({"id":id,"title":title,"type":training_type,"start_at":start,"date":local.format("%Y-%m-%d").to_string(),"time":local.format("%H:%M").to_string(),"datetime_local":local.format("%d.%m.%Y %H:%M").to_string(),"duration_min":duration,"capacity":capacity,"level":level,"booked_count":count,"waitlist_count":waitlist_count,"reminder_enabled":reminder,"reminder_offset_min":offset,"deleted":false,"can_self_cancel":seconds>=270*60,"is_booked":my_status.as_deref()==Some("booked"),"is_waitlisted":my_status.as_deref()==Some("waitlist"),"is_pending":my_status.as_deref()==Some("pending"),"is_past":seconds<=0,"seconds_left":seconds,"cancel_lock_minutes":270,"ics_token":auth::issue_scoped_token(&profiles.bot_token,"ics",id,viewer,now,86_400),"coach":coach,"participants":participants}));
    }
    Ok(json!({"ok":true,"trainings":trainings}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn date_window_matches_python_zoneinfo_across_dst() {
        // Reference values from Python datetime + timedelta(days=1).
        let tz = chrono_tz::Europe::Berlin;
        assert_eq!(end_of_window(1_774_661_400, 1, tz).unwrap(), 1_774_747_800);
        assert_eq!(end_of_window(1_792_801_800, 1, tz).unwrap(), 1_792_888_200);
        assert_eq!(end_of_window(1_774_695_600, 1, tz).unwrap(), 1_774_778_400);
    }
}
