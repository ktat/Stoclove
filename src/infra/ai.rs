use anyhow::{Context, Result};
use serde::Deserialize;
use std::sync::Arc;

use crate::domain::recipe::{RawIngredient, Substitute};

// ── Public trait ──────────────────────────────────────────────────────────────

pub trait LlmClient: Send + Sync {
    /// テキストを解析し、レシピでなければ None を返す。
    /// レシピであれば食材・手順・代替案を含む AnalyzedRecipe を返す。
    fn analyze_recipe(&self, text: &str) -> Result<Option<AnalyzedRecipe>>;
}

pub struct AnalyzedRecipe {
    pub title: String,
    pub ingredients: Vec<RawIngredient>,
    pub instructions: String,
    pub substitutes: Vec<Substitute>,
}

// ── OCR trait (platform FFI 向け) ─────────────────────────────────────────────

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

/// LLM 出力から最初の完全な JSON オブジェクトを抽出する。
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
        let preview = &raw[..raw.len().min(300)];
        anyhow::anyhow!("LLM が JSON を出力しませんでした。出力: {}", preview)
    })?;
    serde_json::from_str(json_str)
        .with_context(|| format!("JSON のパースに失敗: {}", json_str))
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

fn build_user_prompt(text: &str) -> String {
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
- substitutes only for ingredients hard to find at regular supermarkets; [] if none

Text:
{}"#,
        text
    )
}

fn format_prompt_for_model(model_filename: &str, user_content: &str) -> String {
    let f = model_filename.to_lowercase();
    if f.contains("llama-3") || f.contains("llama3") {
        format!(
            "<|begin_of_text|><|start_header_id|>system<|end_header_id|>\n\n\
             You are a recipe extraction assistant. Respond with valid JSON only, no other text.\
             <|eot_id|><|start_header_id|>user<|end_header_id|>\n\n{}\
             <|eot_id|><|start_header_id|>assistant<|end_header_id|>\n\n",
            user_content
        )
    } else if f.contains("gemma") {
        format!(
            "<start_of_turn>user\n\
             You are a recipe extraction assistant. Respond with valid JSON only.\n\n{}\
             <end_of_turn>\n<start_of_turn>model\n",
            user_content
        )
    } else if f.contains("mistral") {
        format!(
            "[INST] You are a recipe extraction assistant. Respond with valid JSON only.\n\n{} [/INST]",
            user_content
        )
    } else {
        // Generic instruct template
        format!(
            "<|system|>You are a recipe extraction assistant. Respond with JSON only.</s>\
             <|user|>{}</s><|assistant|>",
            user_content
        )
    }
}

// ── LlamaCppLlm ───────────────────────────────────────────────────────────────

use llama_cpp_2::{
    context::params::LlamaContextParams,
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::{params::LlamaModelParams, AddBos, Special},
    model::LlamaModel,
    sampling::LlamaSampler,
};
use std::num::NonZeroU32;

pub struct LlamaCppLlm {
    backend: Arc<LlamaBackend>,
    model: Arc<LlamaModel>,
    model_filename: String,
}

// llama-cpp-2 は LlamaModel / LlamaBackend に unsafe impl Send+Sync を提供している
unsafe impl Send for LlamaCppLlm {}
unsafe impl Sync for LlamaCppLlm {}

impl LlamaCppLlm {
    pub fn load(model_path: &str) -> Result<Self> {
        log::info!("Loading model: {}", model_path);
        let backend = LlamaBackend::init().context("llama.cpp バックエンドの初期化に失敗")?;
        let model_params = LlamaModelParams::default();
        let model = LlamaModel::load_from_file(&backend, model_path, &model_params)
            .context("モデルファイルの読み込みに失敗")?;

        let filename = std::path::Path::new(model_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        log::info!("Model loaded: {}", filename);
        Ok(Self {
            backend: Arc::new(backend),
            model: Arc::new(model),
            model_filename: filename,
        })
    }

    fn run_inference(&self, prompt: &str, max_new_tokens: usize) -> Result<String> {
        // トークナイズはコンテキスト生成前に行う
        let prompt_tokens = self
            .model
            .str_to_token(prompt, AddBos::Always)
            .context("Tokenization failed")?;
        let n_prompt = prompt_tokens.len();
        if n_prompt == 0 {
            return Ok(String::new());
        }

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(Some(NonZeroU32::new(4096).unwrap()));
        let mut ctx = self
            .model
            .new_context(&self.backend, ctx_params)
            .context("コンテキストの作成に失敗")?;

        // プロンプト全体をバッチにロード
        let mut batch = LlamaBatch::new(n_prompt, 1);
        for (i, &tok) in prompt_tokens.iter().enumerate() {
            batch.add(tok, i as i32, &[0], i == n_prompt - 1)?;
        }
        ctx.decode(&mut batch).context("Prompt decode failed")?;

        let mut sampler = LlamaSampler::greedy();
        let mut n_cur = n_prompt as i32;
        let mut output = String::new();

        loop {
            let new_token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(new_token);

            if ctx.model.is_eog_token(new_token)
                || (n_cur - n_prompt as i32) >= max_new_tokens as i32
            {
                break;
            }

            let piece = ctx
                .model
                .token_to_str(new_token, Special::Tokenize)
                .unwrap_or_default();
            output.push_str(&piece);

            // JSON が完結したら早期終了
            if extract_json(&output).is_some() {
                break;
            }

            batch.clear();
            batch.add(new_token, n_cur, &[0], true)?;
            n_cur += 1;
            ctx.decode(&mut batch)?;
        }

        Ok(output)
    }
}

impl LlmClient for LlamaCppLlm {
    fn analyze_recipe(&self, text: &str) -> Result<Option<AnalyzedRecipe>> {
        let user_prompt = build_user_prompt(text);
        let prompt = format_prompt_for_model(&self.model_filename, &user_prompt);

        log::debug!("Running LLM inference ({} chars prompt)...", prompt.len());
        let raw = self.run_inference(&prompt, 1024)?;
        log::debug!("LLM raw output: {}", &raw[..raw.len().min(500)]);

        let parsed = parse_llm_output(&raw)?;
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
                || lower.contains("direction")
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
            // テキストが短すぎる・意味を成さない場合はレシピではないと判定
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
