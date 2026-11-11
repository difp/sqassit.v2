//! Privacy-aware public match cards.
use crate::{
    auth, identity,
    profile::{ProfileError, Profiles},
};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

fn person(
    profiles: &Profiles,
    db: &Connection,
    id: i64,
    now: i64,
) -> Result<Option<Value>, ProfileError> {
    let user: Option<(Option<String>, Option<String>, String)> = db
        .query_row(
            "SELECT first_name,avatar_file_id,display_mode FROM users WHERE telegram_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|_| ProfileError::Storage)?;
    Ok(user.map(|(name,file,mode)| {
        let anonymous=mode=="animal";
        let (emoji,animal)=identity::animal_identity(id);
        let name=if anonymous { animal.to_owned() } else { name.filter(|s|!s.is_empty()).unwrap_or_else(||"Игрок".into()) };
        let avatar=if anonymous { String::new() } else { profiles.avatar_url(id,file.as_deref(),false,now) };
        json!({"first_name":name,"avatar_url":avatar,"avatar_emoji":if anonymous {emoji} else {""},"anonymous":anonymous,"profile_token":auth::issue_scoped_token(&profiles.bot_token,"profile",0,id,now,86400)})
    }))
}

pub fn list(profiles: &Profiles, init: &str, now: i64, days: i64) -> Result<Value, ProfileError> {
    let (viewer, user) = profiles.user_id(init, now)?;
    let db = profiles.db.lock().map_err(|_| ProfileError::Storage)?;
    profiles.register(&db, viewer, &user, now)?;
    let until = crate::trainings::end_of_window(now, days.clamp(1, 90), profiles.timezone)?;
    let mut stmt=db.prepare("SELECT id,title,description,location,start_at,duration_min,max_players,level,created_by FROM matches WHERE start_at>=?1 AND start_at<=?2 AND deleted_at IS NULL ORDER BY start_at ASC").map_err(|_|ProfileError::Storage)?;
    let rows = stmt
        .query_map(params![now, until], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, i64>(8)?,
            ))
        })
        .map_err(|_| ProfileError::Storage)?;
    let mut matches = Vec::new();
    for row in rows {
        let (id, title, description, location, start, duration, capacity, level, creator) =
            row.map_err(|_| ProfileError::Storage)?;
        let players_count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM match_players WHERE match_id=?1 AND status='booked'",
                [id],
                |r| r.get(0),
            )
            .map_err(|_| ProfileError::Storage)?;
        let coauthor: bool=db.query_row("SELECT EXISTS(SELECT 1 FROM event_coauthors WHERE event_type='match' AND event_id=?1 AND user_id=?2)",params![id,viewer],|r|r.get(0)).map_err(|_|ProfileError::Storage)?;
        let manage = creator == viewer || coauthor;
        let mut participants = Vec::new();
        let mut pending_requests = Vec::new();
        let mut pending_count = 0;
        let mut current = String::new();
        let mut players=db.prepare("SELECT mp.user_id,mp.status,mp.created_at FROM match_players mp JOIN users u ON u.telegram_id=mp.user_id WHERE mp.match_id=?1 ORDER BY mp.created_at ASC").map_err(|_|ProfileError::Storage)?;
        let people = players
            .query_map([id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(|_| ProfileError::Storage)?;
        for player in people {
            let (person_id, status, joined) = player.map_err(|_| ProfileError::Storage)?;
            if person_id == viewer {
                current = status.clone();
            }
            if status == "pending" {
                pending_count += 1;
            }
            if status == "booked" || (status == "pending" && manage) {
                if let Some(mut item) = person(profiles, &db, person_id, now)? {
                    item["joined_at"] = json!(joined);
                    if status == "booked" {
                        item["is_organizer"] = json!(person_id == creator);
                        participants.push(item);
                    } else {
                        item["telegram_id"] = json!(person_id);
                        pending_requests.push(item);
                    }
                }
            }
        }
        let local = DateTime::<Utc>::from_timestamp(start, 0)
            .ok_or(ProfileError::Storage)?
            .with_timezone(&profiles.timezone);
        matches.push(json!({"id":id,"title":title,"description":description,"location":location,"start_at":start,"date":local.format("%Y-%m-%d").to_string(),"time":local.format("%H:%M").to_string(),"datetime_local":local.format("%d.%m.%Y %H:%M").to_string(),"duration_min":duration,"max_players":capacity,"players_count":players_count,"level":level,"is_joined":current=="booked","is_pending":current=="pending","is_waitlisted":current=="waitlist","is_creator":creator==viewer,"is_coauthor":coauthor,"can_manage":manage,"pending_count":pending_count,"pending_requests":pending_requests,"creator":person(profiles,&db,creator,now)?,"participants":participants}));
    }
    Ok(json!({"ok":true,"matches":matches}))
}
