use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Ingredient {
    pub recipe_id: String,
    pub name: String,
    pub amount: String,
    pub unit: String,
    pub normalized_name: String,
}

impl Ingredient {
    pub fn new(recipe_id: String, name: String, amount: String, unit: String) -> Self {
        let normalized_name = normalize_name(&name);
        Self {
            recipe_id,
            name,
            amount,
            unit,
            normalized_name,
        }
    }
}

/// Normalize ingredient name for consistent indexing and search.
/// Lowercases, strips punctuation, removes common filler words.
pub fn normalize_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let stripped: String = lower
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect();

    let stop_words = ["fresh", "dried", "chopped", "sliced", "diced", "minced", "large", "small", "medium"];
    let tokens: Vec<&str> = stripped
        .split_whitespace()
        .filter(|w| !stop_words.contains(w))
        .collect();

    tokens.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_name() {
        assert_eq!(normalize_name("Fresh Garlic"), "garlic");
        assert_eq!(normalize_name("Chopped Onion"), "onion");
        assert_eq!(normalize_name("Olive Oil"), "olive oil");
    }
}
