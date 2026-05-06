mod domain;
mod infra;
mod ui;

use std::path::PathBuf;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let db_path = data_dir().join("stoclove.sqlite");
    let db_path_str = db_path.to_string_lossy().to_string();

    log::info!("Database path: {}", db_path_str);

    // Optional: startup sync via Google Drive if token is set
    if let Ok(token) = std::env::var("STOCLOVE_DRIVE_TOKEN") {
        log::info!("Drive token found, attempting startup sync...");
        if let Ok(db) = crate::infra::db::Database::open(&db_path_str) {
            if let Err(e) = crate::infra::sync::startup_sync(&db, &db_path_str, &token) {
                log::warn!("Startup sync failed: {}", e);
            }
        }
    }

    if let Err(e) = ui::run_app(&db_path_str, data_dir()) {
        eprintln!("Application error: {}", e);
        std::process::exit(1);
    }
}

fn data_dir() -> PathBuf {
    let dir = dirs_path();
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn dirs_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".local").join("share").join("stoclove");
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        return PathBuf::from(appdata).join("stoclove");
    }
    PathBuf::from(".")
}
