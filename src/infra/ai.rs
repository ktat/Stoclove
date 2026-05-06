/// Mock AI interface for LLM/OCR processing.
/// In production this would call platform-specific APIs:
///   - iOS: CoreML / Vision framework via FFI
///   - Android: ML Kit / Gemini Nano via JNI
///   - Desktop: llama.cpp or Ollama over localhost

use anyhow::Result;
use crate::domain::recipe::{RawIngredient, RawRecipe};
use crate::domain::recipe::Substitute;

pub trait LlmClient: Send + Sync {
    /// Structure raw text into a RawRecipe.
    fn structure_recipe(&self, raw_text: &str) -> Result<RawRecipe>;

    /// Generate substitution suggestions for a list of ingredient names.
    fn suggest_substitutes(&self, ingredients: &[String]) -> Result<Vec<Substitute>>;
}

pub trait OcrClient: Send + Sync {
    /// Extract text from an image file (path).
    fn extract_text(&self, image_path: &str) -> Result<String>;
}

// ── Mock implementations ──────────────────────────────────────────────────────

pub struct MockLlm;

impl LlmClient for MockLlm {
    fn structure_recipe(&self, raw_text: &str) -> Result<RawRecipe> {
        // Heuristic parser used as a stand-in for the real LLM.
        // Splits the text by newlines and guesses sections.
        let lines: Vec<&str> = raw_text.lines().collect();
        let title = lines.first().unwrap_or(&"Untitled Recipe").trim().to_string();

        let mut ingredients = Vec::new();
        let mut instructions_lines = Vec::new();
        let mut in_ingredients = false;
        let mut in_instructions = false;

        for line in &lines[1..] {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let lower = trimmed.to_lowercase();
            if lower.contains("ingredient") {
                in_ingredients = true;
                in_instructions = false;
                continue;
            }
            if lower.contains("instruction") || lower.contains("direction") || lower.contains("method") || lower.contains("step") {
                in_instructions = true;
                in_ingredients = false;
                continue;
            }
            if in_ingredients {
                // Try to parse "2 cups flour" -> amount=2, unit=cups, name=flour
                let parts: Vec<&str> = trimmed.splitn(3, ' ').collect();
                let (amount, unit, name) = match parts.as_slice() {
                    [a, u, n] => (a.to_string(), u.to_string(), n.to_string()),
                    [a, n] => (a.to_string(), String::new(), n.to_string()),
                    _ => (String::new(), String::new(), trimmed.to_string()),
                };
                ingredients.push(RawIngredient { name, amount, unit });
            } else if in_instructions {
                instructions_lines.push(trimmed);
            } else {
                // Default: treat as instruction
                instructions_lines.push(trimmed);
            }
        }

        if ingredients.is_empty() {
            ingredients.push(RawIngredient {
                name: "See instructions".into(),
                amount: String::new(),
                unit: String::new(),
            });
        }

        Ok(RawRecipe {
            title,
            ingredients,
            instructions: instructions_lines.join("\n"),
        })
    }

    fn suggest_substitutes(&self, ingredients: &[String]) -> Result<Vec<Substitute>> {
        // Mock substitution table for common hard-to-find ingredients
        let subs: Vec<Substitute> = ingredients
            .iter()
            .filter_map(|name| {
                let n = name.to_lowercase();
                if n.contains("buttermilk") {
                    Some(Substitute {
                        ingredient: name.clone(),
                        alternatives: vec!["milk + 1 tbsp lemon juice".into(), "plain yogurt".into()],
                        note: Some("Let sit 5 minutes before using".into()),
                    })
                } else if n.contains("cream of tartar") {
                    Some(Substitute {
                        ingredient: name.clone(),
                        alternatives: vec!["lemon juice".into(), "white vinegar".into()],
                        note: None,
                    })
                } else if n.contains("cake flour") {
                    Some(Substitute {
                        ingredient: name.clone(),
                        alternatives: vec!["all-purpose flour - 2 tbsp per cup + 2 tbsp cornstarch".into()],
                        note: Some("Sift together before use".into()),
                    })
                } else {
                    None
                }
            })
            .collect();
        Ok(subs)
    }
}

pub struct MockOcr;

impl OcrClient for MockOcr {
    fn extract_text(&self, image_path: &str) -> Result<String> {
        // In production: call Vision (iOS) or ML Kit (Android) via FFI
        log::info!("Mock OCR called for: {}", image_path);
        Ok(format!(
            "Mock OCR Result\nIngredients\n2 cups flour\n1 cup sugar\n3 eggs\nInstructions\nMix all ingredients. Bake at 180°C for 30 minutes."
        ))
    }
}
