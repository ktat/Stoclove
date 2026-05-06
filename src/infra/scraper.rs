/// URL scraping: fetches HTML and extracts plain text for the LLM.
/// Priority: JSON-LD schema.org/Recipe → recipe-area CSS selectors → body fallback.

use anyhow::{Context, Result};

pub struct WebScraper {
    client: reqwest::blocking::Client,
}

impl WebScraper {
    pub fn new() -> Self {
        let client = reqwest::blocking::Client::builder()
            .user_agent(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
            )
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        Self { client }
    }

    pub fn fetch_text(&self, url: &str) -> Result<String> {
        let html = self
            .client
            .get(url)
            .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
            .header("Accept-Language", "ja,en;q=0.9")
            .send()
            .context("HTTP request failed")?
            .text()
            .context("Failed to read response body")?;

        let text = extract_text_from_html(&html);
        if text.is_empty() {
            anyhow::bail!("ページからテキストを取得できませんでした");
        }
        Ok(text)
    }
}

// ── JSON-LD (schema.org/Recipe) 抽出 ─────────────────────────────────────────

fn try_json_ld(html: &str) -> Option<String> {
    use scraper::{Html, Selector};

    let doc = Html::parse_document(html);
    let sel = Selector::parse("script[type='application/ld+json']").ok()?;

    for el in doc.select(&sel) {
        let raw: String = el.text().collect();
        // JSON 配列 or オブジェクト を両方試す
        let values: Vec<serde_json::Value> = if raw.trim_start().starts_with('[') {
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            serde_json::from_str::<serde_json::Value>(&raw)
                .ok()
                .map(|v| vec![v])
                .unwrap_or_default()
        };

        for v in &values {
            if let Some(text) = recipe_from_ld(v) {
                return Some(text);
            }
            // @graph 配列をたどる
            if let Some(graph) = v.get("@graph").and_then(|g| g.as_array()) {
                for node in graph {
                    if let Some(text) = recipe_from_ld(node) {
                        return Some(text);
                    }
                }
            }
        }
    }
    None
}

fn recipe_from_ld(v: &serde_json::Value) -> Option<String> {
    let type_val = v.get("@type")?;
    let is_recipe = match type_val {
        serde_json::Value::String(s) => s == "Recipe",
        serde_json::Value::Array(arr) => arr.iter().any(|t| t.as_str() == Some("Recipe")),
        _ => false,
    };
    if !is_recipe {
        return None;
    }

    let mut out = String::new();

    // タイトル
    if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
        out.push_str(name);
        out.push('\n');
    }

    // 概要
    if let Some(desc) = v.get("description").and_then(|d| d.as_str()) {
        out.push_str(desc);
        out.push('\n');
    }

    // 材料
    if let Some(ingredients) = v.get("recipeIngredient").and_then(|i| i.as_array()) {
        out.push_str("\n材料\n");
        for ing in ingredients {
            if let Some(s) = ing.as_str() {
                out.push_str(s);
                out.push('\n');
            }
        }
    }

    // 手順
    if let Some(instructions) = v.get("recipeInstructions") {
        out.push_str("\n作り方\n");
        match instructions {
            serde_json::Value::String(s) => {
                out.push_str(s);
                out.push('\n');
            }
            serde_json::Value::Array(steps) => {
                for (i, step) in steps.iter().enumerate() {
                    let text = step
                        .get("text")
                        .and_then(|t| t.as_str())
                        .or_else(|| step.as_str())
                        .unwrap_or("");
                    if !text.is_empty() {
                        out.push_str(&format!("{}. {}\n", i + 1, text));
                    }
                }
            }
            _ => {}
        }
    }

    if out.len() > 50 {
        Some(out)
    } else {
        None
    }
}

// ── HTML テキスト抽出 ─────────────────────────────────────────────────────────

fn extract_text_from_html(html: &str) -> String {
    // JSON-LD が取れればそれを優先
    if let Some(text) = try_json_ld(html) {
        log::debug!("Extracted recipe from JSON-LD ({} chars)", text.len());
        return text;
    }

    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);

    // レシピ系クラス名を幅広くカバー (日本語サイト含む)
    let recipe_selectors = [
        // 汎用 schema / recipe
        "[class*='recipe']",
        "[class*='Recipe']",
        // 材料
        "[class*='ingredient']",
        "[class*='material']",   // sirogohan.com 等
        "[class*='zairyo']",
        // 手順
        "[class*='instruction']",
        "[class*='howto']",       // sirogohan.com 等
        "[class*='step']",
        "[class*='procedure']",
        // 汎用コンテンツ
        "article",
        "main",
        "[role='main']",
    ];

    for sel_str in &recipe_selectors {
        if let Ok(sel) = Selector::parse(sel_str) {
            let text: String = doc
                .select(&sel)
                .flat_map(|el| el.text())
                .collect::<Vec<_>>()
                .join("\n");
            let cleaned = clean_text(&text);
            if cleaned.len() > 200 {
                log::debug!("Extracted via selector '{}' ({} chars)", sel_str, cleaned.len());
                return cleaned;
            }
        }
    }

    // フォールバック: body 全体 (nav/footer/script/style 除く)
    // script や style の内容を除外するため、テキストノードを持つ要素を個別に選択する
    let skip_tags = ["script", "style", "nav", "footer", "header", "aside", "noscript"];
    let include_sel_str = "p, li, h1, h2, h3, h4, td, dt, dd, span, div";
    if let Ok(body_sel) = Selector::parse(include_sel_str) {
        let text: String = doc
            .select(&body_sel)
            .filter(|el| {
                // 祖先に skip タグが含まれていないものだけ
                !el.ancestors().any(|a| {
                    a.value()
                        .as_element()
                        .map(|e| skip_tags.contains(&e.name()))
                        .unwrap_or(false)
                })
            })
            .flat_map(|el| el.text())
            .collect::<Vec<_>>()
            .join("\n");

        let cleaned = clean_text(&text);
        log::debug!("Extracted via body fallback ({} chars)", cleaned.len());
        return cleaned;
    }

    String::new()
}

fn clean_text(text: &str) -> String {
    text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
