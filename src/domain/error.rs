use thiserror::Error;

#[derive(Error, Debug)]
pub enum DomainError {
    #[error("Recipe not found: {0}")]
    RecipeNotFound(String),

    #[error("Invalid ingredient data: {0}")]
    InvalidIngredient(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Sync error: {0}")]
    SyncError(String),

    #[error("AI processing error: {0}")]
    AiError(String),
}
