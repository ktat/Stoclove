/// Google Drive sync client.
/// Uploads/downloads the SQLite database file to a dedicated folder.
/// Uses a simple REST API approach (no OAuth UI flow is implemented in this mock;
/// the token must be provided externally, e.g., from device auth).

use anyhow::{Context, Result};

const DRIVE_UPLOAD_URL: &str = "https://www.googleapis.com/upload/drive/v3/files";
const DRIVE_FILES_URL: &str = "https://www.googleapis.com/drive/v3/files";
const APP_FOLDER_NAME: &str = "stoclove-sync";
const DB_FILENAME: &str = "stoclove.sqlite";

pub struct DriveSync {
    client: reqwest::blocking::Client,
    access_token: String,
}

impl DriveSync {
    pub fn new(access_token: String) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Self { client, access_token }
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.access_token)
    }

    /// Find or create the sync folder in Google Drive.
    fn get_or_create_folder(&self) -> Result<String> {
        let query = format!(
            "name='{}' and mimeType='application/vnd.google-apps.folder' and trashed=false",
            APP_FOLDER_NAME
        );
        let resp: serde_json::Value = self
            .client
            .get(DRIVE_FILES_URL)
            .header("Authorization", self.auth_header())
            .query(&[("q", &query), ("fields", &"files(id,name)".to_string())])
            .send()
            .context("Drive folder list request failed")?
            .json()
            .context("Failed to parse Drive folder list")?;

        if let Some(files) = resp["files"].as_array() {
            if let Some(folder) = files.first() {
                if let Some(id) = folder["id"].as_str() {
                    return Ok(id.to_string());
                }
            }
        }

        // Create folder
        let body = serde_json::json!({
            "name": APP_FOLDER_NAME,
            "mimeType": "application/vnd.google-apps.folder"
        });
        let resp: serde_json::Value = self
            .client
            .post(DRIVE_FILES_URL)
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .context("Drive folder create failed")?
            .json()?;

        resp["id"]
            .as_str()
            .map(|s| s.to_string())
            .context("No id in folder create response")
    }

    /// Upload the local database file to Google Drive.
    pub fn upload(&self, local_path: &str) -> Result<()> {
        let folder_id = self.get_or_create_folder()?;
        let data = std::fs::read(local_path).context("Failed to read local DB file")?;

        // Check if the file already exists in the folder
        let query = format!(
            "name='{}' and '{}' in parents and trashed=false",
            DB_FILENAME, folder_id
        );
        let resp: serde_json::Value = self
            .client
            .get(DRIVE_FILES_URL)
            .header("Authorization", self.auth_header())
            .query(&[("q", &query), ("fields", &"files(id)".to_string())])
            .send()?
            .json()?;

        let existing_id = resp["files"]
            .as_array()
            .and_then(|f| f.first())
            .and_then(|f| f["id"].as_str())
            .map(|s| s.to_string());

        let metadata = serde_json::json!({
            "name": DB_FILENAME,
            "parents": [folder_id]
        });
        let metadata_str = metadata.to_string();

        if let Some(file_id) = existing_id {
            // Update existing file
            let url = format!("{}/{}?uploadType=multipart", DRIVE_UPLOAD_URL, file_id);
            self.client
                .patch(&url)
                .header("Authorization", self.auth_header())
                .header("Content-Type", "multipart/related; boundary=boundary_stoclove")
                .body(build_multipart(&metadata_str, &data))
                .send()
                .context("Drive upload (update) failed")?;
        } else {
            // Create new file
            let url = format!("{}?uploadType=multipart", DRIVE_UPLOAD_URL);
            self.client
                .post(&url)
                .header("Authorization", self.auth_header())
                .header("Content-Type", "multipart/related; boundary=boundary_stoclove")
                .body(build_multipart(&metadata_str, &data))
                .send()
                .context("Drive upload (create) failed")?;
        }

        log::info!("Uploaded {} to Google Drive", local_path);
        Ok(())
    }

    /// Download the remote database to a local path.
    pub fn download(&self, dest_path: &str) -> Result<()> {
        let folder_id = self.get_or_create_folder()?;
        let query = format!(
            "name='{}' and '{}' in parents and trashed=false",
            DB_FILENAME, folder_id
        );
        let resp: serde_json::Value = self
            .client
            .get(DRIVE_FILES_URL)
            .header("Authorization", self.auth_header())
            .query(&[("q", &query), ("fields", &"files(id)".to_string())])
            .send()?
            .json()?;

        let file_id = resp["files"]
            .as_array()
            .and_then(|f| f.first())
            .and_then(|f| f["id"].as_str())
            .context("No remote DB file found in Drive")?
            .to_string();

        let url = format!("{}/{}?alt=media", DRIVE_FILES_URL, file_id);
        let bytes = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .context("Drive download request failed")?
            .bytes()
            .context("Failed to read downloaded bytes")?;

        std::fs::write(dest_path, &bytes).context("Failed to write downloaded DB")?;
        log::info!("Downloaded remote DB to {}", dest_path);
        Ok(())
    }
}

fn build_multipart(metadata: &str, data: &[u8]) -> Vec<u8> {
    let boundary = "boundary_stoclove";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(b"Content-Type: application/json; charset=UTF-8\r\n\r\n");
    body.extend_from_slice(metadata.as_bytes());
    body.extend_from_slice(format!("\r\n--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(b"Content-Type: application/x-sqlite3\r\n\r\n");
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{}--", boundary).as_bytes());
    body
}

/// Convenience function: download remote DB, merge into local, then upload.
pub fn startup_sync(
    local_db: &crate::infra::db::Database,
    local_db_path: &str,
    access_token: &str,
) -> Result<()> {
    let drive = DriveSync::new(access_token.to_string());
    let remote_path = format!("{}.remote.sqlite", local_db_path);

    match drive.download(&remote_path) {
        Ok(()) => {
            log::info!("Remote DB downloaded, merging...");
            let report = local_db.merge_from_file(&remote_path)?;
            log::info!("Merged {} recipes", report.upserted);
            let _ = std::fs::remove_file(&remote_path);
        }
        Err(e) => {
            log::warn!("Could not download remote DB (first run?): {}", e);
        }
    }

    // Export current local DB and upload
    let export_path = format!("{}.export.sqlite", local_db_path);
    local_db.export_to_file(&export_path)?;
    drive.upload(&export_path)?;
    let _ = std::fs::remove_file(&export_path);
    Ok(())
}
