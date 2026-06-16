//! Culling: star ratings, pick/reject flags, color labels (the `ratings_flags` table).

use crate::error::LibError;
use core_db::rusqlite::{params, Connection};

pub fn set_rating(conn: &Connection, image_id: i64, stars: i64) -> Result<(), LibError> {
    conn.execute(
        "INSERT INTO ratings_flags(image_id, stars) VALUES(?1, ?2)
         ON CONFLICT(image_id) DO UPDATE SET stars = excluded.stars",
        params![image_id, stars.clamp(0, 5)],
    )?;
    Ok(())
}

pub fn set_flag(conn: &Connection, image_id: i64, flag: &str) -> Result<(), LibError> {
    let f = match flag {
        "pick" | "reject" | "none" => flag,
        _ => "none",
    };
    conn.execute(
        "INSERT INTO ratings_flags(image_id, flag) VALUES(?1, ?2)
         ON CONFLICT(image_id) DO UPDATE SET flag = excluded.flag",
        params![image_id, f],
    )?;
    Ok(())
}

pub fn set_label(conn: &Connection, image_id: i64, label: Option<&str>) -> Result<(), LibError> {
    conn.execute(
        "INSERT INTO ratings_flags(image_id, color_label) VALUES(?1, ?2)
         ON CONFLICT(image_id) DO UPDATE SET color_label = excluded.color_label",
        params![image_id, label],
    )?;
    Ok(())
}
