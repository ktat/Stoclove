use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use crate::domain::ingredient::Ingredient;
use crate::domain::merge::MergeReport;
use crate::domain::recipe::Recipe;

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path).context("Failed to open SQLite database")?;
        let db = Self { conn };
        db.initialize()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.initialize()?;
        Ok(db)
    }

    fn initialize(&self) -> Result<()> {
        self.conn.execute_batch("
            PRAGMA journal_mode=WAL;
            PRAGMA foreign_keys=ON;

            CREATE TABLE IF NOT EXISTS recipes (
                id              TEXT PRIMARY KEY,
                title           TEXT NOT NULL,
                source_url      TEXT,
                instructions    TEXT NOT NULL,
                substitutes_json TEXT,
                updated_at      INTEGER NOT NULL,
                is_deleted      INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS ingredients (
                recipe_id       TEXT NOT NULL REFERENCES recipes(id) ON DELETE CASCADE,
                name            TEXT NOT NULL,
                amount          TEXT NOT NULL DEFAULT '',
                unit            TEXT NOT NULL DEFAULT '',
                normalized_name TEXT NOT NULL,
                PRIMARY KEY (recipe_id, name)
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS ingredients_fts USING fts5(
                normalized_name,
                recipe_id UNINDEXED,
                content='ingredients',
                content_rowid='rowid'
            );

            CREATE TRIGGER IF NOT EXISTS ingredients_ai
            AFTER INSERT ON ingredients BEGIN
                INSERT INTO ingredients_fts(rowid, normalized_name, recipe_id)
                VALUES (new.rowid, new.normalized_name, new.recipe_id);
            END;

            CREATE TRIGGER IF NOT EXISTS ingredients_ad
            AFTER DELETE ON ingredients BEGIN
                INSERT INTO ingredients_fts(ingredients_fts, rowid, normalized_name, recipe_id)
                VALUES ('delete', old.rowid, old.normalized_name, old.recipe_id);
            END;

            CREATE TRIGGER IF NOT EXISTS ingredients_au
            AFTER UPDATE ON ingredients BEGIN
                INSERT INTO ingredients_fts(ingredients_fts, rowid, normalized_name, recipe_id)
                VALUES ('delete', old.rowid, old.normalized_name, old.recipe_id);
                INSERT INTO ingredients_fts(rowid, normalized_name, recipe_id)
                VALUES (new.rowid, new.normalized_name, new.recipe_id);
            END;

            CREATE TABLE IF NOT EXISTS sync_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
        ")?;
        Ok(())
    }

    // ── Recipes ──────────────────────────────────────────────────────────────

    pub fn upsert_recipe(&self, recipe: &Recipe) -> Result<()> {
        self.conn.execute(
            "INSERT INTO recipes (id, title, source_url, instructions, substitutes_json, updated_at, is_deleted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
               title            = excluded.title,
               source_url       = excluded.source_url,
               instructions     = excluded.instructions,
               substitutes_json = excluded.substitutes_json,
               updated_at       = excluded.updated_at,
               is_deleted       = excluded.is_deleted
             WHERE excluded.updated_at > recipes.updated_at",
            params![
                recipe.id,
                recipe.title,
                recipe.source_url,
                recipe.instructions,
                recipe.substitutes_json,
                recipe.updated_at,
                recipe.is_deleted as i32,
            ],
        )?;

        // Replace all ingredients for this recipe
        self.conn.execute(
            "DELETE FROM ingredients WHERE recipe_id = ?1",
            params![recipe.id],
        )?;
        for ing in &recipe.ingredients {
            self.insert_ingredient(ing)?;
        }
        Ok(())
    }

    fn insert_ingredient(&self, ing: &Ingredient) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO ingredients (recipe_id, name, amount, unit, normalized_name)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![ing.recipe_id, ing.name, ing.amount, ing.unit, ing.normalized_name],
        )?;
        Ok(())
    }

    pub fn get_recipe(&self, id: &str) -> Result<Option<Recipe>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, source_url, instructions, substitutes_json, updated_at, is_deleted
             FROM recipes WHERE id = ?1 AND is_deleted = 0",
        )?;
        let recipe = stmt.query_row(params![id], |row| {
            Ok(Recipe {
                id: row.get(0)?,
                title: row.get(1)?,
                source_url: row.get(2)?,
                instructions: row.get(3)?,
                substitutes_json: row.get(4)?,
                updated_at: row.get(5)?,
                is_deleted: row.get::<_, i32>(6)? != 0,
                ingredients: Vec::new(),
            })
        }).optional()?;

        if let Some(mut r) = recipe {
            r.ingredients = self.get_ingredients(&r.id)?;
            Ok(Some(r))
        } else {
            Ok(None)
        }
    }

    pub fn list_recipes(&self) -> Result<Vec<Recipe>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, source_url, instructions, substitutes_json, updated_at, is_deleted
             FROM recipes WHERE is_deleted = 0
             ORDER BY updated_at DESC",
        )?;
        let mut recipes: Vec<Recipe> = stmt.query_map([], |row| {
            Ok(Recipe {
                id: row.get(0)?,
                title: row.get(1)?,
                source_url: row.get(2)?,
                instructions: row.get(3)?,
                substitutes_json: row.get(4)?,
                updated_at: row.get(5)?,
                is_deleted: row.get::<_, i32>(6)? != 0,
                ingredients: Vec::new(),
            })
        })?.filter_map(|r| r.ok()).collect();

        for recipe in &mut recipes {
            recipe.ingredients = self.get_ingredients(&recipe.id)?;
        }
        Ok(recipes)
    }

    pub fn soft_delete_recipe(&self, id: &str) -> Result<()> {
        let ts = chrono::Utc::now().timestamp();
        self.conn.execute(
            "UPDATE recipes SET is_deleted = 1, updated_at = ?1 WHERE id = ?2",
            params![ts, id],
        )?;
        Ok(())
    }

    // ── Ingredients ──────────────────────────────────────────────────────────

    fn get_ingredients(&self, recipe_id: &str) -> Result<Vec<Ingredient>> {
        let mut stmt = self.conn.prepare(
            "SELECT recipe_id, name, amount, unit, normalized_name
             FROM ingredients WHERE recipe_id = ?1",
        )?;
        let ings = stmt.query_map(params![recipe_id], |row| {
            Ok(Ingredient {
                recipe_id: row.get(0)?,
                name: row.get(1)?,
                amount: row.get(2)?,
                unit: row.get(3)?,
                normalized_name: row.get(4)?,
            })
        })?.filter_map(|r| r.ok()).collect();
        Ok(ings)
    }

    pub fn all_ingredients(&self) -> Result<Vec<Ingredient>> {
        let mut stmt = self.conn.prepare(
            "SELECT i.recipe_id, i.name, i.amount, i.unit, i.normalized_name
             FROM ingredients i
             JOIN recipes r ON r.id = i.recipe_id
             WHERE r.is_deleted = 0
             ORDER BY i.normalized_name",
        )?;
        let ings = stmt.query_map([], |row| {
            Ok(Ingredient {
                recipe_id: row.get(0)?,
                name: row.get(1)?,
                amount: row.get(2)?,
                unit: row.get(3)?,
                normalized_name: row.get(4)?,
            })
        })?.filter_map(|r| r.ok()).collect();
        Ok(ings)
    }

    // ── Full-Text Search ──────────────────────────────────────────────────────

    /// Search recipes by ingredient name using FTS5.
    pub fn search_by_ingredient(&self, query: &str) -> Result<Vec<Recipe>> {
        if query.trim().is_empty() {
            return self.list_recipes();
        }

        // Sanitize query for FTS5
        let fts_query = sanitize_fts_query(query);

        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT r.id, r.title, r.source_url, r.instructions,
                    r.substitutes_json, r.updated_at, r.is_deleted
             FROM recipes r
             JOIN ingredients i ON i.recipe_id = r.id
             WHERE r.is_deleted = 0
               AND i.rowid IN (
                   SELECT rowid FROM ingredients_fts WHERE ingredients_fts MATCH ?1
               )
             ORDER BY r.updated_at DESC",
        )?;

        let mut recipes: Vec<Recipe> = stmt.query_map(params![fts_query], |row| {
            Ok(Recipe {
                id: row.get(0)?,
                title: row.get(1)?,
                source_url: row.get(2)?,
                instructions: row.get(3)?,
                substitutes_json: row.get(4)?,
                updated_at: row.get(5)?,
                is_deleted: row.get::<_, i32>(6)? != 0,
                ingredients: Vec::new(),
            })
        })?.filter_map(|r| r.ok()).collect();

        for recipe in &mut recipes {
            recipe.ingredients = self.get_ingredients(&recipe.id)?;
        }
        Ok(recipes)
    }

    pub fn search_by_title(&self, query: &str) -> Result<Vec<Recipe>> {
        if query.trim().is_empty() {
            return self.list_recipes();
        }
        let pattern = format!("%{}%", query);
        let mut stmt = self.conn.prepare(
            "SELECT id, title, source_url, instructions, substitutes_json, updated_at, is_deleted
             FROM recipes WHERE is_deleted = 0 AND title LIKE ?1
             ORDER BY updated_at DESC",
        )?;
        let mut recipes: Vec<Recipe> = stmt.query_map(params![pattern], |row| {
            Ok(Recipe {
                id: row.get(0)?,
                title: row.get(1)?,
                source_url: row.get(2)?,
                instructions: row.get(3)?,
                substitutes_json: row.get(4)?,
                updated_at: row.get(5)?,
                is_deleted: row.get::<_, i32>(6)? != 0,
                ingredients: Vec::new(),
            })
        })?.filter_map(|r| r.ok()).collect();
        for r in &mut recipes {
            r.ingredients = self.get_ingredients(&r.id)?;
        }
        Ok(recipes)
    }

    // ── Sync Metadata ─────────────────────────────────────────────────────────

    pub fn get_last_sync(&self) -> Result<i64> {
        let ts: Option<i64> = self.conn.query_row(
            "SELECT value FROM sync_meta WHERE key = 'last_sync'",
            [],
            |row| row.get::<_, String>(0),
        ).optional()?.and_then(|s| s.parse().ok());
        Ok(ts.unwrap_or(0))
    }

    pub fn set_last_sync(&self, ts: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_meta (key, value) VALUES ('last_sync', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![ts.to_string()],
        )?;
        Ok(())
    }

    // ── Sync Merge ────────────────────────────────────────────────────────────

    /// Merge recipes from a remote SQLite file using last-write-wins on updated_at.
    pub fn merge_from_file(&self, remote_path: &str) -> Result<MergeReport> {
        self.conn.execute_batch(&format!(
            "ATTACH DATABASE '{}' AS remote;",
            remote_path.replace('\'', "''")
        ))?;

        let result = self.do_merge();

        self.conn.execute_batch("DETACH DATABASE remote;")?;
        result
    }

    fn do_merge(&self) -> Result<MergeReport> {
        let mut report = MergeReport::default();

        // Upsert recipes where remote is newer
        let upserted = self.conn.execute(
            "INSERT INTO main.recipes (id, title, source_url, instructions, substitutes_json, updated_at, is_deleted)
             SELECT id, title, source_url, instructions, substitutes_json, updated_at, is_deleted
             FROM remote.recipes AS r
             WHERE NOT EXISTS (
                 SELECT 1 FROM main.recipes m WHERE m.id = r.id AND m.updated_at >= r.updated_at
             )
             ON CONFLICT(id) DO UPDATE SET
               title            = excluded.title,
               source_url       = excluded.source_url,
               instructions     = excluded.instructions,
               substitutes_json = excluded.substitutes_json,
               updated_at       = excluded.updated_at,
               is_deleted       = excluded.is_deleted",
            [],
        )?;
        report.upserted = upserted;

        // Sync ingredients for upserted recipes
        self.conn.execute_batch(
            "INSERT OR REPLACE INTO main.ingredients (recipe_id, name, amount, unit, normalized_name)
             SELECT ri.recipe_id, ri.name, ri.amount, ri.unit, ri.normalized_name
             FROM remote.ingredients ri
             JOIN main.recipes mr ON mr.id = ri.recipe_id
             WHERE mr.is_deleted = 0;"
        )?;

        let last_sync = chrono::Utc::now().timestamp();
        self.set_last_sync(last_sync)?;
        crate::domain::merge::validate_sync(&report)?;
        Ok(report)
    }

    pub fn export_to_file(&self, path: &str) -> Result<()> {
        use std::fs;
        if std::path::Path::new(path).exists() {
            fs::remove_file(path)?;
        }

        self.conn.execute_batch(&format!(
            "VACUUM INTO '{}';",
            path.replace('\'', "''")
        ))?;
        Ok(())
    }
}

fn sanitize_fts_query(query: &str) -> String {
    // Wrap each token in quotes for safe FTS5 matching
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "")))
        .collect();
    tokens.join(" OR ")
}

trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>>;
}

impl<T> OptionalExt<T> for rusqlite::Result<T> {
    fn optional(self) -> Result<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
