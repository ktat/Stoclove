use std::sync::{Arc, Mutex};
use anyhow::Result;
use slint::{ModelRc, VecModel};

use crate::domain::ingredient::Ingredient;
use crate::domain::recipe::{Recipe, Substitute};
use crate::infra::ai::{LlmClient, MockLlm};
use crate::infra::db::Database;
use crate::infra::scraper::WebScraper;

slint::include_modules!();

fn to_recipe_model(recipe: &Recipe) -> RecipeModel {
    let names: Vec<String> = recipe.ingredients.iter()
        .take(4)
        .map(|i| i.name.clone())
        .collect();
    let ingredient_names = if names.is_empty() {
        "No ingredients listed".into()
    } else {
        names.join(", ")
    };

    RecipeModel {
        id: recipe.id.as_str().into(),
        title: recipe.title.as_str().into(),
        source_url: recipe.source_url.as_deref().unwrap_or("").into(),
        instructions: recipe.instructions.as_str().into(),
        substitutes_json: recipe.substitutes_json.as_deref().unwrap_or("").into(),
        ingredient_names: ingredient_names.into(),
        ingredient_count: recipe.ingredients.len() as i32,
    }
}

fn to_ingredient_models(ingredients: &[Ingredient]) -> Vec<IngredientModel> {
    ingredients.iter().map(|i| IngredientModel {
        recipe_id: i.recipe_id.as_str().into(),
        name: i.name.as_str().into(),
        amount: i.amount.as_str().into(),
        unit: i.unit.as_str().into(),
    }).collect()
}

fn to_substitute_models(subs: &[Substitute]) -> Vec<SubstituteModel> {
    subs.iter().map(|s| SubstituteModel {
        ingredient: s.ingredient.as_str().into(),
        alternatives: s.alternatives.join(", ").into(),
        note: s.note.as_deref().unwrap_or("").into(),
    }).collect()
}

pub fn run_app(db_path: &str) -> Result<()> {
    let db = Arc::new(Mutex::new(Database::open(db_path)?));
    let llm: Arc<dyn LlmClient> = Arc::new(MockLlm);
    let scraper = Arc::new(WebScraper::new());

    let ui = AppWindow::new()?;

    // Initial recipe load
    {
        let db = db.lock().unwrap();
        let recipes = db.list_recipes().unwrap_or_default();
        let models: Vec<RecipeModel> = recipes.iter().map(to_recipe_model).collect();
        ui.set_recipes(ModelRc::new(VecModel::from(models)));

        let all_ings = db.all_ingredients().unwrap_or_default();
        ui.set_stock_ingredients(ModelRc::new(VecModel::from(to_ingredient_models(&all_ings))));
    }

    // ── search-changed ────────────────────────────────────────────────────────
    {
        let db = db.clone();
        let ui_handle = ui.as_weak();
        ui.on_search_changed(move |query| {
            let db = db.lock().unwrap();
            let q = query.to_string();

            // Search both by ingredient (FTS5) and by title
            let mut by_ing = db.search_by_ingredient(&q).unwrap_or_default();
            let by_title = db.search_by_title(&q).unwrap_or_default();

            // Merge results, deduplicate
            for r in by_title {
                if !by_ing.iter().any(|x| x.id == r.id) {
                    by_ing.push(r);
                }
            }

            let models: Vec<RecipeModel> = by_ing.iter().map(to_recipe_model).collect();
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_recipes(ModelRc::new(VecModel::from(models)));
            }
        });
    }

    // ── recipe-selected ───────────────────────────────────────────────────────
    {
        let db = db.clone();
        let ui_handle = ui.as_weak();
        ui.on_recipe_selected(move |id| {
            let db = db.lock().unwrap();
            if let Ok(Some(recipe)) = db.get_recipe(&id) {
                let subs = recipe.substitutes();
                let ing_models = to_ingredient_models(&recipe.ingredients);
                let sub_models = to_substitute_models(&subs);

                if let Some(ui) = ui_handle.upgrade() {
                    ui.set_selected_recipe(to_recipe_model(&recipe));
                    ui.set_detail_ingredients(ModelRc::new(VecModel::from(ing_models)));
                    ui.set_detail_substitutes(ModelRc::new(VecModel::from(sub_models)));
                    ui.set_current_view(AppView::Detail);
                }
            }
        });
    }

    // ── back-pressed ──────────────────────────────────────────────────────────
    {
        let ui_handle = ui.as_weak();
        ui.on_back_pressed(move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_current_view(AppView::Main);
            }
        });
    }

    // ── navigate-stock ────────────────────────────────────────────────────────
    {
        let db = db.clone();
        let ui_handle = ui.as_weak();
        ui.on_navigate_stock(move || {
            let db = db.lock().unwrap();
            let ings = db.all_ingredients().unwrap_or_default();
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_stock_ingredients(ModelRc::new(VecModel::from(to_ingredient_models(&ings))));
                ui.set_current_view(AppView::Stock);
            }
        });
    }

    // ── navigate-main ─────────────────────────────────────────────────────────
    {
        let ui_handle = ui.as_weak();
        ui.on_navigate_main(move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_current_view(AppView::Main);
            }
        });
    }

    // ── navigate-add ──────────────────────────────────────────────────────────
    {
        let ui_handle = ui.as_weak();
        ui.on_navigate_add(move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_current_view(AppView::AddRecipe);
            }
        });
    }

    // ── add-from-url ──────────────────────────────────────────────────────────
    {
        let db = db.clone();
        let llm = llm.clone();
        let scraper = scraper.clone();
        let ui_handle = ui.as_weak();

        ui.on_add_from_url(move |url| {
            let url_str = url.to_string();
            let db = db.clone();
            let llm = llm.clone();
            let scraper = scraper.clone();
            let ui_handle = ui_handle.clone();

            if let Some(ui) = ui_handle.upgrade() {
                ui.set_status_message("Fetching recipe from URL…".into());
                ui.set_current_view(AppView::Main);
            }

            std::thread::spawn(move || {
                let result = import_from_url(&url_str, &db, &llm, &scraper);
                let ui_handle = ui_handle.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        match result {
                            Ok(_) => {
                                let db = db.lock().unwrap();
                                let recipes = db.list_recipes().unwrap_or_default();
                                let models: Vec<RecipeModel> = recipes.iter().map(to_recipe_model).collect();
                                ui.set_recipes(ModelRc::new(VecModel::from(models)));
                                ui.set_status_message("Recipe imported!".into());
                            }
                            Err(e) => {
                                ui.set_status_message(format!("Error: {}", e).into());
                            }
                        }
                    }
                }).ok();
            });
        });
    }

    // ── add-from-text ─────────────────────────────────────────────────────────
    {
        let db = db.clone();
        let llm = llm.clone();
        let ui_handle = ui.as_weak();

        ui.on_add_from_text(move |text| {
            let text_str = text.to_string();
            let db = db.clone();
            let llm = llm.clone();
            let ui_handle = ui_handle.clone();

            if let Some(ui) = ui_handle.upgrade() {
                ui.set_current_view(AppView::Main);
            }

            std::thread::spawn(move || {
                let result = import_from_text(&text_str, None, &db, &llm);
                let ui_handle = ui_handle.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        match result {
                            Ok(_) => {
                                let db = db.lock().unwrap();
                                let recipes = db.list_recipes().unwrap_or_default();
                                let models: Vec<RecipeModel> = recipes.iter().map(to_recipe_model).collect();
                                ui.set_recipes(ModelRc::new(VecModel::from(models)));
                                ui.set_status_message("Recipe imported!".into());
                            }
                            Err(e) => {
                                ui.set_status_message(format!("Error: {}", e).into());
                            }
                        }
                    }
                }).ok();
            });
        });
    }

    // ── delete-recipe ─────────────────────────────────────────────────────────
    {
        let db = db.clone();
        let ui_handle = ui.as_weak();
        ui.on_delete_recipe(move |id| {
            let db = db.lock().unwrap();
            if db.soft_delete_recipe(&id).is_ok() {
                let recipes = db.list_recipes().unwrap_or_default();
                let models: Vec<RecipeModel> = recipes.iter().map(to_recipe_model).collect();
                if let Some(ui) = ui_handle.upgrade() {
                    ui.set_recipes(ModelRc::new(VecModel::from(models)));
                    ui.set_status_message("Recipe deleted.".into());
                }
            }
        });
    }

    // ── sync-pressed ──────────────────────────────────────────────────────────
    {
        let ui_handle = ui.as_weak();
        ui.on_sync_pressed(move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_status_message(
                    "Sync requires a Google Drive access token. Set STOCLOVE_DRIVE_TOKEN env var.".into()
                );
            }
        });
    }

    ui.run()?;
    Ok(())
}

fn import_from_url(
    url: &str,
    db: &Arc<Mutex<Database>>,
    llm: &Arc<dyn LlmClient>,
    scraper: &Arc<WebScraper>,
) -> Result<()> {
    let raw_text = scraper.fetch_text(url)?;
    import_from_text(&raw_text, Some(url.to_string()), db, llm)
}

fn import_from_text(
    text: &str,
    source_url: Option<String>,
    db: &Arc<Mutex<Database>>,
    llm: &Arc<dyn LlmClient>,
) -> Result<()> {
    let raw = llm.structure_recipe(text)?;
    let ingredient_names: Vec<String> = raw.ingredients.iter().map(|i| i.name.clone()).collect();
    let subs = llm.suggest_substitutes(&ingredient_names)?;

    let id = uuid::Uuid::new_v4().to_string();
    let mut recipe = Recipe::new(id.clone(), raw.title, source_url, raw.instructions);
    recipe.substitutes_json = if subs.is_empty() {
        None
    } else {
        serde_json::to_string(&subs).ok()
    };
    recipe.ingredients = raw.ingredients.iter().map(|ri| {
        crate::domain::ingredient::Ingredient::new(
            id.clone(),
            ri.name.clone(),
            ri.amount.clone(),
            ri.unit.clone(),
        )
    }).collect();

    let db = db.lock().unwrap();
    db.upsert_recipe(&recipe)?;
    Ok(())
}
