use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use anyhow::Result;
use slint::{ModelRc, VecModel};

use crate::domain::ingredient::Ingredient;
use crate::domain::recipe::{Recipe, Substitute};
use crate::infra::ai::{LlmClient, MockLlm, OllamaLlm};
use crate::infra::db::Database;
use crate::infra::model_manager::{self, format_size, ModelInfo, AVAILABLE_MODELS, OLLAMA_BASE_URL};
use crate::infra::scraper::WebScraper;

slint::include_modules!();

// ── Slint モデル変換 ──────────────────────────────────────────────────────────

fn to_recipe_model(recipe: &Recipe) -> RecipeModel {
    let names: Vec<String> = recipe.ingredients.iter().take(4).map(|i| i.name.clone()).collect();
    let ingredient_names = if names.is_empty() {
        "食材なし".into()
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
    ingredients
        .iter()
        .map(|i| IngredientModel {
            recipe_id: i.recipe_id.as_str().into(),
            name: i.name.as_str().into(),
            amount: i.amount.as_str().into(),
            unit: i.unit.as_str().into(),
        })
        .collect()
}

fn to_substitute_models(subs: &[Substitute]) -> Vec<SubstituteModel> {
    subs.iter()
        .map(|s| SubstituteModel {
            ingredient: s.ingredient.as_str().into(),
            alternatives: s.alternatives.join(", ").into(),
            note: s.note.as_deref().unwrap_or("").into(),
        })
        .collect()
}

fn to_model_option(info: &ModelInfo) -> ModelOptionModel {
    ModelOptionModel {
        id: info.id.into(),
        name: info.name.into(),
        description: info.description.into(),
        size_text: format!("{:.1} GB", info.size_gb).into(),
        speed: info.speed_label.into(),
        accuracy: info.accuracy_label.into(),
        recommended: info.recommended,
    }
}

// ── UI にレシピ一覧を反映 ─────────────────────────────────────────────────────

fn refresh_recipes(ui: &AppWindow, db: &Arc<Mutex<Database>>) {
    let db = db.lock().unwrap();
    let recipes = db.list_recipes().unwrap_or_default();
    let models: Vec<RecipeModel> = recipes.iter().map(to_recipe_model).collect();
    ui.set_recipes(ModelRc::new(VecModel::from(models)));
}

// ── メイン ────────────────────────────────────────────────────────────────────

pub fn run_app(db_path: &str, data_dir: PathBuf) -> Result<()> {
    let db = Arc::new(Mutex::new(Database::open(db_path)?));
    let scraper = Arc::new(WebScraper::new());
    // LLM は後から設定されるので Mutex<Option<Arc<dyn LlmClient>>> で保持
    let llm: Arc<Mutex<Option<Arc<dyn LlmClient>>>> = Arc::new(Mutex::new(None));
    let data_dir = Arc::new(data_dir);

    let ui = AppWindow::new()?;

    // モデル選択リストを渡す
    let model_options: Vec<ModelOptionModel> = AVAILABLE_MODELS.iter().map(to_model_option).collect();
    ui.set_available_models(ModelRc::new(VecModel::from(model_options)));

    // Ollama 疎通確認 → インストール済みモデル検索
    let ollama_ok = model_manager::is_ollama_running(OLLAMA_BASE_URL);
    ui.set_ollama_running(ollama_ok);

    if ollama_ok {
        if let Some(info) = model_manager::find_installed_model(OLLAMA_BASE_URL) {
            activate_llm(&ui, info, &db, &llm);
        } else {
            ui.set_current_view(AppView::Setup);
        }
    } else {
        ui.set_current_view(AppView::Setup);
    }

    // ── download-model (Ollama pull) ──────────────────────────────────────────
    {
        let db = db.clone();
        let llm = llm.clone();
        let ui_handle = ui.as_weak();

        ui.on_download_model(move |model_id| {
            let id = model_id.to_string();
            let Some(info) = model_manager::model_by_id(&id) else { return };

            let downloaded = Arc::new(AtomicU64::new(0));
            let total = Arc::new(AtomicU64::new(0));

            if let Some(ui) = ui_handle.upgrade() {
                ui.set_current_view(AppView::Downloading);
                ui.set_download_model_name(info.name.into());
                ui.set_download_progress(0.0);
                ui.set_download_downloaded_text("0 MB".into());
                ui.set_download_total_text(format!("{:.1} GB", info.size_gb).into());
            }

            // 進捗ポーリングタイマー
            {
                let downloaded = downloaded.clone();
                let total = total.clone();
                let ui_handle = ui_handle.clone();
                let timer = slint::Timer::default();
                timer.start(
                    slint::TimerMode::Repeated,
                    std::time::Duration::from_millis(400),
                    move || {
                        let dl = downloaded.load(Ordering::Relaxed);
                        let tot = total.load(Ordering::Relaxed);
                        let progress = if tot > 0 { dl as f32 / tot as f32 } else { 0.0 };
                        if let Some(ui) = ui_handle.upgrade() {
                            ui.set_download_progress(progress.min(1.0));
                            ui.set_download_downloaded_text(format_size(dl).into());
                            if tot > 0 {
                                ui.set_download_total_text(format_size(tot).into());
                            }
                        }
                    },
                );
                std::mem::forget(timer);
            }

            // pull スレッド
            let db2 = db.clone();
            let llm2 = llm.clone();
            let ui_handle2 = ui_handle.clone();
            std::thread::spawn(move || {
                let result =
                    model_manager::pull_model(OLLAMA_BASE_URL, info.ollama_model, downloaded, total);

                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle2.upgrade() {
                        match result {
                            Ok(()) => activate_llm(&ui, info, &db2, &llm2),
                            Err(e) => {
                                ui.set_current_view(AppView::Setup);
                                ui.set_status_message(format!("ダウンロード失敗: {}", e).into());
                            }
                        }
                    }
                })
                .ok();
            });
        });
    }

    // ── retry-ollama ──────────────────────────────────────────────────────────
    {
        let db = db.clone();
        let llm = llm.clone();
        let ui_handle = ui.as_weak();
        ui.on_retry_ollama(move || {
            let ok = model_manager::is_ollama_running(OLLAMA_BASE_URL);
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_ollama_running(ok);
                if ok {
                    if let Some(info) = model_manager::find_installed_model(OLLAMA_BASE_URL) {
                        activate_llm(&ui, info, &db, &llm);
                    }
                } else {
                    ui.set_status_message("Ollama がまだ起動していません".into());
                }
            }
        });
    }

    // ── search-changed ────────────────────────────────────────────────────────
    {
        let db = db.clone();
        let ui_handle = ui.as_weak();
        ui.on_search_changed(move |query| {
            let db = db.lock().unwrap();
            let q = query.to_string();
            let mut results = db.search_by_ingredient(&q).unwrap_or_default();
            for r in db.search_by_title(&q).unwrap_or_default() {
                if !results.iter().any(|x| x.id == r.id) {
                    results.push(r);
                }
            }
            let models: Vec<RecipeModel> = results.iter().map(to_recipe_model).collect();
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
                if let Some(ui) = ui_handle.upgrade() {
                    ui.set_selected_recipe(to_recipe_model(&recipe));
                    ui.set_detail_ingredients(ModelRc::new(VecModel::from(
                        to_ingredient_models(&recipe.ingredients),
                    )));
                    ui.set_detail_substitutes(ModelRc::new(VecModel::from(
                        to_substitute_models(&subs),
                    )));
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
                ui.set_stock_ingredients(ModelRc::new(VecModel::from(
                    to_ingredient_models(&ings),
                )));
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
                ui.set_status_message("URL からレシピを取得中...".into());
                ui.set_current_view(AppView::Main);
            }

            std::thread::spawn(move || {
                let result = (|| -> Result<()> {
                    let client = get_llm_or_mock(&llm);
                    let text = scraper.fetch_text(&url_str)?;
                    import_analyzed(text, Some(url_str), &db, &client)
                })();
                let ui_handle = ui_handle.clone();
                let db = db.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        match result {
                            Ok(()) => {
                                refresh_recipes(&ui, &db);
                                ui.set_status_message("レシピを追加しました!".into());
                            }
                            Err(e) => ui.set_status_message(format!("エラー: {}", e).into()),
                        }
                    }
                })
                .ok();
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
                ui.set_status_message("テキストを解析中...".into());
                ui.set_current_view(AppView::Main);
            }

            std::thread::spawn(move || {
                let result = (|| -> Result<()> {
                    let client = get_llm_or_mock(&llm);
                    import_analyzed(text_str, None, &db, &client)
                })();
                let ui_handle = ui_handle.clone();
                let db = db.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        match result {
                            Ok(()) => {
                                refresh_recipes(&ui, &db);
                                ui.set_status_message("レシピを追加しました!".into());
                            }
                            Err(e) => ui.set_status_message(format!("エラー: {}", e).into()),
                        }
                    }
                })
                .ok();
            });
        });
    }

    // ── delete-recipe ─────────────────────────────────────────────────────────
    {
        let db = db.clone();
        let ui_handle = ui.as_weak();
        ui.on_delete_recipe(move |id| {
            let db2 = db.clone();
            if db.lock().unwrap().soft_delete_recipe(&id).is_ok() {
                if let Some(ui) = ui_handle.upgrade() {
                    refresh_recipes(&ui, &db2);
                    ui.set_status_message("レシピを削除しました".into());
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
                    "同期には Google Drive トークンが必要です (STOCLOVE_DRIVE_TOKEN)".into(),
                );
            }
        });
    }

    ui.run()?;
    Ok(())
}

// ── ヘルパー ──────────────────────────────────────────────────────────────────

/// Ollama LLM を有効化して Main ビューへ遷移する。
fn activate_llm(
    ui: &AppWindow,
    info: &'static ModelInfo,
    db: &Arc<Mutex<Database>>,
    llm: &Arc<Mutex<Option<Arc<dyn LlmClient>>>>,
) {
    *llm.lock().unwrap() =
        Some(Arc::new(OllamaLlm::new(OLLAMA_BASE_URL, info.ollama_model)));
    refresh_recipes(ui, db);
    let all_ings = db.lock().unwrap().all_ingredients().unwrap_or_default();
    ui.set_stock_ingredients(ModelRc::new(VecModel::from(to_ingredient_models(&all_ings))));
    ui.set_current_view(AppView::Main);
}

/// LLM が利用可能なら使い、なければ MockLlm にフォールバックする。
fn get_llm_or_mock(llm: &Arc<Mutex<Option<Arc<dyn LlmClient>>>>) -> Arc<dyn LlmClient> {
    llm.lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| Arc::new(MockLlm))
}

/// analyze_recipe を呼び出してレシピを DB に保存する。
fn import_analyzed(
    text: String,
    source_url: Option<String>,
    db: &Arc<Mutex<Database>>,
    llm: &Arc<dyn LlmClient>,
) -> Result<()> {
    match llm.analyze_recipe(&text)? {
        None => anyhow::bail!("レシピとして認識できませんでした。別のテキストをお試しください。"),
        Some(analyzed) => {
            let id = uuid::Uuid::new_v4().to_string();
            let mut recipe = Recipe::new(id.clone(), analyzed.title, source_url, analyzed.instructions);
            recipe.substitutes_json = if analyzed.substitutes.is_empty() {
                None
            } else {
                serde_json::to_string(&analyzed.substitutes).ok()
            };
            recipe.ingredients = analyzed
                .ingredients
                .iter()
                .map(|ri| {
                    crate::domain::ingredient::Ingredient::new(
                        id.clone(),
                        ri.name.clone(),
                        ri.amount.clone(),
                        ri.unit.clone(),
                    )
                })
                .collect();
            db.lock().unwrap().upsert_recipe(&recipe)?;
        }
    }
    Ok(())
}
