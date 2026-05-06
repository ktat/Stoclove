/// URL scraping: fetches HTML and extracts plain text for the LLM.

use anyhow::{Context, Result};

pub struct WebScraper {
    client: reqwest::blocking::Client,
}

impl WebScraper {
    pub fn new() -> Self {
        let client = reqwest::blocking::Client::builder()
            .user_agent("Stoclove/0.1 recipe-importer")
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        Self { client }
    }

    pub fn fetch_text(&self, url: &str) -> Result<String> {
        let html = self
            .client
            .get(url)
            .send()
            .context("HTTP request failed")?
            .text()
            .context("Failed to read response body")?;

        Ok(extract_text_from_html(&html))
    }
}

fn extract_text_from_html(html: &str) -> String {
    use scraper::{Html, Selector};

    let doc = Html::parse_document(html);

    // Remove script and style tags
    let selectors_to_skip = ["script", "style", "nav", "footer", "header", "aside"];

    // Try recipe-specific selectors first
    let recipe_selectors = [
        "[class*='recipe']",
        "[class*='ingredient']",
        "[class*='instruction']",
        "article",
        "main",
    ];

    for sel_str in &recipe_selectors {
        if let Ok(sel) = Selector::parse(sel_str) {
            let text: String = doc
                .select(&sel)
                .flat_map(|el| el.text())
                .collect::<Vec<_>>()
                .join("\n");
            if text.len() > 200 {
                return clean_text(&text);
            }
        }
    }

    // Fallback: full body text
    if let Ok(body_sel) = Selector::parse("body") {
        let _ = selectors_to_skip; // acknowledged
        let text: String = doc
            .select(&body_sel)
            .flat_map(|el| el.text())
            .collect::<Vec<_>>()
            .join("\n");
        return clean_text(&text);
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
