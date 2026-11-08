use core_db::{rusqlite::Connection, Db};
use std::{error::Error, fs, path::Path};

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;
pub const PV5: &str = r#"{"exposure":0.5,"crop":{"cx":0.45,"cy":0.5,"hw":0.4,"hh":0.3,"angle":2.0},"masks":[{"name":"Sky","opacity":0.7,"enabled":true,"components":[{"kind":{"type":"linear","p0":[0.0,0.0],"p1":[0.0,1.0]},"op":"add"}],"adjust":{"exposure":0.8}}],"denoise":{"enabled":true,"amount":35}}"#;
pub const PV6: &str = r#"{"exposure":-0.25,"crop":{"rot90":1},"masks":[{"name":"Subject","opacity":0.8,"components":[{"kind":{"type":"linear","p0":[0.0,0.0],"p1":[0.0,1.0]},"op":"add"}],"adjust":{"exposure":0.8}}],"denoise":{"enabled":true,"amount":60}}"#;
pub const CORRUPT_EDIT: &str = "{invalid-json";

/// Create only synthetic data, in a new directory chosen by the caller.
pub fn generate(root: &Path) -> Result<()> {
    fs::create_dir(root)?;
    fs::create_dir(root.join("photos"))?;
    for name in ["raw.cr3", "raw.jpg", "hdr.exr", "pano.exr", "corrupt.cr3"] {
        fs::write(
            root.join("photos").join(name),
            b"synthetic placeholder, not an image\n",
        )?;
    }
    image::RgbImage::from_pixel(2, 2, image::Rgb([32, 64, 96]))
        .save(root.join("photos/raw.jpg"))?;
    let root = root.canonicalize()?;
    let db = Db::open(&root.join("source.db"))?;
    populate(&db.conn)?;
    set_paths(&db.conn, &root)?;
    write_sidecar(&root)?;
    copy_catalog(&db.conn, &root.join("copied.db"))?;
    fs::write(
        root.join("corrupt.db"),
        b"synthetic corrupt sqlite evidence\n",
    )?;
    let newer = Db::open(&root.join("newer.db"))?;
    // One version ahead of this build, so `Db::open` must refuse it as "from a newer build".
    newer.conn.execute_batch(&format!(
        "PRAGMA user_version = {}",
        core_db::LATEST_SCHEMA_VERSION + 1
    ))?;
    Ok(())
}

fn populate(conn: &Connection) -> Result<()> {
    conn.execute_batch("INSERT INTO folders(id,path,added_at) VALUES(1,'photos',100);
        INSERT INTO import_sessions(id,source_volume,mode,started_at,finished_at,file_count,skipped_count)
        VALUES(1,'synthetic','reference',100,101,6,1);
        INSERT INTO images(id,content_hash,file_size,path,folder_id,original_filename,status,camera_make,camera_model,iso,width,height,imported_at,import_session_id,format) VALUES
        (1,zeroblob(32),32,'photos/raw.cr3',1,'raw.cr3','present','Synthetic','Camera',400,6000,4000,100,1,'raw'),
        (2,zeroblob(32),32,'photos/raw.jpg',1,'raw.jpg','present','Synthetic','Camera',400,6000,4000,100,1,'jpeg'),
        (3,zeroblob(32),32,'photos/hdr.exr',1,'hdr.exr','present',NULL,NULL,NULL,6000,4000,100,1,'hdr'),
        (4,zeroblob(32),32,'photos/pano.exr',1,'pano.exr','present',NULL,NULL,NULL,9000,4000,100,1,'hdr'),
        (5,zeroblob(32),32,'photos/missing.cr3',1,'missing.cr3','missing',NULL,NULL,NULL,NULL,NULL,100,1,'raw'),
        (6,zeroblob(32),32,'photos/corrupt.cr3',1,'corrupt.cr3','present',NULL,NULL,NULL,NULL,NULL,100,1,'raw');
        INSERT INTO image_pairs VALUES(2,1,100);
        INSERT INTO hdr_sources VALUES(3,1,0,-1.0),(3,5,1,1.0);
        INSERT INTO panorama_sources VALUES(4,1,0),(4,5,1);
        INSERT INTO keywords VALUES(1,'Synthetic',NULL);
        INSERT INTO image_keywords VALUES(1,1);
        INSERT INTO collections VALUES(1,'Fixtures',0,NULL);
        INSERT INTO collection_images VALUES(1,1);
        INSERT INTO person(id,name,created_at,updated_at) VALUES(1,'Example Person',100,100);
        INSERT INTO face(id,asset_id,person_id,bbox_x1,bbox_y1,bbox_x2,bbox_y2,kps,quality_score,det_score,source,status,model_version,created_at)
        VALUES(1,1,1,0.1,0.2,0.3,0.4,zeroblob(40),0.8,0.9,'manual','confirmed','synthetic',100);")?;
    core_library::set_edit(conn, 1, 5, PV5, 100)?;
    core_library::set_edit(conn, 2, 6, PV6, 101)?;
    core_library::set_edit(conn, 6, 6, CORRUPT_EDIT, 102)?;
    core_library::create_snapshot(conn, 1, "Before", PV5, 5, 100)?;
    core_library::insert_preset(
        conn,
        "Synthetic",
        "My Presets",
        true,
        "[\"exposure\"]",
        "{\"exposure\":0.5}",
        5,
        100,
    )?;
    Ok(())
}

fn set_paths(conn: &Connection, root: &Path) -> Result<()> {
    let folder = root.join("photos");
    conn.execute(
        "UPDATE folders SET path = ?1 WHERE id = 1",
        [folder.to_str().ok_or("non-UTF-8 path")?],
    )?;
    for (id, name) in [
        (1, "raw.cr3"),
        (2, "raw.jpg"),
        (3, "hdr.exr"),
        (4, "pano.exr"),
        (5, "missing.cr3"),
        (6, "corrupt.cr3"),
    ] {
        let path = folder.join(name);
        conn.execute(
            "UPDATE images SET path = ?1 WHERE id = ?2",
            core_db::rusqlite::params![path.to_str().ok_or("non-UTF-8 path")?, id],
        )?;
    }
    Ok(())
}

fn write_sidecar(root: &Path) -> Result<()> {
    use core_library::sidecar::{Sidecar, SidecarRating, SidecarWrite};
    let sc = Sidecar {
        schema_version: 1,
        content_hash: "00".repeat(32),
        develop: Some(serde_json::from_str(PV5)?),
        process_version: Some(5),
        rating: SidecarRating {
            stars: 0,
            flag: "none".into(),
            color_label: None,
        },
        keywords: vec!["Synthetic".into()],
        updated_at: 100,
    };
    core_library::sidecar::write_snapshot(&SidecarWrite {
        path: root.join("photos/raw.cr3.json"),
        sidecar: sc,
    })?;
    Ok(())
}

fn copy_catalog(conn: &Connection, dest: &Path) -> Result<()> {
    let path = dest.to_str().ok_or("non-UTF-8 destination")?;
    conn.execute("VACUUM INTO ?1", [path])?;
    Ok(())
}
