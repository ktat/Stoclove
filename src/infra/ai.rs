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
    {{"name": "ingredient name", "amount": "numeric quantity or empty string", "unit": "unit like g/ml/cup/tbsp or empty string"}}
  ],
  "instructions": "full cooking steps joined by \\n (keep original language)",
  "substitutes": [
    {{"ingredient": "hard-to-find ingredient", "alternatives": ["substitute 1", "substitute 2"], "note": "tip or null"}}
  ]
}}

Rules:
- Preserve original language (Japanese, English, etc.)
- Separate number from unit: "200" + "g", NOT "200g"
- Use "" for missing amount/unit
- substitutes: only for ingredients hard to find at regular supermarkets; [] if none

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
        let prompt = build_prompt(text);

        let body = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "format": "json",
            "stream": false,
            "options": { "temperature": 0.1, "num_predict": 1024 }
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

// ── MockLlm (フォールバック) ──────────────────────────────────────────────────

pub struct MockLlm;

impl LlmClient for MockLlm {
    fn analyze_recipe(&self, raw_text: &str) -> Result<Option<AnalyzedRecipe>> {
        let lines: Vec<&str> = raw_text.lines().collect();
        let title = lines
            .first()
            .copied()
            .unwrap_or("名称未設定のレシピ")
            .trim()
            .to_string();

        let mut ingredients = Vec::new();
        let mut instructions_lines: Vec<&str> = Vec::new();
        let mut in_ingredients = false;
        let mut in_instructions = false;

        for line in lines.iter().skip(1) {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            let lower = t.to_lowercase();
            if lower.contains("ingredient") || lower.contains("材料") {
                in_ingredients = true;
                in_instructions = false;
                continue;
            }
            if lower.contains("instruction")
                || lower.contains("step")
                || lower.contains("作り方")
                || lower.contains("手順")
            {
                in_instructions = true;
                in_ingredients = false;
                continue;
            }
            if in_ingredients {
                let parts: Vec<&str> = t.splitn(3, ' ').collect();
                let (amount, unit, name) = match parts.as_slice() {
                    [a, u, n] => (a.to_string(), u.to_string(), n.to_string()),
                    [a, n] => (a.to_string(), String::new(), n.to_string()),
                    _ => (String::new(), String::new(), t.to_string()),
                };
                ingredients.push(RawIngredient { name, amount, unit });
            } else if in_instructions {
                instructions_lines.push(t);
            } else {
                instructions_lines.push(t);
            }
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

pub struct MockOcr;
impl OcrClient for MockOcr {
    fn extract_text(&self, image_path: &str) -> Result<String> {
        log::info!("Mock OCR: {}", image_path);
        Ok("チョコレートケーキ\n材料\n200 g 薄力粉\n100 g バター\n2 個 卵\n作り方\n材料を混ぜて180度で30分焼く".into())
    }
}
