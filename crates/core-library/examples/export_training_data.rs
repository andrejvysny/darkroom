//! Validate the behavioral-capture schema by exporting it to per-feature JSONL — proves the logged
//! `user_events` + `image_features` produce clean training data before any model is built.
//!
//! Usage: cargo run -p core-library --example export_training_data
//!        DB=/path/to/catalog.db cargo run -p core-library --example export_training_data
//!
//! Emits one JSON object per line, each tagged with `kind`:
//! - `edit` — {image_id, params} (auto-edit style label vector)
//! - `lighting` — {image_id, target:{temp,tint,exposure,...}, wb_as_shot_rg/bg} (lighting regression)
//! - `dedup_pair` — {keeper, loser} (dedup keeper ranking)
//! - `cull_pair` — {winner, loser, group} (best-shot within-group preference)
//!
//! Prints an event-type summary to stderr.

use std::collections::HashMap;
use std::path::PathBuf;

use core_db::rusqlite::OptionalExtension;
use core_db::Db;
use serde_json::{json, Value};

type Err = Box<dyn std::error::Error>;

fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("DB") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/com.andrejvysny.darkroom/catalog.db")
}

fn emit(v: Value) {
    println!("{v}");
}

fn parse_ids(s: Option<String>) -> Vec<i64> {
    s.and_then(|s| serde_json::from_str::<Vec<i64>>(&s).ok())
        .unwrap_or_default()
}

fn main() -> Result<(), Err> {
    let path = db_path();
    eprintln!("db: {}", path.display());
    let db = Db::open(&path)?;
    let conn = &db.conn;

    // --- event-type summary (stderr) ---
    {
        let mut stmt = conn.prepare(
            "SELECT event_type, COUNT(*) FROM user_events GROUP BY event_type ORDER BY 2 DESC",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        eprintln!("── event summary ──");
        for row in rows {
            let (t, n) = row?;
            eprintln!("  {t:<28} {n}");
        }
    }

    // --- edit label vectors (auto-edit) ---
    let mut n_edit = 0;
    {
        let mut stmt = conn.prepare(
            "SELECT image_id, params_after FROM user_events
             WHERE event_type='develop.params_commit' AND params_after IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (image_id, params) = row?;
            let params: Value = serde_json::from_str(&params).unwrap_or(Value::Null);
            emit(json!({"kind": "edit", "image_id": image_id, "params": params}));
            n_edit += 1;

            // lighting subvector + as-shot WB input (joined per image)
            if let Some(id) = image_id {
                let wb: Option<(Option<f64>, Option<f64>)> = conn
                    .query_row(
                        "SELECT wb_as_shot_rg, wb_as_shot_bg FROM image_features WHERE image_id=?1",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let target = json!({
                    "temp": params.get("temp"), "tint": params.get("tint"),
                    "exposure": params.get("exposure"), "highlights": params.get("highlights"),
                    "shadows": params.get("shadows"), "blacks": params.get("blacks"),
                    "whites": params.get("whites"),
                });
                emit(json!({
                    "kind": "lighting", "image_id": id, "target": target,
                    "wb_as_shot_rg": wb.and_then(|w| w.0), "wb_as_shot_bg": wb.and_then(|w| w.1),
                }));
            }
        }
    }

    // --- dedup keeper ranking pairs ---
    let mut n_dedup = 0;
    {
        let mut stmt = conn.prepare(
            "SELECT chosen_id, rejected_ids FROM user_events WHERE event_type='dedup.keeper_chosen'",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (keeper, rejected) = row?;
            if let Some(k) = keeper {
                for loser in parse_ids(rejected) {
                    emit(json!({"kind": "dedup_pair", "keeper": k, "loser": loser}));
                    n_dedup += 1;
                }
            }
        }
    }

    // --- best-shot within-group preference pairs (pick × reject per group) ---
    let mut n_cull = 0;
    {
        let mut picks: HashMap<String, Vec<i64>> = HashMap::new();
        let mut rejects: HashMap<String, Vec<i64>> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT group_id, event_type, image_id FROM user_events
             WHERE event_type IN ('culling.flag_pick','culling.flag_reject') AND group_id IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?,
            ))
        })?;
        for row in rows {
            let (g, et, id) = row?;
            if let Some(id) = id {
                if et.ends_with("pick") {
                    picks.entry(g).or_default().push(id);
                } else {
                    rejects.entry(g).or_default().push(id);
                }
            }
        }
        for (g, ps) in &picks {
            if let Some(rs) = rejects.get(g) {
                for &w in ps {
                    for &l in rs {
                        emit(json!({"kind": "cull_pair", "winner": w, "loser": l, "group": g}));
                        n_cull += 1;
                    }
                }
            }
        }
    }

    let n_feat: i64 = conn.query_row("SELECT COUNT(*) FROM image_features", [], |r| r.get(0))?;
    eprintln!(
        "── exported: {n_edit} edit, {n_dedup} dedup_pair, {n_cull} cull_pair · {n_feat} feature rows ──"
    );
    Ok(())
}
