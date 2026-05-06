use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: &'static str,
    pub name: &'static str,
    /// 改行区切りの説明文 (UI に表示)
    pub description: &'static str,
    pub size_gb: f32,
    pub speed_label: &'static str,
    pub accuracy_label: &'static str,
    pub recommended: bool,
    pub url: &'static str,
    pub filename: &'static str,
}

pub static AVAILABLE_MODELS: &[ModelInfo] = &[
    ModelInfo {
        id: "gemma3-1b",
        name: "Gemma 3 1B",
        description: "軽量・高速。シンプルなレシピなら十分な精度。\n低スペックPCでも快適に動作します。",
        size_gb: 0.6,
        speed_label: "高速",
        accuracy_label: "△",
        recommended: false,
        url: "https://huggingface.co/bartowski/gemma-3-1b-it-GGUF/resolve/main/gemma-3-1b-it-Q4_K_M.gguf",
        filename: "gemma-3-1b-it-Q4_K_M.gguf",
    },
    ModelInfo {
        id: "llama32-3b",
        name: "Llama 3.2 3B  ★推奨",
        description: "精度と速度のバランスが良く、多言語対応も優秀。\nほとんどのレシピを正確に解析できます。",
        size_gb: 2.0,
        speed_label: "普通",
        accuracy_label: "○",
        recommended: true,
        url: "https://huggingface.co/bartowski/Llama-3.2-3B-Instruct-GGUF/resolve/main/Llama-3.2-3B-Instruct-Q4_K_M.gguf",
        filename: "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
    },
    ModelInfo {
        id: "mistral-7b",
        name: "Mistral 7B",
        description: "高精度。複雑なレシピや多言語テキストに強い。\n高スペックPC向け (RAM 8GB 以上推奨)。",
        size_gb: 4.1,
        speed_label: "低速",
        accuracy_label: "◎",
        recommended: false,
        url: "https://huggingface.co/bartowski/Mistral-7B-Instruct-v0.3-GGUF/resolve/main/Mistral-7B-Instruct-v0.3-Q4_K_M.gguf",
        filename: "Mistral-7B-Instruct-v0.3-Q4_K_M.gguf",
    },
];

/// モデルファイルがデータディレクトリに存在するか検索する。
pub fn find_installed_model(data_dir: &Path) -> Option<(&'static ModelInfo, PathBuf)> {
    for model in AVAILABLE_MODELS {
        let path = data_dir.join(model.filename);
        if path.exists() {
            return Some((model, path));
        }
    }
    None
}

pub fn model_by_id(id: &str) -> Option<&'static ModelInfo> {
    AVAILABLE_MODELS.iter().find(|m| m.id == id)
}

/// HuggingFace から GGUF ファイルをストリーミングダウンロードする。
/// `downloaded_bytes` / `total_bytes` をアトミックに更新して進捗を公開する。
pub fn download_model(
    model: &ModelInfo,
    dest_path: &Path,
    downloaded_bytes: Arc<AtomicU64>,
    total_bytes: Arc<AtomicU64>,
) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()
        .context("Failed to build HTTP client")?;

    let mut response = client
        .get(model.url)
        .send()
        .context("Download request failed")?;

    if !response.status().is_success() {
        anyhow::bail!("Server returned {}: {}", response.status(), model.url);
    }

    let total = response.content_length().unwrap_or(0);
    total_bytes.store(total, Ordering::Relaxed);

    // 一時ファイルに書いて成功時にリネーム
    let tmp_path = dest_path.with_extension("part");
    let mut file = std::fs::File::create(&tmp_path)
        .with_context(|| format!("Cannot create temp file: {}", tmp_path.display()))?;

    let mut buf = vec![0u8; 131_072]; // 128 KB chunks
    let mut downloaded = 0u64;

    loop {
        let n = response.read(&mut buf).context("Read error during download")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("Write error during download")?;
        downloaded += n as u64;
        downloaded_bytes.store(downloaded, Ordering::Relaxed);
    }

    file.flush()?;
    drop(file);
    std::fs::rename(&tmp_path, dest_path)
        .context("Failed to rename downloaded file")?;

    log::info!("Model downloaded to {}", dest_path.display());
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
