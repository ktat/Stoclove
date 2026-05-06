use serde::{Deserialize, Serialize};
use crate::domain::ingredient::Ingredient;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    pub id: String,
    pub title: String,
    pub source_url: Option<String>,
    pub instructions: String,
    pub substitutes_json: Option<String>,
    pub updated_at: i64,
    pub is_deleted: bool,
    pub ingredients: Vec<Ingredient>,
}

impl Recipe {
    pub fn new(
        id: String,
        title: String,
        source_url: Option<String>,
        instructions: String,
    ) -> Self {
        let updated_at = chrono::Utc::now().timestamp();
        Self {
            id,
            title,
            source_url,
            instructions,
            substitutes_json: None,
            updated_at,
            is_deleted: false,
            ingredients: Vec::new(),
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = chrono::Utc::now().timestamp();
    }

    pub fn soft_delete(&mut self) {
        self.is_deleted = true;
        self.touch();
    }

    pub fn substitutes(&self) -> Vec<Substitute> {
        self.substitutes_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Substitute {
    pub ingredient: String,
    pub alternatives: Vec<String>,
    pub note: Option<String>,
}

/// Raw AI-structured recipe before being persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawRecipe {
    pub title: String,
    pub ingredients: Vec<RawIngredient>,
    pub instructions: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawIngredient {
    pub name: String,
    pub amount: String,
    pub unit: String,
}
