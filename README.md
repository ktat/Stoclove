# Stoclove

ローカルファーストのプライバシー重視レシピ管理アプリ。Rust + Slint で構築し、URL シェアや画像 OCR でレシピをストックし、オンデバイス LLM で解析、Google Drive を経由してデバイス間で同期します。

---

## 特徴

- **ローカルファースト** — SQLite に全データを保持し、オフラインでも完全に動作
- **FTS5 食材検索** — 持っている食材名からレシピを即時検索
- **URL インポート** — レシピ URL を貼るだけで自動スクレイピング → LLM 構造化
- **テキスト貼り付け** — OCR 結果や任意テキストから同様に構造化
- **代替食材サジェスト** — 珍しい食材に対して身近な代替案を AI が提示
- **Google Drive 同期** — SQLite ファイルを Drive に保存し、複数デバイスでマージ
- **ソフトデリート** — 削除フラグで同期時にも削除を正しく伝播

---

## スクリーンショット（UI 構成）

```
┌─────────────────────┐   ┌─────────────────────┐   ┌─────────────────────┐
│       Stoclove  [Sync]│   │ ← Back  チキンカレー │   │ ← Back   My Pantry  │
├─────────────────────┤   ├─────────────────────┤   ├─────────────────────┤
│ [Search ingredients]│   │ Ingredients │ Instr. │   │ タップで絞り込み検索  │
├─────────────────────┤   ├─────────────────────┤   ├─────────────────────┤
│ [Pantry] [+ Recipe] │   │ [2 cup] 小麦粉       │   │ 小麦粉              │
├─────────────────────┤   │ [1 cup] 砂糖         │   │ 砂糖               │
│ チキンカレー         │   │ [3 個]  卵           │   │ 卵                 │
│ 鶏肉, 玉ねぎ... 8 材│   │                     │   │ 鶏肉               │
├─────────────────────┤   │                     │   │ 玉ねぎ              │
│ パスタポモドーロ     │   │                     │   │ ...                │
│ トマト, にんにく 5材 │   │                     │   │                    │
└─────────────────────┘   └─────────────────────┘   └─────────────────────┘
      Main View                 Detail View               Stock View
```

---

## アーキテクチャ

ドメイン駆動設計 (DDD) に基づいた 3 層構成です。

```
src/
├── domain/           # ビジネスロジック (UI・DB に非依存)
│   ├── recipe.rs     # Recipe / RawRecipe / Substitute エンティティ
│   ├── ingredient.rs # Ingredient + normalize_name() (FTS 正規化)
│   ├── merge.rs      # last-write-wins マージポリシー
│   └── error.rs      # ドメインエラー型
│
├── infra/            # 外部システムとの接続
│   ├── db.rs         # SQLite: WAL, FTS5, UPSERT sync (ATTACH/DETACH)
│   ├── ai.rs         # LlmClient / OcrClient トレイト + Mock 実装
│   ├── scraper.rs    # HTTP fetch → HTML テキスト抽出
│   └── sync.rs       # Google Drive REST クライアント
│
└── ui/
    └── app_state.rs  # Slint ↔ Rust ハンドラ、バックグラウンドスレッド

ui/
├── app.slint                       # メインウィンドウ・状態管理
└── components/
    ├── types.slint                 # 共有データ型 (RecipeModel 等)
    ├── theme.slint                 # カラーテーマ定数
    ├── recipe_card.slint           # レシピ一覧カード
    ├── recipe_detail.slint         # 詳細タブ (食材/手順/代替案)
    ├── stock_view.slint            # パントリービュー
    └── add_recipe_dialog.slint     # URL/テキスト追加ダイアログ
```

---

## 技術スタック

| 用途 | ライブラリ |
|---|---|
| 言語 | Rust (stable) |
| UI | [Slint](https://slint.dev/) |
| データベース | SQLite via `rusqlite` (bundled) |
| 全文検索 | SQLite FTS5 拡張 |
| HTTP | `reqwest` (blocking) |
| HTML 解析 | `scraper` |
| Drive 同期 | Google Drive REST API v3 |
| シリアライズ | `serde` / `serde_json` |

---

## データモデル

### recipes テーブル

| カラム | 型 | 説明 |
|---|---|---|
| `id` | TEXT PK | UUID v4 |
| `title` | TEXT | レシピ名 |
| `source_url` | TEXT? | 取得元 URL |
| `instructions` | TEXT | 調理手順 |
| `substitutes_json` | TEXT? | AI 生成代替案 (JSON) |
| `updated_at` | INTEGER | Unix タイムスタンプ (マージキー) |
| `is_deleted` | INTEGER | ソフトデリートフラグ |

### ingredients テーブル + FTS5 仮想テーブル

```sql
CREATE VIRTUAL TABLE ingredients_fts USING fts5(
    normalized_name,
    recipe_id UNINDEXED,
    content='ingredients',
    content_rowid='rowid'
);
```

INSERT/UPDATE/DELETE トリガーで FTS インデックスを自動更新します。

---

## Google Drive 同期の仕組み

起動時に `STOCLOVE_DRIVE_TOKEN` 環境変数があれば自動同期が走ります。

```
起動
 └─ Drive からリモート DB をダウンロード
     └─ ATTACH DATABASE 'remote.sqlite' AS remote
         └─ INSERT OR REPLACE ... WHERE remote.updated_at > local.updated_at
             └─ DETACH → ローカル DB を Drive にアップロード
```

マージ戦略は **last-write-wins** (`updated_at` が新しい方が勝つ)。削除は `is_deleted = 1` のソフトデリートで同期先にも伝播します。

---

## セットアップ・ビルド

### 必要環境

- Rust (stable, 1.80+)
- Linux: `libssl-dev`, `pkg-config`
- macOS / Windows: 追加依存なし (OpenSSL は bundled)

### ビルド

```bash
git clone <repo>
cd Stoclove
cargo build --release
```

### 実行

```bash
# 通常起動 (DB は ~/.local/share/stoclove/stoclove.sqlite)
cargo run --release

# Google Drive 同期を有効化
STOCLOVE_DRIVE_TOKEN=<OAuth2_access_token> cargo run --release

# ログ出力レベル調整
RUST_LOG=debug cargo run
```

---

## AI インターフェースの差し替え

`src/infra/ai.rs` に `LlmClient` / `OcrClient` トレイトを定義しています。現在はヒューリスティックな Mock が入っています。プラットフォーム別の実装に差し替えるには:

```rust
// iOS (CoreML / Vision) → FFI 経由で実装
pub struct CoreMlLlm;
impl LlmClient for CoreMlLlm { ... }

// Android (Gemini Nano / ML Kit) → JNI 経由で実装
pub struct GeminiNanoLlm;
impl LlmClient for GeminiNanoLlm { ... }

// main.rs / app_state.rs の以下の行を差し替えるだけ
let llm: Arc<dyn LlmClient> = Arc::new(CoreMlLlm);
```

---

## テスト

```bash
cargo test
```

現在のテスト:

- `domain::ingredient::tests::test_normalize_name` — 食材名正規化ロジック

---

## ライセンス

MIT
