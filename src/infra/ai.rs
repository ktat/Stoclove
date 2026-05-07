use anyhow::{Context, Result};
use serde::Deserialize;
use std::sync::Arc;

use crate::domain::recipe::{RawIngredient, Substitute};

// ── Public trait ──────────────────────────────────────────────────────────────

pub trait LlmClient: Send + Sync {
    /// テキストを解析し、レシピでなければ None を返す。
    fn analyze_recipe(&self, text: &str) -> Result<Option<AnalyzedRecipe>>;
}

pub struct AnalyzedRecipe {
    pub title: String,
    pub ingredients: Vec<RawIngredient>,
    pub instructions: String,
    pub substitutes: Vec<Substitute>,
}

pub trait OcrClient: Send + Sync {
    fn extract_text(&self, image_path: &str) -> Result<String>;
}

// ── Serde 型: LLM が返す JSON ─────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct LlmOutput {
    #[serde(default)]
    is_recipe: bool,
    #[serde(default)]
    title: String,
    #[serde(default)]
    ingredients: Vec<LlmIngredient>,
    #[serde(default)]
    instructions: String,
    #[serde(default)]
    substitutes: Vec<LlmSubstitute>,
}

#[derive(Deserialize, Default)]
struct LlmIngredient {
    #[serde(default)]
    name: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    unit: String,
}

#[derive(Deserialize, Default)]
struct LlmSubstitute {
    #[serde(default)]
    ingredient: String,
    #[serde(default)]
    alternatives: Vec<String>,
    #[serde(default)]
    note: Option<String>,
}

// ── テキスト前処理 ─────────────────────────────────────────────────────────────

/// LLM に渡す前にノイズを除去する。
/// - 1文字だけの行（A/B 等の材料グループ記号、ステップ番号など）を除外
/// - 【...】で始まる行（画像キャプション）を除外
/// - 記号のみの行（! 等のアイコン）を除外
/// - 重複空行をまとめる
pub fn preprocess_text(text: &str) -> String {
    text.lines()
        .filter_map(|line| {
            let t = line.trim();
            if t.is_empty() {
                return None;
            }
            // 1文字だけ (A B 1 2 等)
            if t.chars().count() == 1 {
                return None;
            }
            // 画像キャプション 【...】
            if t.starts_with('【') {
                return None;
            }
            // 記号のみの行
            if t.chars().all(|c| !c.is_alphanumeric() && !"ぁ-ん".contains(c) && !"ァ-ン".contains(c) && !('\u{4E00}'..='\u{9FFF}').contains(&c)) {
                return None;
            }
            Some(t)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ── JSON ユーティリティ ────────────────────────────────────────────────────────

fn extract_json(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, b) in raw[start..].bytes().enumerate() {
        let ch = b as char;
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' && in_string {
            escape = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_llm_output(raw: &str) -> Result<LlmOutput> {
    let json_str = extract_json(raw).ok_or_else(|| {
        anyhow::anyhow!(
            "LLM が JSON を出力しませんでした。出力: {}",
            &raw[..raw.len().min(300)]
        )
    })?;
    serde_json::from_str(json_str)
        .with_context(|| format!("JSON パース失敗: {}", &json_str[..json_str.len().min(300)]))
}

fn llm_output_to_result(parsed: LlmOutput) -> Option<AnalyzedRecipe> {
    if !parsed.is_recipe {
        return None;
    }
    let ingredients = parsed
        .ingredients
        .into_iter()
        .filter(|i| !i.name.trim().is_empty())
        .map(|i| RawIngredient {
            name: i.name,
            amount: i.amount,
            unit: i.unit,
        })
        .collect();

    let substitutes = parsed
        .substitutes
        .into_iter()
        .filter(|s| !s.ingredient.trim().is_empty())
        .map(|s| Substitute {
            ingredient: s.ingredient,
            alternatives: s.alternatives,
            note: s.note,
        })
        .collect();

    Some(AnalyzedRecipe {
        title: if parsed.title.is_empty() {
            "名称未設定のレシピ".into()
        } else {
            parsed.title
        },
        ingredients,
        instructions: parsed.instructions,
        substitutes,
    })
}

// ── プロンプト ─────────────────────────────────────────────────────────────────

fn build_prompt(text: &str) -> String {
    format!(
        r#"Analyze the text below. If it is NOT a recipe, return: {{"is_recipe": false}}

If it IS a recipe, return ONLY this JSON (no markdown, no explanation):
{{
  "is_recipe": true,
  "title": "recipe name (keep original language)",
  "ingredients": [
    {{"name": "ingredient name", "amount": "numeric quantity or empty string", "unit": "unit like g/ml/cup/tbsp/大さじ/小さじ or empty string"}}
  ],
  "instructions": "full cooking steps joined by \\n (keep original language)",
  "substitutes": [
    {{"ingredient": "hard-to-find ingredient", "alternatives": ["substitute 1", "substitute 2"], "note": "tip or null"}}
  ]
}}

Rules:
- Preserve original language (Japanese, English, etc.)
- Separate number from unit: "200" + "g", NOT "200g"; "1" + "合"; "大さじ" + "1"
- Use "" for missing amount/unit
- substitutes: only for ingredients hard to find at regular supermarkets; [] if none
- Japanese recipes often list ingredient name on one line and amount on the next line

Text:
{}"#,
        text
    )
}

// ── OllamaLlm ─────────────────────────────────────────────────────────────────

pub struct OllamaLlm {
    client: reqwest::blocking::Client,
    base_url: String,
    model: String,
}

impl OllamaLlm {
    pub fn new(base_url: &str, model: &str) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .unwrap_or_default();
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
        }
    }
}

impl LlmClient for OllamaLlm {
    fn analyze_recipe(&self, text: &str) -> Result<Option<AnalyzedRecipe>> {
        let cleaned = preprocess_text(text);
        let prompt = build_prompt(&cleaned);

        let body = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "format": "json",
            "stream": false,
            "options": { "temperature": 0.1, "num_predict": 2048 }
        });

        log::debug!("Calling Ollama model '{}'...", self.model);

        let resp: serde_json::Value = self
            .client
            .post(format!("{}/api/generate", self.base_url))
            .json(&body)
            .send()
            .context("Ollama へのリクエストに失敗しました")?
            .json()
            .context("Ollama レスポンスのパースに失敗しました")?;

        let raw = resp["response"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Ollama レスポンスに response フィールドがありません"))?;

        log::debug!("Ollama raw output: {}", &raw[..raw.len().min(500)]);

        let parsed = parse_llm_output(raw)?;
        Ok(llm_output_to_result(parsed))
    }
}

// ── MockLlm (フォールバック / Ollama なし時) ─────────────────────────────────

/// 日本語レシピの多行フォーマット (食材名の次の行に分量) に対応したヒューリスティック解析。
pub struct MockLlm;

impl LlmClient for MockLlm {
    fn analyze_recipe(&self, raw_text: &str) -> Result<Option<AnalyzedRecipe>> {
        let text = preprocess_text(raw_text);
        let lines: Vec<&str> = text.lines().collect();

        // セクション種別
        const PREAMBLE: u8 = 0;
        const INGREDIENTS: u8 = 1;
        const INSTRUCTIONS: u8 = 2;

        let mut title = String::new();
        let mut ingredients: Vec<RawIngredient> = Vec::new();
        let mut instructions_lines: Vec<String> = Vec::new();
        let mut section = PREAMBLE;

        for line in &lines {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            let lower = t.to_lowercase();

            // セクション切り替え
            if lower == "材料"
                || lower.starts_with("材料（")
                || lower.starts_with("材料(")
                || lower.contains("ingredient")
            {
                section = INGREDIENTS;
                continue;
            }
            if lower == "作り方"
                || lower == "手順"
                || lower == "下準備"
                || lower.contains("instruction")
                || lower.contains("direction")
                || lower.contains("step")
            {
                section = INSTRUCTIONS;
                continue;
            }

            match section {
                PREAMBLE => {
                    // 人数表記 (2人分 等) はスキップ
                    if t.ends_with("人分") || t.ends_with("人前") {
                        continue;
                    }
                    if title.is_empty() {
                        title = t.to_string();
                    }
                }
                INGREDIENTS => {
                    // 人数表記はスキップ
                    if t.ends_with("人分") || t.ends_with("人前") {
                        continue;
                    }
                    // 「大さじN」「小さじN」「N合」「Ng」等だけの行 = 直前食材の分量行
                    if is_amount_only(t) {
                        if let Some(last) = ingredients.last_mut() {
                            if last.amount.is_empty() {
                                let (amount, unit) = split_amount(t);
                                last.amount = amount;
                                last.unit = unit;
                            }
                        }
                    } else {
                        // 「食材名 分量」が1行に入っている場合
                        let (name, amount, unit) = parse_ingredient_line(t);
                        ingredients.push(RawIngredient { name, amount, unit });
                    }
                }
                INSTRUCTIONS => {
                    instructions_lines.push(t.to_string());
                }
                _ => {}
            }
        }

        if title.is_empty() {
            title = "名称未設定のレシピ".into();
        }

        if ingredients.is_empty() && instructions_lines.is_empty() {
            return Ok(None);
        }
        if ingredients.is_empty() {
            ingredients.push(RawIngredient {
                name: "手順参照".into(),
                amount: String::new(),
                unit: String::new(),
            });
        }

        Ok(Some(AnalyzedRecipe {
            title,
            ingredients,
            instructions: instructions_lines.join("\n"),
            substitutes: Vec::new(),
        }))
    }
}

/// 分量だけの行かどうかを判定する。
/// 例: "大さじ1" "小さじ1/2" "1合" "200g" "適量" "少々"
fn is_amount_only(s: &str) -> bool {
    if s == "適量" || s == "少々" || s == "少量" {
        return true;
    }
    // 先頭が数字
    if s.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        return true;
    }
    // 大さじ/小さじ で始まる
    if s.starts_with("大さじ") || s.starts_with("小さじ") || s.starts_with("カップ") {
        return true;
    }
    false
}

/// 「大さじ1」→ ("1", "大さじ")、「200g」→ ("200", "g") のように分割する。
fn split_amount(s: &str) -> (String, String) {
    if s == "適量" || s == "少々" || s == "少量" {
        return (s.to_string(), String::new());
    }
    // 大さじ/小さじ/カップ を先頭単位として扱う
    for prefix in ["大さじ", "小さじ", "カップ"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return (rest.trim().to_string(), prefix.to_string());
        }
    }
    // 末尾の単位を分離 (200g → "200", "g" など)
    let digit_end = s
        .find(|c: char| !c.is_ascii_digit() && c != '/' && c != '.')
        .unwrap_or(s.len());
    let amount = s[..digit_end].to_string();
    let unit = s[digit_end..].trim().to_string();
    (amount, unit)
}

/// 1行に食材名と分量が入っている場合のパース。
/// "薄力粉 200g" → ("薄力粉", "200", "g")
fn parse_ingredient_line(s: &str) -> (String, String, String) {
    // 末尾の空白区切りで分割を試みる
    let parts: Vec<&str> = s.splitn(2, ' ').collect();
    match parts.as_slice() {
        [name, amount_str] => {
            let (amount, unit) = split_amount(amount_str.trim());
            (name.to_string(), amount, unit)
        }
        _ => (s.to_string(), String::new(), String::new()),
    }
}

pub struct MockOcr;
impl OcrClient for MockOcr {
    fn extract_text(&self, image_path: &str) -> Result<String> {
        log::info!("Mock OCR: {}", image_path);
        Ok("チョコレートケーキ\n材料\n200 g 薄力粉\n100 g バター\n2 個 卵\n作り方\n材料を混ぜて180度で30分焼く".into())
    }
}
