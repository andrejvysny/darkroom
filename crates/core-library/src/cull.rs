//! Culling: star ratings, pick/reject flags, color labels (the `ratings_flags` table).
//! Single-image setters plus batch (`*_many`) variants that apply to a selection in one
//! transaction.

use crate::error::LibError;
use core_db::rusqlite::{params, Connection};

fn norm_flag(flag: &str) -> &str {
    match flag {
        "pick" | "reject" | "none" => flag,
        _ => "none",
    }
}

pub fn set_rating(conn: &Connection, image_id: i64, stars: i64) -> Result<(), LibError> {
    conn.execute(
        "INSERT INTO ratings_flags(image_id, stars) VALUES(?1, ?2)
         ON CONFLICT(image_id) DO UPDATE SET stars = excluded.stars",
        params![image_id, stars.clamp(0, 5)],
    )?;
    Ok(())
}

pub fn set_flag(conn: &Connection, image_id: i64, flag: &str) -> Result<(), LibError> {
    conn.execute(
        "INSERT INTO ratings_flags(image_id, flag) VALUES(?1, ?2)
         ON CONFLICT(image_id) DO UPDATE SET flag = excluded.flag",
        params![image_id, norm_flag(flag)],
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

/// Set the same rating on many images in one transaction.
pub fn set_rating_many(
    conn: &mut Connection,
    image_ids: &[i64],
    stars: i64,
) -> Result<(), LibError> {
    let s = stars.clamp(0, 5);
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO ratings_flags(image_id, stars) VALUES(?1, ?2)
             ON CONFLICT(image_id) DO UPDATE SET stars = excluded.stars",
        )?;
        for &id in image_ids {
            stmt.execute(params![id, s])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Set the same flag on many images in one transaction.
pub fn set_flag_many(conn: &mut Connection, image_ids: &[i64], flag: &str) -> Result<(), LibError> {
    let f = norm_flag(flag);
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO ratings_flags(image_id, flag) VALUES(?1, ?2)
             ON CONFLICT(image_id) DO UPDATE SET flag = excluded.flag",
        )?;
        for &id in image_ids {
            stmt.execute(params![id, f])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Set the same color label (or clear it) on many images in one transaction.
pub fn set_label_many(
    conn: &mut Connection,
    image_ids: &[i64],
    label: Option<&str>,
) -> Result<(), LibError> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO ratings_flags(image_id, color_label) VALUES(?1, ?2)
             ON CONFLICT(image_id) DO UPDATE SET color_label = excluded.color_label",
        )?;
        for &id in image_ids {
            stmt.execute(params![id, label])?;
        }
    }
    tx.commit()?;
    Ok(())
}
