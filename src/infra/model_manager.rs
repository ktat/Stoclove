use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use anyhow::{Context, Result};

pub const OLLAMA_BASE_URL: &str = "http://localhost:11434";

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub size_gb: f32,
    pub speed_label: &'static str,
    pub accuracy_label: &'static str,
    pub recommended: bool,
    /// Ollama のモデル名 (例: "llama3.2:3b")
    pub ollama_model: &'static str,
}

pub static AVAILABLE_MODELS: &[ModelInfo] = &[
    ModelInfo {
        id: "gemma3-1b",
        name: "Gemma 3 1B",
        description: "軽量・高速。シンプルなレシピなら十分な精度。\n低スペックPCでも快適に動作します。",
        size_gb: 0.8,
        speed_label: "高速",
        accuracy_label: "△",
        recommended: false,
        ollama_model: "gemma3:1b",
    },
    ModelInfo {
        id: "llama32-3b",
        name: "Llama 3.2 3B  ★推奨",
        description: "精度と速度のバランスが良く、多言語対応も優秀。\nほとんどのレシピを正確に解析できます。",
        size_gb: 2.0,
        speed_label: "普通",
        accuracy_label: "○",
        recommended: true,
        ollama_model: "llama3.2:3b",
    },
    ModelInfo {
        id: "mistral-7b",
        name: "Mistral 7B",
        description: "高精度。複雑なレシピや多言語テキストに強い。\n高スペックPC向け (RAM 8GB 以上推奨)。",
        size_gb: 4.1,
        speed_label: "低速",
        accuracy_label: "◎",
        recommended: false,
        ollama_model: "mistral:7b",
    },
];

pub fn model_by_id(id: &str) -> Option<&'static ModelInfo> {
    AVAILABLE_MODELS.iter().find(|m| m.id == id)
}

fn make_client(timeout_secs: u64) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(if timeout_secs == 0 {
            None
        } else {
            Some(std::time::Duration::from_secs(timeout_secs))
        })
        .build()
        .unwrap_or_default()
}

/// Ollama が起動しているか確認する。
pub fn is_ollama_running(base_url: &str) -> bool {
    make_client(3)
        .get(format!("{}/api/tags", base_url))
        .send()
        .is_ok()
}

/// Ollama にプルされているモデルの中から既知モデルを探す。
pub fn find_installed_model(base_url: &str) -> Option<&'static ModelInfo> {
    let resp: serde_json::Value = make_client(3)
        .get(format!("{}/api/tags", base_url))
        .send()
        .ok()?
        .json()
        .ok()?;

    let names: Vec<&str> = resp["models"]
        .as_array()?
        .iter()
        .filter_map(|m| m["name"].as_str())
        .collect();

    AVAILABLE_MODELS
        .iter()
        .find(|info| names.iter().any(|n| *n == info.ollama_model))
}

/// Ollama の pull API でモデルをダウンロードする。進捗をアトミック変数で公開する。
pub fn pull_model(
    base_url: &str,
    ollama_model: &str,
    downloaded_bytes: Arc<AtomicU64>,
    total_bytes: Arc<AtomicU64>,
) -> Result<()> {
    let body = serde_json::json!({ "model": ollama_model, "stream": true });

    let response = make_client(0)
        .post(format!("{}/api/pull", base_url))
        .json(&body)
        .send()
        .context("Ollama pull request failed")?;

    let reader = BufReader::new(response);
    for line in reader.lines() {
        let line = line.context("Error reading pull stream")?;
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some(c) = v["completed"].as_u64() {
                downloaded_bytes.store(c, Ordering::Relaxed);
            }
            if let Some(t) = v["total"].as_u64() {
                total_bytes.store(t, Ordering::Relaxed);
            }
            if v["status"].as_str() == Some("success") {
                break;
            }
        }
    }

    log::info!("Model '{}' pulled successfully", ollama_model);
    Ok(())
}

pub fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        return "---".into();
    }
    let gb = bytes as f64 / 1_073_741_824.0;
    if gb >= 1.0 {
        format!("{:.2} GB", gb)
    } else {
        format!("{:.0} MB", bytes as f64 / 1_048_576.0)
    }
}
