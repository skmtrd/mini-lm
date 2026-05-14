use chrono::Utc;
use futures_util::StreamExt;
use regex::Regex;
use reqwest::StatusCode;
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::time::{sleep, Duration};
use uuid::Uuid;
use walkdir::WalkDir;

const KEYCHAIN_SERVICE: &str = "mini-lm";
const KEYCHAIN_USER: &str = "deepseek-api-key";
const DEFAULT_MODEL: &str = "deepseek-v4-flash";
const EMBEDDING_MODEL: &str = "mini-lm-ja-ngram-hash-v1";
const EMBEDDING_DIMS: usize = 384;
const DEFAULT_CONTEXT_CHARS: usize = 12_000;
const DEFAULT_BATCH_CHARS: usize = 6_000;
const MAX_REDUCED_FACTS: usize = 120;
const MAX_SHORT_RATE_WAIT_MS: u64 = 15_000;
const ALLOWED_EXTENSIONS: &[&str] = &["txt", "md", "markdown", "csv", "tsv", "json", "log", "text"];

#[derive(Clone)]
struct AppStateInner {
    db_path: PathBuf,
    active_cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
}

impl AppStateInner {
    fn new(db_path: PathBuf) -> Self {
        Self {
            db_path,
            active_cancel: Arc::new(Mutex::new(None)),
        }
    }

    fn conn(&self) -> Result<Connection, String> {
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| e.to_string())?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| e.to_string())?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| e.to_string())?;
        Ok(conn)
    }

    fn begin_task(&self) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        if let Ok(mut slot) = self.active_cancel.lock() {
            *slot = Some(cancel.clone());
        }
        cancel
    }

    fn clear_task(&self, token: &Arc<AtomicBool>) {
        if let Ok(mut slot) = self.active_cancel.lock() {
            if slot.as_ref().is_some_and(|active| Arc::ptr_eq(active, token)) {
                *slot = None;
            }
        }
    }

    fn cancel_task(&self) -> bool {
        if let Ok(slot) = self.active_cancel.lock() {
            if let Some(cancel) = slot.as_ref() {
                cancel.store(true, AtomicOrdering::SeqCst);
                return true;
            }
        }
        false
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    source_path: Option<String>,
    model: String,
    thinking_enabled: bool,
    reasoning_effort: String,
    temperature: f32,
    max_context_chars: usize,
    comprehensive_batch_chars: usize,
    api_key_saved: bool,
    api_key_storage: String,
    #[serde(skip_serializing, skip_deserializing)]
    api_key: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            source_path: None,
            model: DEFAULT_MODEL.to_string(),
            thinking_enabled: false,
            reasoning_effort: "high".to_string(),
            temperature: 0.1,
            max_context_chars: DEFAULT_CONTEXT_CHARS,
            comprehensive_batch_chars: DEFAULT_BATCH_CHARS,
            api_key_saved: false,
            api_key_storage: "none".to_string(),
            api_key: None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsUpdate {
    model: String,
    thinking_enabled: bool,
    reasoning_effort: String,
    temperature: f32,
    max_context_chars: usize,
    comprehensive_batch_chars: usize,
    api_key: Option<String>,
    clear_api_key: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppSnapshot {
    settings: Settings,
    documents: Vec<DocumentInfo>,
    stats: AppStats,
    db_path: String,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DocumentInfo {
    id: i64,
    path: String,
    file_name: String,
    size: i64,
    mtime_ms: i64,
    sha256: String,
    char_count: i64,
    selected: bool,
    indexed_at: Option<String>,
    status: String,
    error: Option<String>,
    chunk_count: i64,
}

#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct AppStats {
    document_count: i64,
    selected_document_count: i64,
    total_chars: i64,
    selected_chars: i64,
    chunk_count: i64,
    embedding_count: i64,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ProgressPayload {
    stage: String,
    status: String,
    message: String,
    run_id: Option<String>,
    completed: u64,
    total: u64,
    elapsed_ms: u128,
    can_cancel: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexSummary {
    scanned_files: usize,
    indexed_files: usize,
    skipped_files: usize,
    failed_files: usize,
    chunk_count: usize,
    elapsed_ms: u128,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchRequest {
    query: String,
    selected_document_ids: Vec<i64>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchResponse {
    hits: Vec<SearchHit>,
    query_terms: Vec<String>,
    scanned_vectors: usize,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SearchHit {
    chunk_id: i64,
    document_id: i64,
    file_name: String,
    path: String,
    heading_path: String,
    char_start: i64,
    char_end: i64,
    snippet: String,
    score: f64,
    fts_score: f64,
    ngram_score: f64,
    vector_score: f64,
    exact_bonus: f64,
    heading_bonus: f64,
    debug: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnswerRequest {
    question: String,
    mode: String,
    selected_document_ids: Vec<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AnswerResponse {
    run_id: String,
    status: String,
    answer: String,
    hits: Vec<SearchHit>,
    facts: Vec<ExtractedFact>,
    audit: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ExtractedFact {
    chunk_id: i64,
    file_name: String,
    path: String,
    heading_path: String,
    char_start: i64,
    char_end: i64,
    statement: String,
    scope: String,
    subject: String,
    object: String,
    condition: String,
    effect: String,
    exception: String,
    polarity: String,
    quote: String,
    confidence: String,
}

#[derive(Debug, Default, Clone)]
struct QuestionProfile {
    terms: Vec<String>,
    aspects: Vec<String>,
    lower_concepts: Vec<String>,
    is_broad: bool,
}

impl QuestionProfile {
    fn from_question(question: &str) -> Self {
        let terms = extract_terms(question);
        Self {
            is_broad: is_broad_question(question, &terms),
            terms,
            aspects: Vec::new(),
            lower_concepts: Vec::new(),
        }
    }

    fn search_terms(&self) -> Vec<String> {
        let mut terms = self.terms.clone();
        terms.extend(self.aspects.clone());
        terms.extend(self.lower_concepts.clone());
        normalize_string_list(&mut terms);
        terms
    }
}

#[derive(Debug, Clone)]
struct ChunkDraft {
    content: String,
    heading_path: String,
    char_start: i64,
    char_end: i64,
}

#[derive(Debug, Clone)]
struct ChunkRecord {
    id: i64,
    document_id: i64,
    file_name: String,
    path: String,
    heading_path: String,
    char_start: i64,
    char_end: i64,
    content: String,
    prev_chunk_id: Option<i64>,
    next_chunk_id: Option<i64>,
}

#[derive(Debug, Clone)]
struct SectionContext {
    document_id: i64,
    file_name: String,
    path: String,
    heading_path: String,
    char_start: i64,
    char_end: i64,
    chunk_ids: Vec<i64>,
    concepts: Vec<String>,
    summary_text: String,
}

#[derive(Debug, Clone)]
struct DocumentContext {
    document_id: i64,
    file_name: String,
    path: String,
    chunk_count: i64,
    char_count: i64,
    concepts: Vec<String>,
    heading_index: Vec<String>,
    summary_text: String,
}

#[derive(Debug, Default, Clone)]
struct HierarchyContext {
    sections: Vec<SectionContext>,
    documents: Vec<DocumentContext>,
    corpus_concepts: Vec<String>,
}

#[derive(Debug, Default, Clone)]
struct ScoreParts {
    fts_rank: Option<usize>,
    ngram_rank: Option<usize>,
    vector_rank: Option<usize>,
    fts_score: f64,
    ngram_score: f64,
    vector_score: f64,
    rrf: f64,
    exact_bonus: f64,
    heading_bonus: f64,
}

#[derive(Debug)]
struct ApiFailure {
    class_name: String,
    message: String,
    retry_after_ms: Option<u64>,
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("app data dir error: {e}"))?;
            fs::create_dir_all(&app_dir)
                .map_err(|e| format!("failed to create app data dir: {e}"))?;
            let state = AppStateInner::new(app_dir.join("mini-lm.sqlite3"));
            init_db(&state)?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_snapshot,
            save_settings,
            set_source_directory,
            index_source_directory,
            set_document_selected,
            select_all_documents,
            clear_document_selection,
            hybrid_search,
            answer_question,
            cancel_current_task
        ])
        .run(tauri::generate_context!())
        .expect("error while running mini-lm");
}

fn init_db(state: &AppStateInner) -> Result<(), String> {
    let conn = state.conn()?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS documents (
            id INTEGER PRIMARY KEY,
            path TEXT NOT NULL UNIQUE,
            file_name TEXT NOT NULL,
            size INTEGER NOT NULL DEFAULT 0,
            mtime_ms INTEGER NOT NULL DEFAULT 0,
            sha256 TEXT NOT NULL DEFAULT '',
            char_count INTEGER NOT NULL DEFAULT 0,
            selected INTEGER NOT NULL DEFAULT 1,
            indexed_at TEXT,
            status TEXT NOT NULL DEFAULT 'pending',
            error TEXT
        );

        CREATE TABLE IF NOT EXISTS chunks (
            id INTEGER PRIMARY KEY,
            document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            char_start INTEGER NOT NULL,
            char_end INTEGER NOT NULL,
            heading_path TEXT NOT NULL,
            content TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            prev_chunk_id INTEGER,
            next_chunk_id INTEGER,
            indexed_at TEXT NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS chunk_fts USING fts5(
            content,
            heading_path,
            file_name
        );

        CREATE TABLE IF NOT EXISTS chunk_ngrams (
            term TEXT NOT NULL,
            chunk_id INTEGER NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
            weight REAL NOT NULL,
            PRIMARY KEY (term, chunk_id)
        );

        CREATE TABLE IF NOT EXISTS chunk_embeddings (
            chunk_id INTEGER PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
            dimensions INTEGER NOT NULL,
            model TEXT NOT NULL,
            vector BLOB NOT NULL,
            indexed_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS section_contexts (
            id INTEGER PRIMARY KEY,
            document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
            heading_path TEXT NOT NULL,
            char_start INTEGER NOT NULL,
            char_end INTEGER NOT NULL,
            chunk_ids_json TEXT NOT NULL,
            concepts_json TEXT NOT NULL,
            summary_text TEXT NOT NULL,
            indexed_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS document_contexts (
            document_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
            chunk_count INTEGER NOT NULL,
            char_count INTEGER NOT NULL,
            concepts_json TEXT NOT NULL,
            heading_index_json TEXT NOT NULL,
            summary_text TEXT NOT NULL,
            indexed_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS questions (
            id INTEGER PRIMARY KEY,
            text TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS retrieval_runs (
            id TEXT PRIMARY KEY,
            question_id INTEGER REFERENCES questions(id),
            mode TEXT NOT NULL,
            status TEXT NOT NULL,
            selected_document_ids TEXT NOT NULL,
            hits_json TEXT,
            started_at TEXT NOT NULL,
            completed_at TEXT,
            error TEXT
        );

        CREATE TABLE IF NOT EXISTS extracted_facts (
            id INTEGER PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES retrieval_runs(id) ON DELETE CASCADE,
            chunk_id INTEGER NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
            fact TEXT NOT NULL,
            quote TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS answers (
            id INTEGER PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES retrieval_runs(id) ON DELETE CASCADE,
            answer TEXT NOT NULL,
            audit_json TEXT,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS logs (
            id INTEGER PRIMARY KEY,
            timestamp TEXT NOT NULL,
            operation TEXT NOT NULL,
            status TEXT NOT NULL,
            message TEXT NOT NULL,
            run_id TEXT,
            metadata_json TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_documents_selected ON documents(selected);
        CREATE INDEX IF NOT EXISTS idx_chunks_document ON chunks(document_id, ordinal);
        CREATE INDEX IF NOT EXISTS idx_section_contexts_document ON section_contexts(document_id, heading_path);
        CREATE INDEX IF NOT EXISTS idx_chunk_ngrams_chunk ON chunk_ngrams(chunk_id);
        CREATE INDEX IF NOT EXISTS idx_extracted_facts_run ON extracted_facts(run_id);
        "#,
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn get_app_snapshot(state: State<'_, AppStateInner>) -> Result<AppSnapshot, String> {
    let conn = state.conn()?;
    let settings = load_settings(&conn)?;
    Ok(AppSnapshot {
        settings,
        documents: load_documents(&conn)?,
        stats: load_stats(&conn)?,
        db_path: state.db_path.to_string_lossy().to_string(),
    })
}

#[tauri::command]
fn save_settings(update: SettingsUpdate, state: State<'_, AppStateInner>) -> Result<Settings, String> {
    let conn = state.conn()?;
    set_setting(&conn, "model", update.model.trim())?;
    set_setting(
        &conn,
        "thinking_enabled",
        if update.thinking_enabled { "true" } else { "false" },
    )?;
    set_setting(&conn, "reasoning_effort", update.reasoning_effort.trim())?;
    set_setting(&conn, "temperature", &update.temperature.clamp(0.0, 1.0).to_string())?;
    set_setting(
        &conn,
        "max_context_chars",
        &update.max_context_chars.clamp(3_000, 80_000).to_string(),
    )?;
    set_setting(
        &conn,
        "comprehensive_batch_chars",
        &update.comprehensive_batch_chars.clamp(2_000, 16_000).to_string(),
    )?;

    if update.clear_api_key {
        delete_api_key(&conn)?;
    } else if let Some(api_key) = update.api_key.as_deref() {
        if !api_key.trim().is_empty() {
            save_api_key(&conn, api_key.trim())?;
        }
    }

    load_settings(&conn)
}

#[tauri::command]
fn set_source_directory(path: String, state: State<'_, AppStateInner>) -> Result<AppSnapshot, String> {
    let source = PathBuf::from(path.trim());
    if !source.is_dir() {
        return Err("指定されたsourceディレクトリが見つかりません。".to_string());
    }
    let conn = state.conn()?;
    set_setting(&conn, "source_path", &source.to_string_lossy())?;
    get_app_snapshot(state)
}

#[tauri::command]
fn set_document_selected(id: i64, selected: bool, state: State<'_, AppStateInner>) -> Result<AppSnapshot, String> {
    let conn = state.conn()?;
    conn.execute(
        "UPDATE documents SET selected = ?1 WHERE id = ?2",
        params![if selected { 1 } else { 0 }, id],
    )
    .map_err(|e| e.to_string())?;
    get_app_snapshot(state)
}

#[tauri::command]
fn select_all_documents(state: State<'_, AppStateInner>) -> Result<AppSnapshot, String> {
    let conn = state.conn()?;
    conn.execute("UPDATE documents SET selected = 1 WHERE status != 'missing'", [])
        .map_err(|e| e.to_string())?;
    get_app_snapshot(state)
}

#[tauri::command]
fn clear_document_selection(state: State<'_, AppStateInner>) -> Result<AppSnapshot, String> {
    let conn = state.conn()?;
    conn.execute("UPDATE documents SET selected = 0", [])
        .map_err(|e| e.to_string())?;
    get_app_snapshot(state)
}

#[tauri::command]
fn cancel_current_task(state: State<'_, AppStateInner>) -> Result<bool, String> {
    Ok(state.cancel_task())
}

#[tauri::command]
fn index_source_directory(app: AppHandle, state: State<'_, AppStateInner>) -> Result<IndexSummary, String> {
    let started = Instant::now();
    let cancel = state.begin_task();
    let result = index_source_directory_inner(&app, &state, &cancel, started);
    state.clear_task(&cancel);
    result
}

fn index_source_directory_inner(
    app: &AppHandle,
    state: &AppStateInner,
    cancel: &Arc<AtomicBool>,
    started: Instant,
) -> Result<IndexSummary, String> {
    let source_path = {
        let conn = state.conn()?;
        get_setting(&conn, "source_path")?
    }
    .ok_or_else(|| "sourceディレクトリが未設定です。".to_string())?;

    let source = PathBuf::from(source_path);
    if !source.is_dir() {
        return Err("保存済みsourceディレクトリが見つかりません。".to_string());
    }

    emit_progress(
        app,
        started,
        "index",
        "running",
        "sourceディレクトリを走査しています",
        None,
        0,
        0,
        true,
    );

    let files = collect_source_files(&source);
    let total = files.len() as u64;
    let found_paths: HashSet<String> = files
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    let mut indexed_files = 0usize;
    let mut skipped_files = 0usize;
    let mut failed_files = 0usize;
    let mut total_chunks = 0usize;
    let mut conn = state.conn()?;

    mark_missing_documents(&conn, &found_paths)?;

    for (idx, path) in files.iter().enumerate() {
        if cancel.load(AtomicOrdering::SeqCst) {
            emit_progress(
                app,
                started,
                "index",
                "cancelled",
                "インデックス作成をキャンセルしました",
                None,
                idx as u64,
                total,
                false,
            );
            return Err("インデックス作成をキャンセルしました。".to_string());
        }

        emit_progress(
            app,
            started,
            "index",
            "running",
            &format!("読み込み中: {}", path.file_name().unwrap_or_default().to_string_lossy()),
            None,
            idx as u64,
            total,
            true,
        );

        match index_one_file(&mut conn, path) {
            Ok(FileIndexOutcome::Indexed(chunks)) => {
                indexed_files += 1;
                total_chunks += chunks;
            }
            Ok(FileIndexOutcome::Skipped) => {
                skipped_files += 1;
            }
            Err(error) => {
                failed_files += 1;
                record_document_error(&conn, path, &error)?;
            }
        }
    }

    emit_progress(
        app,
        started,
        "index",
        "complete",
        "インデックス作成が完了しました",
        None,
        total,
        total,
        false,
    );

    log_event(
        &conn,
        "index",
        "complete",
        "source indexing completed",
        None,
        json!({
            "indexedFiles": indexed_files,
            "skippedFiles": skipped_files,
            "failedFiles": failed_files,
            "chunks": total_chunks
        }),
    )?;

    Ok(IndexSummary {
        scanned_files: files.len(),
        indexed_files,
        skipped_files,
        failed_files,
        chunk_count: total_chunks,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

#[tauri::command]
fn hybrid_search(request: SearchRequest, state: State<'_, AppStateInner>) -> Result<SearchResponse, String> {
    let conn = state.conn()?;
    let response = hybrid_search_internal(
        &conn,
        &request.query,
        &request.selected_document_ids,
        request.limit.unwrap_or(12),
    )?;
    Ok(response)
}

#[tauri::command]
async fn answer_question(
    app: AppHandle,
    request: AnswerRequest,
    state: State<'_, AppStateInner>,
) -> Result<AnswerResponse, String> {
    let started = Instant::now();
    let cancel = state.begin_task();
    let run_id = Uuid::new_v4().to_string();
    let result = answer_question_inner(&app, &state, &request, &run_id, &cancel, started).await;
    state.clear_task(&cancel);
    result
}

async fn answer_question_inner(
    app: &AppHandle,
    state: &AppStateInner,
    request: &AnswerRequest,
    run_id: &str,
    cancel: &Arc<AtomicBool>,
    started: Instant,
) -> Result<AnswerResponse, String> {
    if request.question.trim().is_empty() {
        return Err("質問が空です。".to_string());
    }

    let settings = {
        let conn = state.conn()?;
        let loaded = load_settings(&conn)?;
        let question_id = insert_question(&conn, &request.question)?;
        insert_run(
            &conn,
            run_id,
            question_id,
            &request.mode,
            &request.selected_document_ids,
        )?;
        loaded
    };

    emit_progress(
        app,
        started,
        "retrieve",
        "running",
        "ローカルインデックスを検索しています",
        Some(run_id.to_string()),
        0,
        0,
        true,
    );

    let search = {
        let conn = state.conn()?;
        let search = hybrid_search_internal(&conn, &request.question, &request.selected_document_ids, 16)?;
        let hits_json = serde_json::to_string(&search.hits).unwrap_or_else(|_| "[]".to_string());
        conn.execute(
            "UPDATE retrieval_runs SET hits_json = ?1 WHERE id = ?2",
            params![hits_json, run_id],
        )
        .map_err(|e| e.to_string())?;
        search
    };

    let is_comprehensive = true;
    if is_comprehensive {
        answer_comprehensively(app, state, &settings, request, run_id, cancel, started, search.hits).await
    } else {
        answer_normally(app, state, &settings, request, run_id, cancel, started, search.hits).await
    }
}

async fn answer_normally(
    app: &AppHandle,
    state: &AppStateInner,
    settings: &Settings,
    request: &AnswerRequest,
    run_id: &str,
    cancel: &Arc<AtomicBool>,
    started: Instant,
    hits: Vec<SearchHit>,
) -> Result<AnswerResponse, String> {
    emit_progress(
        app,
        started,
        "answer",
        "running",
        "DeepSeekで回答を生成しています",
        Some(run_id.to_string()),
        0,
        1,
        true,
    );

    let context = build_context_from_hits(&hits, settings.max_context_chars);
    let messages = vec![
        json!({
            "role": "system",
            "content": "あなたはローカル文書専用の回答エンジンです。回答は与えられたSOURCEだけを根拠にしてください。SOURCEにない情報は推測せず、根拠がないと明記してください。主要な主張には [S1] のように出典番号を付けてください。"
        }),
        json!({
            "role": "user",
            "content": format!("質問:\n{}\n\nSOURCE:\n{}\n\n要件:\n- source外の一般知識で補完しない\n- 条件、例外、金額、期間、手続き、対象者を落とさない\n- 根拠不足は根拠不足と書く\n- 日本語で簡潔かつ網羅的に答える", request.question, context)
        }),
    ];

    let answer = call_deepseek(app, settings, messages, 2_000, false, true, Some(run_id), cancel).await?;
    let audit = audit_answer(app, settings, &answer, &hits, run_id, cancel).await.ok();
    let conn = state.conn()?;
    save_answer(&conn, run_id, &answer, audit.as_deref())?;
    complete_run(&conn, run_id, "complete", None)?;

    emit_progress(
        app,
        started,
        "answer",
        "complete",
        "回答が完了しました",
        Some(run_id.to_string()),
        1,
        1,
        false,
    );

    Ok(AnswerResponse {
        run_id: run_id.to_string(),
        status: "complete".to_string(),
        answer,
        hits,
        facts: vec![],
        audit,
    })
}

async fn answer_comprehensively(
    app: &AppHandle,
    state: &AppStateInner,
    settings: &Settings,
    request: &AnswerRequest,
    run_id: &str,
    cancel: &Arc<AtomicBool>,
    started: Instant,
    seed_hits: Vec<SearchHit>,
) -> Result<AnswerResponse, String> {
    emit_progress(
        app,
        started,
        "profile",
        "running",
        "質問の観点と検索語を展開しています",
        Some(run_id.to_string()),
        0,
        0,
        true,
    );

    let mut profile = build_question_profile(app, settings, &request.question, run_id, cancel)
        .await
        .unwrap_or_else(|_| QuestionProfile::from_question(&request.question));
    let chunks = {
        let conn = state.conn()?;
        load_selected_chunks(&conn, &request.selected_document_ids)?
    };
    enrich_profile_with_source_concepts(&mut profile, &chunks, &request.question);
    let seed_hits = {
        let conn = state.conn()?;
        let corrected =
            corrective_retrieval_internal(&conn, &request.question, &request.selected_document_ids, &profile, seed_hits)?;
        let hits_json = serde_json::to_string(&corrected).unwrap_or_else(|_| "[]".to_string());
        conn.execute(
            "UPDATE retrieval_runs SET hits_json = ?1 WHERE id = ?2",
            params![hits_json, run_id],
        )
        .map_err(|e| e.to_string())?;
        corrected
    };
    let hierarchy = {
        let conn = state.conn()?;
        load_hierarchy_context(&conn, &request.selected_document_ids, &profile, &seed_hits)?
    };
    let batches = build_chunk_batches(&chunks, settings.comprehensive_batch_chars);
    let total_batches = batches.len() as u64;
    let mut facts = Vec::new();

    for (idx, batch) in batches.iter().enumerate() {
        if cancel.load(AtomicOrdering::SeqCst) {
            let conn = state.conn()?;
            complete_run(&conn, run_id, "cancelled", Some("ユーザーがキャンセルしました"))?;
            emit_progress(
                app,
                started,
                "extract",
                "cancelled",
                "網羅抽出をキャンセルしました",
                Some(run_id.to_string()),
                idx as u64,
                total_batches,
                false,
            );
            return Err("網羅抽出をキャンセルしました。".to_string());
        }

        emit_progress(
            app,
            started,
            "extract",
            "running",
            &format!("全チャンク確認中: batch {}/{}", idx + 1, total_batches),
            Some(run_id.to_string()),
            idx as u64,
            total_batches,
            true,
        );

        let extracted = extract_facts_from_batch(
            app,
            settings,
            &request.question,
            &profile,
            &hierarchy,
            batch,
            run_id,
            cancel,
        )
        .await?;

        let conn = state.conn()?;
        for fact in extracted {
            insert_fact(&conn, run_id, &fact)?;
            facts.push(fact);
        }
    }

    emit_progress(
        app,
        started,
        "synthesize",
        "running",
        "抽出事実を統合して最終回答を生成しています",
        Some(run_id.to_string()),
        total_batches,
        total_batches,
        true,
    );

    let reduced_facts = reduce_facts(app, settings, &request.question, &profile, &facts, run_id, cancel).await?;
    let mut answer =
        synthesize_answer(app, settings, &request.question, &profile, &hierarchy, &reduced_facts, run_id, cancel).await?;
    let mut audit = audit_facts_answer(app, settings, &request.question, &answer, &reduced_facts, run_id, cancel).await.ok();

    if let Some(audit_text) = audit.clone() {
        if audit_needs_revision(&audit_text) {
            emit_progress(
                app,
                started,
                "audit",
                "running",
                "監査結果に基づいて回答を修正しています",
                Some(run_id.to_string()),
                total_batches,
                total_batches,
                true,
            );
            if let Ok(revised) =
                revise_answer_from_audit(app, settings, &request.question, &answer, &audit_text, &reduced_facts, run_id, cancel)
                    .await
            {
                answer = revised;
                let revised_audit =
                    audit_facts_answer(app, settings, &request.question, &answer, &reduced_facts, run_id, cancel)
                        .await
                        .ok();
                audit = Some(combine_audit_json(&audit_text, revised_audit.as_deref()));
            }
        }
    }

    let conn = state.conn()?;
    save_answer(&conn, run_id, &answer, audit.as_deref())?;
    complete_run(&conn, run_id, "complete", None)?;
    emit_progress(
        app,
        started,
        "answer",
        "complete",
        "高精度網羅回答が完了しました",
        Some(run_id.to_string()),
        total_batches,
        total_batches,
        false,
    );

    Ok(AnswerResponse {
        run_id: run_id.to_string(),
        status: "complete".to_string(),
        answer,
        hits: seed_hits,
        facts: reduced_facts,
        audit,
    })
}

fn collect_source_files(source: &Path) -> Vec<PathBuf> {
    let mut files = WalkDir::new(source)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let path = entry.into_path();
            let ext = path.extension()?.to_string_lossy().to_lowercase();
            ALLOWED_EXTENSIONS.contains(&ext.as_str()).then_some(path)
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

enum FileIndexOutcome {
    Indexed(usize),
    Skipped,
}

fn index_one_file(conn: &mut Connection, path: &Path) -> Result<FileIndexOutcome, String> {
    let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes).to_string();
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default();
    let path_string = path.to_string_lossy().to_string();
    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let char_count = text.chars().count() as i64;
    let existing = conn
        .query_row(
            "SELECT id, sha256, mtime_ms, size FROM documents WHERE path = ?1",
            params![path_string],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;

    if let Some((doc_id, old_hash, old_mtime, old_size)) = existing {
        let chunk_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chunks WHERE document_id = ?1",
                params![doc_id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if old_hash == sha256 && old_mtime == mtime_ms && old_size == metadata.len() as i64 && chunk_count > 0 {
            return Ok(FileIndexOutcome::Skipped);
        }
    }

    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let selected = tx
        .query_row(
            "SELECT selected FROM documents WHERE path = ?1",
            params![path_string],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or(1);

    tx.execute(
        r#"
        INSERT INTO documents(path, file_name, size, mtime_ms, sha256, char_count, selected, indexed_at, status, error)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'indexing', NULL)
        ON CONFLICT(path) DO UPDATE SET
            file_name = excluded.file_name,
            size = excluded.size,
            mtime_ms = excluded.mtime_ms,
            sha256 = excluded.sha256,
            char_count = excluded.char_count,
            selected = excluded.selected,
            indexed_at = excluded.indexed_at,
            status = 'indexing',
            error = NULL
        "#,
        params![
            path_string,
            file_name,
            metadata.len() as i64,
            mtime_ms,
            sha256,
            char_count,
            selected,
            Utc::now().to_rfc3339()
        ],
    )
    .map_err(|e| e.to_string())?;

    let doc_id = tx
        .query_row(
            "SELECT id FROM documents WHERE path = ?1",
            params![path_string],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|e| e.to_string())?;

    let old_chunk_ids = tx
        .prepare("SELECT id FROM chunks WHERE document_id = ?1")
        .map_err(|e| e.to_string())?
        .query_map(params![doc_id], |row| row.get::<_, i64>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    for chunk_id in old_chunk_ids {
        tx.execute("DELETE FROM chunk_fts WHERE rowid = ?1", params![chunk_id])
            .map_err(|e| e.to_string())?;
    }
    tx.execute("DELETE FROM chunks WHERE document_id = ?1", params![doc_id])
        .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM section_contexts WHERE document_id = ?1", params![doc_id])
        .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM document_contexts WHERE document_id = ?1", params![doc_id])
        .map_err(|e| e.to_string())?;

    let chunks = split_text_into_chunks(&text);
    let indexed_at = Utc::now().to_rfc3339();
    let mut inserted_ids = Vec::with_capacity(chunks.len());

    for (ordinal, chunk) in chunks.iter().enumerate() {
        let content_hash = hex::encode(Sha256::digest(chunk.content.as_bytes()));
        tx.execute(
            r#"
            INSERT INTO chunks(document_id, ordinal, char_start, char_end, heading_path, content, content_hash, indexed_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                doc_id,
                ordinal as i64,
                chunk.char_start,
                chunk.char_end,
                chunk.heading_path,
                chunk.content,
                content_hash,
                indexed_at
            ],
        )
        .map_err(|e| e.to_string())?;
        let chunk_id = tx.last_insert_rowid();
        inserted_ids.push(chunk_id);

        tx.execute(
            "INSERT INTO chunk_fts(rowid, content, heading_path, file_name) VALUES (?1, ?2, ?3, ?4)",
            params![chunk_id, chunk.content, chunk.heading_path, file_name],
        )
        .map_err(|e| e.to_string())?;

        let mut terms = term_weights(&chunk.content);
        for (term, weight) in term_weights(&chunk.heading_path) {
            *terms.entry(term).or_insert(0.0) += weight * 2.0;
        }
        insert_ngram_terms(&tx, chunk_id, terms)?;

        let embedding = embedding_for_text(&format!("{}\n{}", chunk.heading_path, chunk.content));
        tx.execute(
            r#"
            INSERT INTO chunk_embeddings(chunk_id, dimensions, model, vector, indexed_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                chunk_id,
                EMBEDDING_DIMS as i64,
                EMBEDDING_MODEL,
                encode_vector(&embedding),
                indexed_at
            ],
        )
        .map_err(|e| e.to_string())?;
    }

    for (idx, chunk_id) in inserted_ids.iter().enumerate() {
        let prev = idx.checked_sub(1).map(|i| inserted_ids[i]);
        let next = inserted_ids.get(idx + 1).copied();
        tx.execute(
            "UPDATE chunks SET prev_chunk_id = ?1, next_chunk_id = ?2 WHERE id = ?3",
            params![prev, next, chunk_id],
        )
        .map_err(|e| e.to_string())?;
    }

    rebuild_hierarchy_contexts(&tx, doc_id, &file_name, &path_string, char_count, &chunks, &inserted_ids, &indexed_at)?;

    tx.execute(
        "UPDATE documents SET status = 'indexed', indexed_at = ?1, error = NULL WHERE id = ?2",
        params![Utc::now().to_rfc3339(), doc_id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;

    Ok(FileIndexOutcome::Indexed(chunks.len()))
}

fn rebuild_hierarchy_contexts(
    conn: &Connection,
    document_id: i64,
    file_name: &str,
    path: &str,
    char_count: i64,
    chunks: &[ChunkDraft],
    chunk_ids: &[i64],
    indexed_at: &str,
) -> Result<(), String> {
    let mut heading_order = Vec::new();
    let mut section_indexes: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, chunk) in chunks.iter().enumerate() {
        let heading = if chunk.heading_path.trim().is_empty() {
            "(見出しなし)".to_string()
        } else {
            chunk.heading_path.clone()
        };
        if !section_indexes.contains_key(&heading) {
            heading_order.push(heading.clone());
        }
        section_indexes.entry(heading).or_default().push(idx);
    }

    for heading in &heading_order {
        let Some(indexes) = section_indexes.get(heading) else {
            continue;
        };
        let section_chunks = indexes
            .iter()
            .filter_map(|idx| chunks.get(*idx).map(|chunk| (*idx, chunk)))
            .collect::<Vec<_>>();
        let section_chunk_ids = section_chunks
            .iter()
            .filter_map(|(idx, _)| chunk_ids.get(*idx).copied())
            .collect::<Vec<_>>();
        let char_start = section_chunks
            .iter()
            .map(|(_, chunk)| chunk.char_start)
            .min()
            .unwrap_or_default();
        let char_end = section_chunks
            .iter()
            .map(|(_, chunk)| chunk.char_end)
            .max()
            .unwrap_or_default();
        let section_texts = section_chunks
            .iter()
            .map(|(_, chunk)| (chunk.heading_path.as_str(), chunk.content.as_str()))
            .collect::<Vec<_>>();
        let concepts = top_concepts_from_texts(&section_texts, 40);
        let sample = section_chunks
            .first()
            .map(|(_, chunk)| first_chars(&chunk.content, 260))
            .unwrap_or_default();
        let summary_text = format!(
            "section={} chars={}-{} chunks={} concepts={} sample={}",
            heading,
            char_start,
            char_end,
            section_chunk_ids.len(),
            concepts.join(", "),
            sample
        );
        conn.execute(
            r#"
            INSERT INTO section_contexts(document_id, heading_path, char_start, char_end, chunk_ids_json, concepts_json, summary_text, indexed_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                document_id,
                heading,
                char_start,
                char_end,
                serde_json::to_string(&section_chunk_ids).unwrap_or_else(|_| "[]".to_string()),
                serde_json::to_string(&concepts).unwrap_or_else(|_| "[]".to_string()),
                summary_text,
                indexed_at
            ],
        )
        .map_err(|e| e.to_string())?;
    }

    let document_texts = chunks
        .iter()
        .map(|chunk| (chunk.heading_path.as_str(), chunk.content.as_str()))
        .collect::<Vec<_>>();
    let concepts = top_concepts_from_texts(&document_texts, 80);
    let mut heading_index = chunks
        .iter()
        .map(|chunk| chunk.heading_path.clone())
        .filter(|heading| !heading.trim().is_empty())
        .collect::<Vec<_>>();
    normalize_string_list(&mut heading_index);
    heading_index.truncate(120);
    let summary_text = format!(
        "document={} chars={} chunks={} concepts={} headings={}",
        file_name,
        char_count,
        chunks.len(),
        concepts.join(", "),
        heading_index.iter().take(30).cloned().collect::<Vec<_>>().join(" / ")
    );
    conn.execute(
        r#"
        INSERT INTO document_contexts(document_id, chunk_count, char_count, concepts_json, heading_index_json, summary_text, indexed_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        "#,
        params![
            document_id,
            chunks.len() as i64,
            char_count,
            serde_json::to_string(&concepts).unwrap_or_else(|_| "[]".to_string()),
            serde_json::to_string(&heading_index).unwrap_or_else(|_| "[]".to_string()),
            summary_text,
            indexed_at
        ],
    )
    .map_err(|e| e.to_string())?;

    let _ = path;
    Ok(())
}

fn top_concepts_from_texts(texts: &[(&str, &str)], limit: usize) -> Vec<String> {
    let mut scores: HashMap<String, f64> = HashMap::new();
    for (heading, text) in texts {
        for concept in concepts_from_heading(heading) {
            if !is_generic_concept(&concept) {
                *scores.entry(concept).or_insert(0.0) += 3.0;
            }
        }
        for (concept, count) in content_concept_counts(text) {
            if !is_generic_concept(&concept) {
                *scores.entry(concept).or_insert(0.0) += count as f64;
            }
        }
    }
    let mut ranked = scores.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut out = ranked
        .into_iter()
        .map(|(concept, _)| concept)
        .take(limit)
        .collect::<Vec<_>>();
    normalize_string_list(&mut out);
    out
}

fn first_chars(text: &str, max_chars: usize) -> String {
    let mut out = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        out.push_str("...");
    }
    out.replace('\n', " ")
}

fn split_text_into_chunks(text: &str) -> Vec<ChunkDraft> {
    let heading_re = Regex::new(r"^\s*(第[0-9０-９一二三四五六七八九十百千]+(章|節|款|目|条).*)\s*$").unwrap();
    let bracket_re = Regex::new(r"^\s*（[^）]{1,60}）\s*$").unwrap();
    let mut sections = Vec::new();
    let mut heading_stack: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_heading = String::new();
    let mut current_start = 0i64;
    let mut char_cursor = 0i64;

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        let is_heading = heading_re.is_match(trimmed) || bracket_re.is_match(trimmed);
        if is_heading && !current.trim().is_empty() {
            let end = current_start + current.chars().count() as i64;
            sections.push(ChunkDraft {
                content: current.trim().to_string(),
                heading_path: current_heading.clone(),
                char_start: current_start,
                char_end: end,
            });
            current.clear();
            current_start = char_cursor;
        } else if current.is_empty() {
            current_start = char_cursor;
        }

        if is_heading {
            update_heading_stack(&mut heading_stack, trimmed);
            current_heading = heading_stack.join(" > ");
        }

        current.push_str(line);
        char_cursor += line.chars().count() as i64;
    }

    if !current.trim().is_empty() {
        let end = current_start + current.chars().count() as i64;
        sections.push(ChunkDraft {
            content: current.trim().to_string(),
            heading_path: current_heading,
            char_start: current_start,
            char_end: end,
        });
    }

    pack_sections(sections)
}

fn update_heading_stack(stack: &mut Vec<String>, heading: &str) {
    let level = if heading.contains('章') {
        0
    } else if heading.contains('節') {
        1
    } else if heading.contains('款') {
        2
    } else if heading.contains('目') {
        3
    } else if heading.contains('条') {
        4
    } else {
        5
    };
    if stack.len() <= level {
        stack.resize(level + 1, String::new());
    }
    stack[level] = heading.trim().to_string();
    stack.truncate(level + 1);
}

fn pack_sections(sections: Vec<ChunkDraft>) -> Vec<ChunkDraft> {
    let target_chars = 1_400usize;
    let max_chars = 2_400usize;
    let mut chunks = Vec::new();
    let mut buffer = String::new();
    let mut heading = String::new();
    let mut start = 0i64;
    let mut end = 0i64;

    for section in sections {
        let section_chars = section.content.chars().count();
        if section_chars > max_chars {
            if !buffer.trim().is_empty() {
                chunks.push(ChunkDraft {
                    content: buffer.trim().to_string(),
                    heading_path: heading.clone(),
                    char_start: start,
                    char_end: end,
                });
                buffer.clear();
            }
            chunks.extend(split_long_section(section, max_chars));
            continue;
        }

        let buffer_chars = buffer.chars().count();
        let heading_changed = buffer_chars > 0
            && !section.heading_path.is_empty()
            && !heading.is_empty()
            && section.heading_path != heading;
        if buffer_chars > 0 && (buffer_chars + section_chars > target_chars || heading_changed) {
            chunks.push(ChunkDraft {
                content: buffer.trim().to_string(),
                heading_path: heading.clone(),
                char_start: start,
                char_end: end,
            });
            buffer.clear();
        }

        if buffer.is_empty() {
            start = section.char_start;
            heading = section.heading_path.clone();
        }
        if !buffer.is_empty() {
            buffer.push_str("\n\n");
        }
        buffer.push_str(&section.content);
        end = section.char_end;
    }

    if !buffer.trim().is_empty() {
        chunks.push(ChunkDraft {
            content: buffer.trim().to_string(),
            heading_path: heading,
            char_start: start,
            char_end: end,
        });
    }

    chunks
}

fn split_long_section(section: ChunkDraft, max_chars: usize) -> Vec<ChunkDraft> {
    let mut chunks = Vec::new();
    let mut buffer = String::new();
    let mut start = section.char_start;
    let mut cursor = section.char_start;

    for paragraph in section.content.split_inclusive('\n') {
        let paragraph_chars = paragraph.chars().count();
        if !buffer.is_empty() && buffer.chars().count() + paragraph_chars > max_chars {
            let end = start + buffer.chars().count() as i64;
            chunks.push(ChunkDraft {
                content: buffer.trim().to_string(),
                heading_path: section.heading_path.clone(),
                char_start: start,
                char_end: end,
            });
            buffer.clear();
            start = cursor;
        }
        buffer.push_str(paragraph);
        cursor += paragraph_chars as i64;
    }

    if !buffer.trim().is_empty() {
        let end = start + buffer.chars().count() as i64;
        chunks.push(ChunkDraft {
            content: buffer.trim().to_string(),
            heading_path: section.heading_path,
            char_start: start,
            char_end: end,
        });
    }
    chunks
}

fn insert_ngram_terms(conn: &Connection, chunk_id: i64, terms: HashMap<String, f64>) -> Result<(), String> {
    let mut stmt = conn
        .prepare(
            r#"
            INSERT INTO chunk_ngrams(term, chunk_id, weight)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(term, chunk_id) DO UPDATE SET weight = excluded.weight
            "#,
        )
        .map_err(|e| e.to_string())?;
    for (term, weight) in terms {
        if !term.trim().is_empty() {
            stmt.execute(params![term, chunk_id, weight.min(12.0)])
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn hybrid_search_internal(
    conn: &Connection,
    query: &str,
    requested_doc_ids: &[i64],
    limit: usize,
) -> Result<SearchResponse, String> {
    let query_terms = extract_terms(query);
    let selected_doc_ids = selected_document_ids(conn, requested_doc_ids)?;
    if selected_doc_ids.is_empty() {
        return Ok(SearchResponse {
            hits: vec![],
            query_terms,
            scanned_vectors: 0,
        });
    }

    let selected_set: HashSet<i64> = selected_doc_ids.iter().copied().collect();
    let fts_ranking = run_fts_search(conn, query, &query_terms, &selected_set)?;
    let ngram_ranking = run_ngram_search(conn, &query_terms, &selected_doc_ids)?;
    let (vector_ranking, scanned_vectors) = run_vector_search(conn, query, &selected_doc_ids)?;

    let mut scores: HashMap<i64, ScoreParts> = HashMap::new();
    apply_ranking(&mut scores, &fts_ranking, "fts", 1.0);
    apply_ranking(&mut scores, &ngram_ranking, "ngram", 1.15);
    apply_ranking(&mut scores, &vector_ranking, "vector", 0.95);

    let mut candidate_ids: Vec<i64> = scores.keys().copied().collect();
    candidate_ids.sort_unstable();
    let records = load_chunk_records(conn, &candidate_ids)?;
    let record_map: HashMap<i64, ChunkRecord> = records.into_iter().map(|r| (r.id, r)).collect();

    for (chunk_id, parts) in scores.iter_mut() {
        if let Some(record) = record_map.get(chunk_id) {
            parts.exact_bonus = exact_bonus(query, &query_terms, &record.content);
            parts.heading_bonus = exact_bonus(query, &query_terms, &record.heading_path) * 1.4;
        }
    }

    let mut ranked: Vec<(i64, f64)> = scores
        .iter()
        .map(|(id, parts)| {
            let score = parts.rrf
                + parts.exact_bonus
                + parts.heading_bonus
                + parts.ngram_score * 0.004
                + parts.vector_score.max(0.0) * 0.08
                + parts.fts_score * 0.03;
            (*id, score)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));

    let mut expanded_ids = Vec::new();
    for (chunk_id, _) in ranked.iter().take(limit * 2) {
        if let Some(record) = record_map.get(chunk_id) {
            if let Some(prev) = record.prev_chunk_id {
                expanded_ids.push(prev);
            }
            expanded_ids.push(*chunk_id);
            if let Some(next) = record.next_chunk_id {
                expanded_ids.push(next);
            }
        }
    }
    expanded_ids.sort_unstable();
    expanded_ids.dedup();

    for neighbor_id in expanded_ids {
        scores.entry(neighbor_id).or_insert_with(|| ScoreParts {
            rrf: 0.003,
            ..ScoreParts::default()
        });
    }

    let final_ids: Vec<i64> = scores.keys().copied().collect();
    let final_records = load_chunk_records(conn, &final_ids)?;
    let final_map: HashMap<i64, ChunkRecord> = final_records.into_iter().map(|r| (r.id, r)).collect();
    let mut hits = Vec::new();

    for (chunk_id, parts) in scores.iter_mut() {
        if let Some(record) = final_map.get(chunk_id) {
            parts.exact_bonus = parts.exact_bonus.max(exact_bonus(query, &query_terms, &record.content));
            parts.heading_bonus = parts
                .heading_bonus
                .max(exact_bonus(query, &query_terms, &record.heading_path) * 1.4);
            let score = parts.rrf
                + parts.exact_bonus
                + parts.heading_bonus
                + parts.ngram_score * 0.004
                + parts.vector_score.max(0.0) * 0.08
                + parts.fts_score * 0.03;
            hits.push(SearchHit {
                chunk_id: *chunk_id,
                document_id: record.document_id,
                file_name: record.file_name.clone(),
                path: record.path.clone(),
                heading_path: record.heading_path.clone(),
                char_start: record.char_start,
                char_end: record.char_end,
                snippet: make_snippet(&record.content, &query_terms, 700),
                score,
                fts_score: parts.fts_score,
                ngram_score: parts.ngram_score,
                vector_score: parts.vector_score,
                exact_bonus: parts.exact_bonus,
                heading_bonus: parts.heading_bonus,
                debug: format!(
                    "fts={:?} ngram={:?} vector={:?} rrf={:.4}",
                    parts.fts_rank, parts.ngram_rank, parts.vector_rank, parts.rrf
                ),
            });
        }
    }

    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
    diversify_hits(&mut hits, limit);
    hits.truncate(limit);

    Ok(SearchResponse {
        hits,
        query_terms,
        scanned_vectors,
    })
}

fn run_fts_search(
    conn: &Connection,
    query: &str,
    query_terms: &[String],
    selected_set: &HashSet<i64>,
) -> Result<Vec<(i64, f64)>, String> {
    let mut terms = vec![query.trim().to_string()];
    terms.extend(query_terms.iter().take(12).cloned());
    terms.retain(|t| t.chars().count() >= 2);
    terms.sort();
    terms.dedup();
    if terms.is_empty() {
        return Ok(vec![]);
    }
    let fts_query = terms
        .iter()
        .take(10)
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ");

    let mut stmt = conn
        .prepare(
            r#"
            SELECT c.id, c.document_id, bm25(chunk_fts) AS rank
            FROM chunk_fts
            JOIN chunks c ON c.id = chunk_fts.rowid
            WHERE chunk_fts MATCH ?1
            ORDER BY rank
            LIMIT 180
            "#,
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map(params![fts_query], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })
        .map_err(|e| e.to_string());

    match rows {
        Ok(mapped) => {
            let mut out = Vec::new();
            for row in mapped {
                let (chunk_id, document_id, rank) = row.map_err(|e| e.to_string())?;
                if selected_set.contains(&document_id) {
                    out.push((chunk_id, 1.0 / (1.0 + rank.abs())));
                }
            }
            Ok(out)
        }
        Err(_) => Ok(vec![]),
    }
}

fn run_ngram_search(
    conn: &Connection,
    query_terms: &[String],
    selected_doc_ids: &[i64],
) -> Result<Vec<(i64, f64)>, String> {
    if query_terms.is_empty() || selected_doc_ids.is_empty() {
        return Ok(vec![]);
    }
    let mut terms = query_terms.iter().take(60).cloned().collect::<Vec<_>>();
    terms.sort();
    terms.dedup();
    let doc_placeholders = placeholders(selected_doc_ids.len());
    let term_placeholders = placeholders(terms.len());
    let sql = format!(
        r#"
        SELECT n.chunk_id, SUM(n.weight) AS score
        FROM chunk_ngrams n
        JOIN chunks c ON c.id = n.chunk_id
        WHERE c.document_id IN ({}) AND n.term IN ({})
        GROUP BY n.chunk_id
        ORDER BY score DESC
        LIMIT 220
        "#,
        doc_placeholders, term_placeholders
    );
    let mut values: Vec<Value> = selected_doc_ids.iter().map(|id| Value::Integer(*id)).collect();
    values.extend(terms.into_iter().map(Value::Text));

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params_from_iter(values), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

fn run_vector_search(
    conn: &Connection,
    query: &str,
    selected_doc_ids: &[i64],
) -> Result<(Vec<(i64, f64)>, usize), String> {
    if selected_doc_ids.is_empty() {
        return Ok((vec![], 0));
    }
    let doc_placeholders = placeholders(selected_doc_ids.len());
    let sql = format!(
        r#"
        SELECT e.chunk_id, e.vector
        FROM chunk_embeddings e
        JOIN chunks c ON c.id = e.chunk_id
        WHERE c.document_id IN ({}) AND e.model = ?{}
        "#,
        doc_placeholders,
        selected_doc_ids.len() + 1
    );
    let mut values: Vec<Value> = selected_doc_ids.iter().map(|id| Value::Integer(*id)).collect();
    values.push(Value::Text(EMBEDDING_MODEL.to_string()));
    let query_vector = embedding_for_text(query);
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params_from_iter(values), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(|e| e.to_string())?;
    let mut scored = Vec::new();
    let mut scanned = 0usize;
    for row in rows {
        let (chunk_id, blob) = row.map_err(|e| e.to_string())?;
        let vector = decode_vector(&blob);
        if vector.len() == EMBEDDING_DIMS {
            scanned += 1;
            scored.push((chunk_id, dot_product(&query_vector, &vector)));
        }
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
    scored.truncate(220);
    Ok((scored, scanned))
}

fn apply_ranking(scores: &mut HashMap<i64, ScoreParts>, ranking: &[(i64, f64)], kind: &str, weight: f64) {
    for (rank, (chunk_id, raw_score)) in ranking.iter().enumerate() {
        let parts = scores.entry(*chunk_id).or_default();
        parts.rrf += weight / (60.0 + rank as f64 + 1.0);
        match kind {
            "fts" => {
                parts.fts_rank = Some(rank + 1);
                parts.fts_score = *raw_score;
            }
            "ngram" => {
                parts.ngram_rank = Some(rank + 1);
                parts.ngram_score = *raw_score;
            }
            "vector" => {
                parts.vector_rank = Some(rank + 1);
                parts.vector_score = *raw_score;
            }
            _ => {}
        }
    }
}

fn selected_document_ids(conn: &Connection, requested_doc_ids: &[i64]) -> Result<Vec<i64>, String> {
    if !requested_doc_ids.is_empty() {
        return Ok(requested_doc_ids.to_vec());
    }
    let mut stmt = conn
        .prepare("SELECT id FROM documents WHERE selected = 1 AND status = 'indexed' ORDER BY file_name")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, i64>(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

fn load_chunk_records(conn: &Connection, chunk_ids: &[i64]) -> Result<Vec<ChunkRecord>, String> {
    if chunk_ids.is_empty() {
        return Ok(vec![]);
    }
    let placeholders = placeholders(chunk_ids.len());
    let sql = format!(
        r#"
        SELECT c.id, c.document_id, d.file_name, d.path, c.heading_path, c.char_start, c.char_end,
               c.content, c.prev_chunk_id, c.next_chunk_id
        FROM chunks c
        JOIN documents d ON d.id = c.document_id
        WHERE c.id IN ({})
        "#,
        placeholders
    );
    let values: Vec<Value> = chunk_ids.iter().map(|id| Value::Integer(*id)).collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params_from_iter(values), |row| {
            Ok(ChunkRecord {
                id: row.get(0)?,
                document_id: row.get(1)?,
                file_name: row.get(2)?,
                path: row.get(3)?,
                heading_path: row.get(4)?,
                char_start: row.get(5)?,
                char_end: row.get(6)?,
                content: row.get(7)?,
                prev_chunk_id: row.get(8)?,
                next_chunk_id: row.get(9)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

fn load_selected_chunks(conn: &Connection, requested_doc_ids: &[i64]) -> Result<Vec<ChunkRecord>, String> {
    let doc_ids = selected_document_ids(conn, requested_doc_ids)?;
    if doc_ids.is_empty() {
        return Ok(vec![]);
    }
    let doc_placeholders = placeholders(doc_ids.len());
    let sql = format!(
        r#"
        SELECT c.id, c.document_id, d.file_name, d.path, c.heading_path, c.char_start, c.char_end,
               c.content, c.prev_chunk_id, c.next_chunk_id
        FROM chunks c
        JOIN documents d ON d.id = c.document_id
        WHERE c.document_id IN ({})
        ORDER BY d.file_name, c.ordinal
        "#,
        doc_placeholders
    );
    let values: Vec<Value> = doc_ids.iter().map(|id| Value::Integer(*id)).collect();
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params_from_iter(values), |row| {
            Ok(ChunkRecord {
                id: row.get(0)?,
                document_id: row.get(1)?,
                file_name: row.get(2)?,
                path: row.get(3)?,
                heading_path: row.get(4)?,
                char_start: row.get(5)?,
                char_end: row.get(6)?,
                content: row.get(7)?,
                prev_chunk_id: row.get(8)?,
                next_chunk_id: row.get(9)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

fn load_hierarchy_context(
    conn: &Connection,
    requested_doc_ids: &[i64],
    profile: &QuestionProfile,
    hits: &[SearchHit],
) -> Result<HierarchyContext, String> {
    let doc_ids = selected_document_ids(conn, requested_doc_ids)?;
    if doc_ids.is_empty() {
        return Ok(HierarchyContext::default());
    }
    let placeholders = placeholders(doc_ids.len());
    let values: Vec<Value> = doc_ids.iter().map(|id| Value::Integer(*id)).collect();

    let document_sql = format!(
        r#"
        SELECT dc.document_id, d.file_name, d.path, dc.chunk_count, dc.char_count,
               dc.concepts_json, dc.heading_index_json, dc.summary_text
        FROM document_contexts dc
        JOIN documents d ON d.id = dc.document_id
        WHERE dc.document_id IN ({})
        ORDER BY d.file_name
        "#,
        placeholders
    );
    let mut stmt = conn.prepare(&document_sql).map_err(|e| e.to_string())?;
    let document_rows = stmt
        .query_map(params_from_iter(values.clone()), |row| {
            Ok(DocumentContext {
                document_id: row.get(0)?,
                file_name: row.get(1)?,
                path: row.get(2)?,
                chunk_count: row.get(3)?,
                char_count: row.get(4)?,
                concepts: parse_json_string_vec(row.get::<_, String>(5)?.as_str()),
                heading_index: parse_json_string_vec(row.get::<_, String>(6)?.as_str()),
                summary_text: row.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let documents = document_rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let section_sql = format!(
        r#"
        SELECT sc.document_id, d.file_name, d.path, sc.heading_path, sc.char_start, sc.char_end,
               sc.chunk_ids_json, sc.concepts_json, sc.summary_text
        FROM section_contexts sc
        JOIN documents d ON d.id = sc.document_id
        WHERE sc.document_id IN ({})
        ORDER BY d.file_name, sc.char_start
        "#,
        placeholders
    );
    let mut stmt = conn.prepare(&section_sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params_from_iter(values), |row| {
            Ok(SectionContext {
                document_id: row.get(0)?,
                file_name: row.get(1)?,
                path: row.get(2)?,
                heading_path: row.get(3)?,
                char_start: row.get(4)?,
                char_end: row.get(5)?,
                chunk_ids: parse_json_i64_vec(row.get::<_, String>(6)?.as_str()),
                concepts: parse_json_string_vec(row.get::<_, String>(7)?.as_str()),
                summary_text: row.get(8)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut sections = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    sections = filter_hierarchy_sections(sections, profile, hits);

    let mut corpus_concepts = Vec::new();
    corpus_concepts.extend(profile.lower_concepts.clone());
    for doc in &documents {
        corpus_concepts.extend(doc.concepts.iter().take(30).cloned());
    }
    for section in &sections {
        corpus_concepts.extend(section.concepts.iter().take(12).cloned());
    }
    normalize_string_list(&mut corpus_concepts);
    corpus_concepts.truncate(120);

    Ok(HierarchyContext {
        sections,
        documents,
        corpus_concepts,
    })
}

fn filter_hierarchy_sections(
    mut sections: Vec<SectionContext>,
    profile: &QuestionProfile,
    hits: &[SearchHit],
) -> Vec<SectionContext> {
    let hit_chunks = hits.iter().map(|hit| hit.chunk_id).collect::<HashSet<_>>();
    let terms = profile.search_terms();
    for section in sections.iter_mut() {
        let mut score = 0.0;
        if section.chunk_ids.iter().any(|id| hit_chunks.contains(id)) {
            score += 8.0;
        }
        if terms.iter().any(|term| section.heading_path.contains(term)) {
            score += 4.0;
        }
        if section
            .concepts
            .iter()
            .any(|concept| terms.iter().any(|term| concept.contains(term) || term.contains(concept)))
        {
            score += 4.0;
        }
        if profile.is_broad {
            score += 1.0;
        }
        section.summary_text = format!("score={:.1} {}", score, section.summary_text);
    }
    sections.sort_by(|a, b| {
        let score_a = context_score(&a.summary_text);
        let score_b = context_score(&b.summary_text);
        score_b
            .partial_cmp(&score_a)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.file_name.cmp(&b.file_name))
            .then_with(|| a.char_start.cmp(&b.char_start))
    });
    sections.truncate(if profile.is_broad { 120 } else { 60 });
    sections
}

fn context_score(summary: &str) -> f64 {
    summary
        .strip_prefix("score=")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or_default()
}

fn parse_json_string_vec(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

fn parse_json_i64_vec(raw: &str) -> Vec<i64> {
    serde_json::from_str::<Vec<i64>>(raw).unwrap_or_default()
}

fn build_batch_hierarchy_context(batch: &[ChunkRecord], hierarchy: &HierarchyContext) -> String {
    if hierarchy.documents.is_empty() && hierarchy.sections.is_empty() {
        return "階層文脈なし".to_string();
    }
    let batch_chunk_ids = batch.iter().map(|chunk| chunk.id).collect::<HashSet<_>>();
    let batch_doc_ids = batch.iter().map(|chunk| chunk.document_id).collect::<HashSet<_>>();
    let mut out = String::new();
    if !hierarchy.corpus_concepts.is_empty() {
        out.push_str(&format!(
            "CORPUS concepts: {}\n",
            hierarchy
                .corpus_concepts
                .iter()
                .take(60)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push_str("DOCUMENT parents:\n");
    for doc in hierarchy
        .documents
        .iter()
        .filter(|doc| batch_doc_ids.contains(&doc.document_id))
        .take(8)
    {
        out.push_str(&format!(
            "- file={} path={} chunks={} chars={} concepts={} summary={}\n",
            doc.file_name,
            doc.path,
            doc.chunk_count,
            doc.char_count,
            doc.concepts.iter().take(20).cloned().collect::<Vec<_>>().join(", "),
            doc.summary_text
        ));
    }
    out.push_str("SECTION parents:\n");
    let mut included = 0usize;
    for section in &hierarchy.sections {
        if !batch_doc_ids.contains(&section.document_id) {
            continue;
        }
        let intersects = section.chunk_ids.iter().any(|id| batch_chunk_ids.contains(id));
        let same_heading = batch
            .iter()
            .any(|chunk| chunk.document_id == section.document_id && chunk.heading_path == section.heading_path);
        if !intersects && !same_heading {
            continue;
        }
        out.push_str(&format!(
            "- file={} path={} heading={} chars={}-{} concepts={} summary={}\n",
            section.file_name,
            section.path,
            section.heading_path,
            section.char_start,
            section.char_end,
            section.concepts.iter().take(20).cloned().collect::<Vec<_>>().join(", "),
            section.summary_text
        ));
        included += 1;
        if included >= 12 {
            break;
        }
    }
    out
}

fn build_answer_hierarchy_context(hierarchy: &HierarchyContext, profile: &QuestionProfile) -> String {
    if hierarchy.documents.is_empty() && hierarchy.sections.is_empty() {
        return "階層文脈なし".to_string();
    }
    let mut out = String::new();
    out.push_str(&format!(
        "CORPUS concepts: {}\n",
        hierarchy
            .corpus_concepts
            .iter()
            .take(80)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push_str("DOCUMENT summaries:\n");
    for doc in hierarchy.documents.iter().take(20) {
        out.push_str(&format!(
            "- file={} path={} chunks={} chars={} concepts={} headings={} summary={}\n",
            doc.file_name,
            doc.path,
            doc.chunk_count,
            doc.char_count,
            doc.concepts.iter().take(20).cloned().collect::<Vec<_>>().join(", "),
            doc.heading_index.iter().take(12).cloned().collect::<Vec<_>>().join(" / "),
            doc.summary_text
        ));
    }
    if profile.is_broad {
        out.push_str("RELEVANT section summaries:\n");
        for section in hierarchy.sections.iter().take(60) {
            out.push_str(&format!(
                "- file={} path={} heading={} concepts={} summary={}\n",
                section.file_name,
                section.path,
                section.heading_path,
                section.concepts.iter().take(16).cloned().collect::<Vec<_>>().join(", "),
                section.summary_text
            ));
        }
    }
    out
}

fn build_chunk_batches(chunks: &[ChunkRecord], max_chars: usize) -> Vec<Vec<ChunkRecord>> {
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut current_chars = 0usize;
    for chunk in chunks {
        let len = chunk.content.chars().count() + chunk.heading_path.chars().count() + 80;
        if !current.is_empty() && current_chars + len > max_chars {
            batches.push(current);
            current = Vec::new();
            current_chars = 0;
        }
        current.push(chunk.clone());
        current_chars += len;
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

fn corrective_retrieval_internal(
    conn: &Connection,
    question: &str,
    selected_document_ids: &[i64],
    profile: &QuestionProfile,
    seed_hits: Vec<SearchHit>,
) -> Result<Vec<SearchHit>, String> {
    let max_seed_score = seed_hits
        .iter()
        .map(|hit| hit.score)
        .fold(0.0_f64, |a, b| a.max(b));
    let should_expand = profile.is_broad || seed_hits.is_empty() || max_seed_score < 0.08;
    if !should_expand {
        return Ok(seed_hits);
    }

    let mut queries = Vec::new();
    queries.push(question.trim().to_string());
    queries.extend(profile.lower_concepts.iter().take(30).cloned());
    queries.extend(profile.aspects.iter().take(12).cloned());
    if profile.terms.len() > 1 {
        queries.push(profile.terms.iter().take(8).cloned().collect::<Vec<_>>().join(" "));
    }
    normalize_string_list(&mut queries);

    let mut merged: HashMap<i64, SearchHit> = seed_hits
        .into_iter()
        .map(|hit| (hit.chunk_id, hit))
        .collect();
    for query in queries.into_iter().take(36) {
        let response = hybrid_search_internal(conn, &query, selected_document_ids, 8)?;
        for mut hit in response.hits {
            hit.debug = format!("{} corrective_query={}", hit.debug, query);
            match merged.get_mut(&hit.chunk_id) {
                Some(existing) => {
                    existing.score = existing.score.max(hit.score) + 0.015;
                    if !existing.debug.contains("corrective_query=") {
                        existing.debug = format!("{} corrective_query={}", existing.debug, query);
                    }
                }
                None => {
                    merged.insert(hit.chunk_id, hit);
                }
            }
        }
    }

    let mut hits = merged.into_values().collect::<Vec<_>>();
    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
    diversify_hits(&mut hits, 32);
    hits.truncate(32);
    Ok(hits)
}

fn enrich_profile_with_source_concepts(profile: &mut QuestionProfile, chunks: &[ChunkRecord], question: &str) {
    let mut concepts = source_derived_concepts(chunks, question, &profile.terms);
    profile.lower_concepts.append(&mut concepts);
    normalize_string_list(&mut profile.lower_concepts);
    if !profile.lower_concepts.is_empty() && is_broad_question(question, &profile.terms) {
        profile.is_broad = true;
    }
}

fn source_derived_concepts(chunks: &[ChunkRecord], question: &str, terms: &[String]) -> Vec<String> {
    let question_terms = meaningful_question_terms(question, terms);
    let mut scores: HashMap<String, f64> = HashMap::new();

    for chunk in chunks {
        for concept in concepts_from_heading(&chunk.heading_path) {
            if is_generic_concept(&concept) {
                continue;
            }
            let mut score = 1.0;
            if concept_matches_question(&concept, &question_terms) {
                score += 8.0;
            }
            if chunk.content.contains(&concept) {
                score += 0.5;
            }
            *scores.entry(concept).or_insert(0.0) += score;
        }
        for (concept, count) in content_concept_counts(&chunk.content) {
            if is_generic_concept(&concept) {
                continue;
            }
            let mut score = (count as f64).min(8.0) * 0.75;
            if concept_matches_question(&concept, &question_terms) {
                score += 8.0;
            }
            if heading_mentions_concept(&chunk.heading_path, &concept) {
                score += 2.0;
            }
            *scores.entry(concept).or_insert(0.0) += score;
        }
    }

    let broad = is_broad_question(question, terms);
    let mut ranked = scores
        .into_iter()
        .filter(|(concept, score)| broad || *score >= 8.0 || concept_matches_question(concept, &question_terms))
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));

    let mut out = ranked
        .into_iter()
        .map(|(concept, _)| concept)
        .take(if broad { 80 } else { 30 })
        .collect::<Vec<_>>();
    normalize_string_list(&mut out);
    out
}

fn content_concept_counts(text: &str) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for segment in split_japanese_segments(text) {
        let chars: Vec<char> = segment.chars().collect();
        if chars.len() < 2 {
            continue;
        }
        for suffix in concept_suffixes() {
            let suffix_chars: Vec<char> = suffix.chars().collect();
            if suffix_chars.is_empty() || chars.len() < suffix_chars.len() {
                continue;
            }
            for start_at in 0..=chars.len() - suffix_chars.len() {
                if chars[start_at..start_at + suffix_chars.len()] != suffix_chars[..] {
                    continue;
                }
                let end = start_at + suffix_chars.len();
                let start = concept_start_index(&chars, start_at);
                if start >= end {
                    continue;
                }
                let concept: String = chars[start..end].iter().collect();
                let concept = normalize_content_concept(&concept);
                if is_valid_source_concept(&concept) {
                    *counts.entry(concept).or_insert(0) += 1;
                }
            }
        }
    }
    counts
}

fn split_japanese_segments(text: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if is_japanese_word_char(ch) {
            current.push(ch);
        } else if !current.is_empty() {
            segments.push(current.clone());
            current.clear();
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

fn is_japanese_word_char(ch: char) -> bool {
    let code = ch as u32;
    (0x3040..=0x30ff).contains(&code)
        || (0x3400..=0x9fff).contains(&code)
        || (0xf900..=0xfaff).contains(&code)
        || ch == '々'
        || ch == 'ー'
}

fn concept_suffixes() -> &'static [&'static str] {
    &[
        "手当",
        "休暇",
        "休業",
        "規程",
        "規則",
        "制度",
        "給与",
        "勤務",
        "旅費",
        "届出",
        "申請",
        "許可",
        "承認",
        "控除",
        "賞与",
        "退職",
        "手続",
        "補助",
        "扶養",
    ]
}

fn concept_start_index(chars: &[char], suffix_start: usize) -> usize {
    let mut start = suffix_start.saturating_sub(10);
    while start < suffix_start && is_concept_connector(chars[start]) {
        start += 1;
    }
    for idx in (start..suffix_start).rev() {
        if is_concept_boundary(chars[idx]) {
            return idx + 1;
        }
    }
    start
}

fn is_concept_connector(ch: char) -> bool {
    matches!(ch, 'の' | 'に' | 'を' | 'は' | 'が' | 'と' | '及' | 'び' | '又')
}

fn is_concept_boundary(ch: char) -> bool {
    matches!(
        ch,
        'の' | 'に' | 'を' | 'は' | 'が' | 'と' | '及' | 'び' | '又' | '者' | '時' | '合'
    )
}

fn normalize_content_concept(raw: &str) -> String {
    let article_re = Regex::new(r"^第[一二三四五六七八九十百千0-9０-９]+(章|節|款|目|条)").unwrap();
    let mut text = article_re.replace(raw.trim(), "").trim().to_string();
    for prefix in ["この", "その", "当該", "各", "別に定める"] {
        text = text.trim_start_matches(prefix).to_string();
    }
    text
}

fn is_valid_source_concept(concept: &str) -> bool {
    let len = concept.chars().count();
    len >= 2
        && len <= 18
        && !is_stop_term(concept)
        && !concept.chars().all(|ch| matches!(ch, '第' | '章' | '節' | '条' | '項'))
}

fn heading_mentions_concept(heading_path: &str, concept: &str) -> bool {
    !concept.trim().is_empty() && heading_path.contains(concept)
}

fn concepts_from_heading(heading_path: &str) -> Vec<String> {
    let mut concepts = Vec::new();
    for raw in heading_path.split(" > ") {
        let concept = normalize_heading_concept(raw);
        if concept.chars().count() >= 2 && concept.chars().count() <= 50 {
            concepts.push(concept);
        }
    }
    normalize_string_list(&mut concepts);
    concepts
}

fn normalize_heading_concept(raw: &str) -> String {
    let mut text = raw.trim().to_string();
    if text.starts_with('（') && text.ends_with('）') && text.chars().count() <= 60 {
        text = text
            .trim_start_matches('（')
            .trim_end_matches('）')
            .trim()
            .to_string();
    }
    let article_re = Regex::new(r"^第[0-9０-９一二三四五六七八九十百千]+(章|節|款|目|条)\s*").unwrap();
    text = article_re.replace(&text, "").trim().to_string();
    text.trim_matches(|c: char| matches!(c, '「' | '」' | '"' | '\'' | ' ' | '\t'))
        .to_string()
}

fn meaningful_question_terms(question: &str, terms: &[String]) -> Vec<String> {
    let mut out = terms.to_vec();
    out.extend(extract_terms(question));
    out.retain(|term| term.chars().count() >= 2 && !is_stop_term(term));
    normalize_string_list(&mut out);
    out
}

fn concept_matches_question(concept: &str, terms: &[String]) -> bool {
    terms.iter().any(|term| {
        term.chars().count() >= 2 && (concept.contains(term) || term.contains(concept))
    })
}

fn is_broad_question(question: &str, terms: &[String]) -> bool {
    let q = question.trim();
    if q.chars().count() <= 12 && terms.len() <= 8 {
        return true;
    }
    [
        "どうなっていますか",
        "どうなってますか",
        "全体",
        "一覧",
        "まとめ",
        "網羅",
        "すべて",
        "全部",
        "全て",
        "教えて",
        "について",
    ]
    .iter()
    .any(|marker| q.contains(marker))
}

fn is_stop_term(term: &str) -> bool {
    matches!(
        term,
        "どう"
            | "なっ"
            | "なって"
            | "います"
            | "ます"
            | "です"
            | "ください"
            | "教えて"
            | "について"
            | "もの"
            | "こと"
            | "場合"
            | "情報"
            | "質問"
            | "回答"
            | "一覧"
            | "まとめ"
            | "全体"
            | "全部"
            | "全て"
            | "すべて"
    )
}

fn is_generic_concept(concept: &str) -> bool {
    matches!(
        concept,
        "総則"
            | "目的"
            | "定義"
            | "適用範囲"
            | "雑則"
            | "附則"
            | "施行"
            | "改正"
            | "経過措置"
    )
}

fn normalize_string_list(items: &mut Vec<String>) {
    for item in items.iter_mut() {
        *item = item.trim().to_string();
    }
    items.retain(|item| item.chars().count() >= 2 && item.chars().count() <= 80);
    items.sort();
    items.dedup();
}

async fn build_question_profile(
    app: &AppHandle,
    settings: &Settings,
    question: &str,
    run_id: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<QuestionProfile, String> {
    let messages = vec![
        json!({
            "role": "system",
            "content": "質問から、ローカル文書検索に使う日本語検索語、同義語、表記揺れ、回答観点をJSONだけで返してください。source外の事実を作らないでください。質問が「どうなっていますか」「一覧」「全体」「まとめ」のように広い場合は is_broad を true にしてください。形式: {\"terms\":[\"...\"],\"aspects\":[\"...\"],\"is_broad\":true|false}"
        }),
        json!({"role": "user", "content": question}),
    ];
    let content = call_deepseek(app, settings, messages, 800, true, false, Some(run_id), cancel).await?;
    let mut terms = extract_terms(question);
    let mut aspects = Vec::new();
    let mut is_broad = is_broad_question(question, &terms);
    if let Ok(parsed) = serde_json::from_str::<JsonValue>(&content) {
        if let Some(items) = parsed.get("terms").and_then(|v| v.as_array()) {
            for item in items {
                if let Some(text) = item.as_str() {
                    terms.extend(extract_terms(text));
                    if text.chars().count() >= 2 && text.chars().count() <= 40 {
                        terms.push(text.to_string());
                    }
                }
            }
        }
        if let Some(items) = parsed.get("aspects").and_then(|v| v.as_array()) {
            for item in items {
                if let Some(text) = item.as_str() {
                    aspects.extend(extract_terms(text));
                    if text.chars().count() >= 2 && text.chars().count() <= 50 {
                        aspects.push(text.to_string());
                    }
                }
            }
        }
        if let Some(value) = parsed.get("is_broad").and_then(|v| v.as_bool()) {
            is_broad |= value;
        }
    }
    normalize_string_list(&mut terms);
    normalize_string_list(&mut aspects);
    Ok(QuestionProfile {
        terms,
        aspects,
        lower_concepts: Vec::new(),
        is_broad,
    })
}

async fn extract_facts_from_batch(
    app: &AppHandle,
    settings: &Settings,
    question: &str,
    profile: &QuestionProfile,
    hierarchy: &HierarchyContext,
    batch: &[ChunkRecord],
    run_id: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<Vec<ExtractedFact>, String> {
    let mut source = String::new();
    for chunk in batch {
        source.push_str(&format!(
            "\n[C{}] file={} chars={}-{} heading={}\n{}\n",
            chunk.id, chunk.file_name, chunk.char_start, chunk.char_end, chunk.heading_path, chunk.content
        ));
    }
    let profile_terms = profile.search_terms();
    let lower_concepts = if profile.lower_concepts.is_empty() {
        "なし".to_string()
    } else {
        profile.lower_concepts.iter().take(80).cloned().collect::<Vec<_>>().join(", ")
    };
    let hierarchy_context = build_batch_hierarchy_context(batch, hierarchy);
    let messages = vec![
        json!({
            "role": "system",
            "content": "あなたは文書監査用の抽出器です。質問へ直接答えるために必要な事実だけをSOURCEから抽出してください。SOURCEにない推測は禁止です。特に scope, subject, object, condition, effect の関係を崩さず抽出してください。条件、例外、対象、手続き、金額、期間、判断基準を優先してください。広い質問の場合は、提示されたsource由来の下位概念に関する事実も漏らさず抽出してください。JSONだけで返してください。形式: {\"facts\":[{\"chunk_id\":123,\"statement\":\"...\",\"scope\":\"文書名/章/条/制度など\",\"subject\":\"誰・何についてか\",\"object\":\"対象制度・手当・行為など\",\"condition\":\"成立条件・対象条件・除外条件\",\"effect\":\"支給/控除/必要/禁止などの効果\",\"exception\":\"例外。なければ空文字\",\"polarity\":\"positive|negative|conditional\",\"quote\":\"SOURCE中の短い根拠引用\",\"confidence\":\"high|medium|low\"}]}"
        }),
        json!({
            "role": "user",
            "content": format!(
                "質問:\n{}\n\n広い質問か:\n{}\n\n検索観点:\n{}\n\nsource由来の下位概念候補:\n{}\n\n抽出ルール:\n- source外の一般知識は使わない\n- condition がある effect は、必ず condition と結び付けて抽出する\n- subject/object/scope を広げない\n- 否定、対象外、ただし書きは polarity または exception に残す\n- PARENT_CONTEXT はSOURCEチャンクの親セクション・文書・全体文脈です。条件や例外を切り落とさないために使ってよいが、quote はSOURCE内から取る\n- 質問範囲外の不足情報は抽出しない\n\nPARENT_CONTEXT:\n{}\n\nSOURCE:\n{}",
                question,
                profile.is_broad,
                profile_terms.join(", "),
                lower_concepts,
                hierarchy_context,
                source
            )
        }),
    ];
    let content = call_deepseek(app, settings, messages, 1_400, true, false, Some(run_id), cancel).await?;
    let parsed = serde_json::from_str::<JsonValue>(&content).unwrap_or_else(|_| json!({"facts":[]}));
    let mut out = Vec::new();
    if let Some(items) = parsed.get("facts").and_then(|v| v.as_array()) {
        for item in items {
            let chunk_id = item.get("chunk_id").and_then(|v| v.as_i64()).unwrap_or_default();
            let statement = item
                .get("statement")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let quote = item
                .get("quote")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let scope = fact_field(item, "scope");
            let subject = fact_field(item, "subject");
            let object = fact_field(item, "object");
            let condition = fact_field(item, "condition");
            let effect = fact_field(item, "effect");
            let exception = fact_field(item, "exception");
            let polarity = normalize_polarity(&fact_field(item, "polarity"), &condition, &effect);
            let confidence = normalize_confidence(&fact_field(item, "confidence"));
            let statement = if statement.is_empty() {
                compose_fact_statement(&subject, &object, &condition, &effect, &exception)
            } else {
                statement
            };
            if statement.is_empty() {
                continue;
            }
            if let Some(chunk) = batch.iter().find(|c| c.id == chunk_id) {
                out.push(ExtractedFact {
                    chunk_id,
                    file_name: chunk.file_name.clone(),
                    path: chunk.path.clone(),
                    heading_path: chunk.heading_path.clone(),
                    char_start: chunk.char_start,
                    char_end: chunk.char_end,
                    statement,
                    scope: if scope.is_empty() {
                        fallback_scope(chunk)
                    } else {
                        scope
                    },
                    subject,
                    object,
                    condition,
                    effect,
                    exception,
                    polarity,
                    quote,
                    confidence,
                });
            }
        }
    }
    Ok(out)
}

fn fact_field(item: &JsonValue, key: &str) -> String {
    item.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

fn normalize_polarity(raw: &str, condition: &str, effect: &str) -> String {
    let value = raw.trim().to_ascii_lowercase();
    if matches!(value.as_str(), "positive" | "negative" | "conditional") {
        return value;
    }
    if !condition.trim().is_empty() {
        return "conditional".to_string();
    }
    if effect.contains("ない") || effect.contains("対象外") || effect.contains("除く") || effect.contains("禁止") {
        return "negative".to_string();
    }
    "positive".to_string()
}

fn normalize_confidence(raw: &str) -> String {
    let value = raw.trim().to_ascii_lowercase();
    if matches!(value.as_str(), "high" | "medium" | "low") {
        value
    } else {
        "medium".to_string()
    }
}

fn compose_fact_statement(subject: &str, object: &str, condition: &str, effect: &str, exception: &str) -> String {
    let mut parts = Vec::new();
    if !subject.trim().is_empty() {
        parts.push(format!("subject={}", subject.trim()));
    }
    if !object.trim().is_empty() {
        parts.push(format!("object={}", object.trim()));
    }
    if !condition.trim().is_empty() {
        parts.push(format!("condition={}", condition.trim()));
    }
    if !effect.trim().is_empty() {
        parts.push(format!("effect={}", effect.trim()));
    }
    if !exception.trim().is_empty() {
        parts.push(format!("exception={}", exception.trim()));
    }
    parts.join(" / ")
}

fn fallback_scope(chunk: &ChunkRecord) -> String {
    if chunk.heading_path.trim().is_empty() {
        chunk.file_name.clone()
    } else {
        format!("{} / {}", chunk.file_name, chunk.heading_path)
    }
}

async fn reduce_facts(
    app: &AppHandle,
    settings: &Settings,
    question: &str,
    profile: &QuestionProfile,
    facts: &[ExtractedFact],
    run_id: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<Vec<ExtractedFact>, String> {
    if facts.len() <= MAX_REDUCED_FACTS {
        return Ok(facts.to_vec());
    }
    let mut source = String::new();
    for (idx, fact) in facts.iter().enumerate() {
        source.push_str(&format!(
            "[F{}] chunk={} file={} heading={} scope={} subject={} object={} condition={} effect={} exception={} polarity={} confidence={} statement={} quote={}\n",
            idx + 1,
            fact.chunk_id,
            fact.file_name,
            fact.heading_path,
            fact.scope,
            fact.subject,
            fact.object,
            fact.condition,
            fact.effect,
            fact.exception,
            fact.polarity,
            fact.confidence,
            fact.statement,
            fact.quote
        ));
    }
    let lower_concepts = if profile.lower_concepts.is_empty() {
        "なし".to_string()
    } else {
        profile.lower_concepts.iter().take(80).cloned().collect::<Vec<_>>().join(", ")
    };
    let messages = vec![
        json!({
            "role": "system",
            "content": "重複した抽出事実を統合し、質問へ直接答えるために必要な事実を残してください。source由来の下位概念がある場合は、質問に関係する各下位概念の根拠を可能な限り残してください。結論、条件、例外、対象、手続き、金額、期間、判断基準に関わる事実を優先してください。subject/object/condition/effect/scope の関係が違う事実を混ぜないでください。JSONだけで返してください。形式: {\"keep_indexes\":[1,2,3]}"
        }),
        json!({"role":"user","content":format!("質問:\n{}\n\n広い質問か:\n{}\n\nsource由来の下位概念候補:\n{}\n\n残す最大件数:\n{}\n\nFACTS:\n{}", question, profile.is_broad, lower_concepts, MAX_REDUCED_FACTS, source)}),
    ];
    let content = call_deepseek(app, settings, messages, 1_200, true, false, Some(run_id), cancel).await?;
    let parsed = serde_json::from_str::<JsonValue>(&content).unwrap_or_else(|_| json!({}));
    let mut reduced = Vec::new();
    if let Some(indexes) = parsed.get("keep_indexes").and_then(|v| v.as_array()) {
        for index in indexes {
            if let Some(i) = index.as_u64() {
                if let Some(fact) = facts.get(i.saturating_sub(1) as usize) {
                    reduced.push(fact.clone());
                }
            }
        }
    }
    if reduced.is_empty() {
        Ok(facts.iter().take(MAX_REDUCED_FACTS).cloned().collect())
    } else {
        reduced.truncate(MAX_REDUCED_FACTS);
        Ok(reduced)
    }
}

async fn synthesize_answer(
    app: &AppHandle,
    settings: &Settings,
    question: &str,
    profile: &QuestionProfile,
    hierarchy: &HierarchyContext,
    facts: &[ExtractedFact],
    run_id: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<String, String> {
    if facts.is_empty() {
        return Ok("根拠が見つかりません。選択中のsource内に、この質問へ回答できる事実は抽出されませんでした。".to_string());
    }
    let mut source = String::new();
    for (idx, fact) in facts.iter().enumerate() {
        source.push_str(&format_fact_for_prompt(idx + 1, fact));
    }
    let lower_concepts = if profile.lower_concepts.is_empty() {
        "なし".to_string()
    } else {
        profile.lower_concepts.iter().take(80).cloned().collect::<Vec<_>>().join(", ")
    };
    let hierarchy_context = build_answer_hierarchy_context(hierarchy, profile);
    let messages = vec![
        json!({
            "role": "system",
            "content": "あなたはローカル文書専用の回答エンジンです。FACTSだけを根拠に回答してください。最優先は、質問に直接答えることです。情報の列挙だけで終わらせず、まず結論を明示し、その後に条件、例外、根拠を整理してください。各主要主張に [F1] のような出典を付けてください。FACTSにない情報は根拠不足としてください。Markdownで返してください。"
        }),
        json!({
            "role": "user",
            "content": format!(
                "質問:\n{}\n\n広い質問か:\n{}\n\nsource由来の下位概念候補:\n{}\n\nHIERARCHY_CONTEXT:\n{}\n\nFACTS:\n{}\n\n回答要件:\n- 冒頭で質問への直接回答を書く\n- 「はい/いいえ」「対象/対象外」「できる/できない」「こう扱う」など判断できる質問では、まず判断を示す\n- 判断に条件がある場合は、条件付きの結論として書く\n- 広い質問では、source由来の下位概念候補とFACTSに基づいて、関係する下位概念ごとに整理する\n- HIERARCHY_CONTEXTは網羅性確認と章・文書範囲確認に使う。ただし、主要主張の根拠はFACTSに限定する\n- その後に理由、条件、例外、対象者、金額、期間、手続きを整理する\n- 文書間差分や矛盾があれば明示する\n- FACTSにない推測は禁止\n- subject を別の主体に置き換えない\n- object を別の制度・手当・行為に置き換えない\n- condition のない effect として断定しない\n- effect を反転させない\n- scope を広げない\n- 根拠不足は、質問へ直接答えるために必要な点だけに絞る\n- 日本語で、Markdownとして読みやすく回答する",
                question,
                profile.is_broad,
                lower_concepts,
                hierarchy_context,
                source
            )
        }),
    ];
    call_deepseek(app, settings, messages, 3_500, false, true, Some(run_id), cancel).await
}

async fn audit_answer(
    app: &AppHandle,
    settings: &Settings,
    answer: &str,
    hits: &[SearchHit],
    run_id: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<String, String> {
    let source = build_context_from_hits(hits, settings.max_context_chars);
    let messages = vec![
        json!({"role":"system","content":"回答の各主張がSOURCEに支えられているか検査し、JSONだけで返してください。形式: {\"unsupported_claims\":[\"...\"],\"verdict\":\"pass|warning|fail\"}"}),
        json!({"role":"user","content":format!("ANSWER:\n{}\n\nSOURCE:\n{}", answer, source)}),
    ];
    call_deepseek(app, settings, messages, 1_000, true, false, Some(run_id), cancel).await
}

async fn audit_facts_answer(
    app: &AppHandle,
    settings: &Settings,
    question: &str,
    answer: &str,
    facts: &[ExtractedFact],
    run_id: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<String, String> {
    let mut source = String::new();
    for (idx, fact) in facts.iter().enumerate() {
        source.push_str(&format_fact_for_prompt(idx + 1, fact));
    }
    let messages = vec![
        json!({"role":"system","content":"回答の各主張がFACTSに支えられているか検査し、JSONだけで返してください。unsupported_claims はFACTSにない主張、relationship_errors は subject/object/condition/effect の関係ミス、scope_errors はFACTSより広い範囲への一般化、polarity_errors は肯定/否定/条件付きの反転や言い過ぎ、insufficient_evidence_overreach は質問範囲外の根拠不足列挙です。形式: {\"unsupported_claims\":[\"...\"],\"relationship_errors\":[{\"claim\":\"...\",\"problem\":\"...\",\"supported_fact_ids\":[\"F1\"]}],\"scope_errors\":[{\"claim\":\"...\",\"problem\":\"...\",\"supported_fact_ids\":[\"F2\"]}],\"polarity_errors\":[{\"claim\":\"...\",\"problem\":\"...\",\"supported_fact_ids\":[\"F3\"]}],\"insufficient_evidence_overreach\":[\"...\"],\"verdict\":\"pass|warning|fail\"}"}),
        json!({"role":"user","content":format!("QUESTION:\n{}\n\nANSWER:\n{}\n\nFACTS:\n{}", question, answer, source)}),
    ];
    call_deepseek(app, settings, messages, 1_500, true, false, Some(run_id), cancel).await
}

async fn revise_answer_from_audit(
    app: &AppHandle,
    settings: &Settings,
    question: &str,
    answer: &str,
    audit: &str,
    facts: &[ExtractedFact],
    run_id: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<String, String> {
    let mut source = String::new();
    for (idx, fact) in facts.iter().enumerate() {
        source.push_str(&format_fact_for_prompt(idx + 1, fact));
    }
    let messages = vec![
        json!({
            "role": "system",
            "content": "あなたはローカル文書専用回答の修正者です。AUDITで指摘された unsupported/relationship/scope/polarity/overreach を修正してください。FACTSだけを根拠にし、根拠のない主張は削除するか根拠不足として質問範囲内に限定して書いてください。subject/object/condition/effect/scope を改変しないでください。Markdownで回答だけを返してください。"
        }),
        json!({
            "role": "user",
            "content": format!("QUESTION:\n{}\n\nCURRENT_ANSWER:\n{}\n\nAUDIT:\n{}\n\nFACTS:\n{}", question, answer, audit, source)
        }),
    ];
    call_deepseek(app, settings, messages, 3_000, false, true, Some(run_id), cancel).await
}

fn format_fact_for_prompt(index: usize, fact: &ExtractedFact) -> String {
    format!(
        "[F{}] file={} chunk={} chars={}-{} heading={}\nscope: {}\nsubject: {}\nobject: {}\ncondition: {}\neffect: {}\nexception: {}\npolarity: {}\nconfidence: {}\nstatement: {}\nquote: {}\n\n",
        index,
        fact.file_name,
        fact.chunk_id,
        fact.char_start,
        fact.char_end,
        fact.heading_path,
        fact.scope,
        fact.subject,
        fact.object,
        fact.condition,
        fact.effect,
        fact.exception,
        fact.polarity,
        fact.confidence,
        fact.statement,
        fact.quote
    )
}

fn audit_needs_revision(audit: &str) -> bool {
    let parsed = match serde_json::from_str::<JsonValue>(audit) {
        Ok(value) => value,
        Err(_) => return false,
    };
    let verdict = parsed
        .get("verdict")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if verdict == "warning" || verdict == "fail" {
        return true;
    }
    [
        "unsupported_claims",
        "relationship_errors",
        "scope_errors",
        "polarity_errors",
        "insufficient_evidence_overreach",
    ]
    .iter()
    .any(|key| {
        parsed
            .get(key)
            .and_then(|v| v.as_array())
            .is_some_and(|items| !items.is_empty())
    })
}

fn combine_audit_json(initial: &str, final_audit: Option<&str>) -> String {
    let initial_value = serde_json::from_str::<JsonValue>(initial).unwrap_or_else(|_| json!({"raw": initial}));
    let final_value = final_audit
        .map(|text| serde_json::from_str::<JsonValue>(text).unwrap_or_else(|_| json!({"raw": text})))
        .unwrap_or_else(|| json!({"error": "re-audit failed"}));
    json!({
        "revision_applied": true,
        "initial": initial_value,
        "final": final_value
    })
    .to_string()
}

async fn call_deepseek(
    app: &AppHandle,
    settings: &Settings,
    messages: Vec<JsonValue>,
    max_tokens: u32,
    json_mode: bool,
    stream: bool,
    run_id: Option<&str>,
    cancel: &Arc<AtomicBool>,
) -> Result<String, String> {
    let api_key = settings
        .api_key
        .as_deref()
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| "DeepSeek API key が未設定です。設定画面で保存してください。".to_string())?;
    let client = reqwest::Client::new();
    let body = build_deepseek_body(settings, messages, max_tokens, json_mode, stream);

    let mut attempt = 0usize;
    loop {
        if cancel.load(AtomicOrdering::SeqCst) {
            return Err("処理をキャンセルしました。".to_string());
        }
        attempt += 1;
        emit_api_status(app, run_id, "DeepSeekへ送信しています", attempt, true);
        let response = client
            .post("https://api.deepseek.com/chat/completions")
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("DeepSeek通信に失敗しました: {e}"))?;

        if response.status().is_success() {
            if stream {
                return read_deepseek_stream(app, response, run_id, cancel).await;
            }
            let value = response
                .json::<JsonValue>()
                .await
                .map_err(|e| format!("DeepSeek応答の解析に失敗しました: {e}"))?;
            return Ok(value
                .get("choices")
                .and_then(|v| v.as_array())
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("message"))
                .and_then(|message| message.get("content"))
                .and_then(|content| content.as_str())
                .unwrap_or("")
                .to_string());
        }

        let failure = classify_api_failure(response).await;
        if failure.class_name == "rate_limit" && attempt <= 2 {
            if let Some(wait_ms) = failure.retry_after_ms {
                if wait_ms <= MAX_SHORT_RATE_WAIT_MS {
                    emit_api_status(
                        app,
                        run_id,
                        &format!("DeepSeek制限により{}秒待機しています", (wait_ms + 999) / 1000),
                        attempt,
                        true,
                    );
                    sleep(Duration::from_millis(wait_ms)).await;
                    continue;
                }
            }
        }

        emit_api_status(
            app,
            run_id,
            &format!("DeepSeek APIで停止しました: {}", failure.message),
            attempt,
            false,
        );
        return Err(format!("{}: {}", failure.class_name, failure.message));
    }
}

fn build_deepseek_body(
    settings: &Settings,
    messages: Vec<JsonValue>,
    max_tokens: u32,
    json_mode: bool,
    stream: bool,
) -> JsonValue {
    let mut body = json!({
        "model": settings.model,
        "messages": messages,
        "temperature": settings.temperature,
        "max_tokens": max_tokens,
        "stream": stream,
        "thinking": {
            "type": if settings.thinking_enabled { "enabled" } else { "disabled" }
        }
    });

    if settings.thinking_enabled {
        body["reasoning_effort"] = json!(settings.reasoning_effort);
    }

    if json_mode {
        body["response_format"] = json!({"type": "json_object"});
    }

    body
}

async fn read_deepseek_stream(
    app: &AppHandle,
    response: reqwest::Response,
    run_id: Option<&str>,
    cancel: &Arc<AtomicBool>,
) -> Result<String, String> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut answer = String::new();
    while let Some(item) = stream.next().await {
        if cancel.load(AtomicOrdering::SeqCst) {
            return Err("処理をキャンセルしました。".to_string());
        }
        let bytes = item.map_err(|e| format!("DeepSeek stream error: {e}"))?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].trim().to_string();
            buffer = buffer[pos + 1..].to_string();
            if !line.starts_with("data:") {
                continue;
            }
            let data = line.trim_start_matches("data:").trim();
            if data == "[DONE]" {
                return Ok(answer);
            }
            if let Ok(value) = serde_json::from_str::<JsonValue>(data) {
                if let Some(delta) = value
                    .get("choices")
                    .and_then(|v| v.as_array())
                    .and_then(|choices| choices.first())
                    .and_then(|choice| choice.get("delta"))
                    .and_then(|delta| delta.get("content"))
                    .and_then(|content| content.as_str())
                {
                    answer.push_str(delta);
                    let _ = app.emit(
                        "answer-delta",
                        json!({
                            "runId": run_id,
                            "delta": delta
                        }),
                    );
                }
            }
        }
    }
    Ok(answer)
}

async fn classify_api_failure(response: reqwest::Response) -> ApiFailure {
    let status = response.status();
    let retry_after_ms = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after_ms);
    let body = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<JsonValue>(&body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| {
            if body.trim().is_empty() {
                status.to_string()
            } else {
                body
            }
        });
    let class_name = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "auth",
        StatusCode::TOO_MANY_REQUESTS => "rate_limit",
        StatusCode::BAD_REQUEST if message.to_lowercase().contains("context") => "context_limit",
        StatusCode::PAYLOAD_TOO_LARGE => "context_limit",
        s if s.is_server_error() => "server",
        _ => "api_error",
    }
    .to_string();
    ApiFailure {
        class_name,
        message,
        retry_after_ms,
    }
}

fn parse_retry_after_ms(raw: &str) -> Option<u64> {
    raw.trim().parse::<u64>().ok().map(|seconds| seconds * 1000)
}

fn emit_api_status(app: &AppHandle, run_id: Option<&str>, message: &str, attempt: usize, can_cancel: bool) {
    let _ = app.emit(
        "task-progress",
        ProgressPayload {
            stage: "api".to_string(),
            status: if can_cancel { "running" } else { "stopped" }.to_string(),
            message: format!("{} (attempt {})", message, attempt),
            run_id: run_id.map(ToString::to_string),
            completed: attempt as u64,
            total: 0,
            elapsed_ms: 0,
            can_cancel,
        },
    );
}

fn build_context_from_hits(hits: &[SearchHit], max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, hit) in hits.iter().enumerate() {
        let block = format!(
            "[S{}] file={} chunk={} chars={}-{} heading={}\n{}\n\n",
            idx + 1,
            hit.file_name,
            hit.chunk_id,
            hit.char_start,
            hit.char_end,
            hit.heading_path,
            hit.snippet
        );
        if out.chars().count() + block.chars().count() > max_chars {
            break;
        }
        out.push_str(&block);
    }
    out
}

fn insert_question(conn: &Connection, text: &str) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO questions(text, created_at) VALUES (?1, ?2)",
        params![text, Utc::now().to_rfc3339()],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

fn insert_run(
    conn: &Connection,
    run_id: &str,
    question_id: i64,
    mode: &str,
    selected_document_ids: &[i64],
) -> Result<(), String> {
    conn.execute(
        r#"
        INSERT INTO retrieval_runs(id, question_id, mode, status, selected_document_ids, started_at)
        VALUES (?1, ?2, ?3, 'running', ?4, ?5)
        "#,
        params![
            run_id,
            question_id,
            mode,
            serde_json::to_string(selected_document_ids).unwrap_or_else(|_| "[]".to_string()),
            Utc::now().to_rfc3339()
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn complete_run(conn: &Connection, run_id: &str, status: &str, error: Option<&str>) -> Result<(), String> {
    conn.execute(
        "UPDATE retrieval_runs SET status = ?1, completed_at = ?2, error = ?3 WHERE id = ?4",
        params![status, Utc::now().to_rfc3339(), error, run_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn insert_fact(conn: &Connection, run_id: &str, fact: &ExtractedFact) -> Result<(), String> {
    let fact_json = serde_json::to_string(fact).unwrap_or_else(|_| fact.statement.clone());
    conn.execute(
        "INSERT INTO extracted_facts(run_id, chunk_id, fact, quote, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![run_id, fact.chunk_id, fact_json, fact.quote, Utc::now().to_rfc3339()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn save_answer(conn: &Connection, run_id: &str, answer: &str, audit: Option<&str>) -> Result<(), String> {
    conn.execute(
        "INSERT INTO answers(run_id, answer, audit_json, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![run_id, answer, audit, Utc::now().to_rfc3339()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn load_settings(conn: &Connection) -> Result<Settings, String> {
    let mut settings = Settings::default();
    settings.source_path = get_setting(conn, "source_path")?;
    if let Some(model) = get_setting(conn, "model")? {
        settings.model = model;
    }
    if let Some(value) = get_setting(conn, "thinking_enabled")? {
        settings.thinking_enabled = value == "true";
    }
    if let Some(value) = get_setting(conn, "reasoning_effort")? {
        settings.reasoning_effort = value;
    }
    if let Some(value) = get_setting(conn, "temperature")? {
        settings.temperature = value.parse().unwrap_or(settings.temperature);
    }
    if let Some(value) = get_setting(conn, "max_context_chars")? {
        settings.max_context_chars = value.parse().unwrap_or(settings.max_context_chars);
    }
    if let Some(value) = get_setting(conn, "comprehensive_batch_chars")? {
        settings.comprehensive_batch_chars = value.parse().unwrap_or(settings.comprehensive_batch_chars);
    }
    apply_simple_defaults(&mut settings);
    let (api_key, api_key_storage) = load_api_key(conn);
    settings.api_key_saved = api_key.is_some();
    settings.api_key_storage = api_key_storage;
    settings.api_key = api_key;
    Ok(settings)
}

fn apply_simple_defaults(settings: &mut Settings) {
    settings.model = DEFAULT_MODEL.to_string();
    settings.thinking_enabled = false;
    settings.reasoning_effort = "high".to_string();
    settings.temperature = 0.1;
    settings.max_context_chars = DEFAULT_CONTEXT_CHARS;
    settings.comprehensive_batch_chars = DEFAULT_BATCH_CHARS;
}

fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>, String> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |row| {
        row.get::<_, String>(0)
    })
    .optional()
    .map_err(|e| e.to_string())
}

fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn delete_setting(conn: &Connection, key: &str) -> Result<(), String> {
    conn.execute("DELETE FROM settings WHERE key = ?1", params![key])
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn load_documents(conn: &Connection) -> Result<Vec<DocumentInfo>, String> {
    let mut stmt = conn
        .prepare(
            r#"
            SELECT d.id, d.path, d.file_name, d.size, d.mtime_ms, d.sha256, d.char_count,
                   d.selected, d.indexed_at, d.status, d.error, COUNT(c.id) AS chunk_count
            FROM documents d
            LEFT JOIN chunks c ON c.document_id = d.id
            GROUP BY d.id
            ORDER BY d.file_name
            "#,
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(DocumentInfo {
                id: row.get(0)?,
                path: row.get(1)?,
                file_name: row.get(2)?,
                size: row.get(3)?,
                mtime_ms: row.get(4)?,
                sha256: row.get(5)?,
                char_count: row.get(6)?,
                selected: row.get::<_, i64>(7)? == 1,
                indexed_at: row.get(8)?,
                status: row.get(9)?,
                error: row.get(10)?,
                chunk_count: row.get(11)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

fn load_stats(conn: &Connection) -> Result<AppStats, String> {
    let document_count = scalar_i64(conn, "SELECT COUNT(*) FROM documents")?;
    let selected_document_count = scalar_i64(conn, "SELECT COUNT(*) FROM documents WHERE selected = 1")?;
    let total_chars = scalar_i64(conn, "SELECT COALESCE(SUM(char_count), 0) FROM documents")?;
    let selected_chars = scalar_i64(conn, "SELECT COALESCE(SUM(char_count), 0) FROM documents WHERE selected = 1")?;
    let chunk_count = scalar_i64(conn, "SELECT COUNT(*) FROM chunks")?;
    let embedding_count = scalar_i64(conn, "SELECT COUNT(*) FROM chunk_embeddings")?;
    Ok(AppStats {
        document_count,
        selected_document_count,
        total_chars,
        selected_chars,
        chunk_count,
        embedding_count,
    })
}

fn scalar_i64(conn: &Connection, sql: &str) -> Result<i64, String> {
    conn.query_row(sql, [], |row| row.get::<_, i64>(0))
        .map_err(|e| e.to_string())
}

fn mark_missing_documents(conn: &Connection, found_paths: &HashSet<String>) -> Result<(), String> {
    let documents = load_documents(conn)?;
    for doc in documents {
        if !found_paths.contains(&doc.path) {
            conn.execute(
                "UPDATE documents SET status = 'missing', error = 'sourceから見つかりません' WHERE id = ?1",
                params![doc.id],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn record_document_error(conn: &Connection, path: &Path, error: &str) -> Result<(), String> {
    let path_string = path.to_string_lossy().to_string();
    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    conn.execute(
        r#"
        INSERT INTO documents(path, file_name, status, error)
        VALUES (?1, ?2, 'error', ?3)
        ON CONFLICT(path) DO UPDATE SET status = 'error', error = excluded.error
        "#,
        params![path_string, file_name, error],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn log_event(
    conn: &Connection,
    operation: &str,
    status: &str,
    message: &str,
    run_id: Option<&str>,
    metadata: JsonValue,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO logs(timestamp, operation, status, message, run_id, metadata_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            Utc::now().to_rfc3339(),
            operation,
            status,
            message,
            run_id,
            metadata.to_string()
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn save_api_key(conn: &Connection, api_key: &str) -> Result<String, String> {
    match keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER)
        .map_err(|e| e.to_string())
        .and_then(|entry| entry.set_password(api_key).map_err(|e| e.to_string()))
    {
        Ok(_) => {
            delete_setting(conn, "api_key_plaintext")?;
            delete_setting(conn, "api_key_keychain_error")?;
            set_setting(conn, "api_key_storage", "os-keychain")?;
            Ok("os-keychain".to_string())
        }
        Err(error) => {
            set_setting(conn, "api_key_plaintext", api_key)?;
            set_setting(conn, "api_key_storage", "sqlite-plaintext-fallback")?;
            set_setting(conn, "api_key_keychain_error", &error)?;
            Ok("sqlite-plaintext-fallback".to_string())
        }
    }
}

fn load_api_key(conn: &Connection) -> (Option<String>, String) {
    if let Ok(entry) = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER) {
        if let Ok(password) = entry.get_password() {
            if !password.trim().is_empty() {
                return (Some(password), "os-keychain".to_string());
            }
        }
    }

    match get_setting(conn, "api_key_plaintext") {
        Ok(Some(value)) if !value.trim().is_empty() => {
            (Some(value), "sqlite-plaintext-fallback".to_string())
        }
        _ => (None, "none".to_string()),
    }
}

fn delete_api_key(conn: &Connection) -> Result<(), String> {
    if let Ok(entry) = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER) {
        let _ = entry.delete_credential();
    }
    delete_setting(conn, "api_key_plaintext")?;
    delete_setting(conn, "api_key_keychain_error")?;
    set_setting(conn, "api_key_storage", "none")?;
    Ok(())
}

fn emit_progress(
    app: &AppHandle,
    started: Instant,
    stage: &str,
    status: &str,
    message: &str,
    run_id: Option<String>,
    completed: u64,
    total: u64,
    can_cancel: bool,
) {
    let _ = app.emit(
        "task-progress",
        ProgressPayload {
            stage: stage.to_string(),
            status: status.to_string(),
            message: message.to_string(),
            run_id,
            completed,
            total,
            elapsed_ms: started.elapsed().as_millis(),
            can_cancel,
        },
    );
}

fn placeholders(count: usize) -> String {
    (0..count).map(|_| "?").collect::<Vec<_>>().join(",")
}

fn extract_terms(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();
    let mut current_kind = 0u8;
    for ch in text.chars() {
        let kind = token_kind(ch);
        if kind == 0 {
            flush_token(&mut terms, &current, current_kind);
            current.clear();
            current_kind = 0;
            continue;
        }
        if current_kind != 0 && current_kind != kind {
            flush_token(&mut terms, &current, current_kind);
            current.clear();
        }
        current.push(ch);
        current_kind = kind;
    }
    flush_token(&mut terms, &current, current_kind);
    terms.sort();
    terms.dedup();
    terms
}

fn flush_token(terms: &mut Vec<String>, token: &str, kind: u8) {
    if token.trim().is_empty() {
        return;
    }
    let normalized = if kind == 1 {
        token.to_ascii_lowercase()
    } else {
        token.to_string()
    };
    let chars: Vec<char> = normalized.chars().collect();
    if chars.len() >= 2 && chars.len() <= 40 {
        terms.push(normalized.clone());
    }
    if kind != 1 {
        for n in [2usize, 3usize] {
            if chars.len() >= n {
                for gram in chars.windows(n) {
                    terms.push(gram.iter().collect());
                }
            }
        }
    }
}

fn token_kind(ch: char) -> u8 {
    if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
        return 1;
    }
    let code = ch as u32;
    if (0x3040..=0x30ff).contains(&code)
        || (0x3400..=0x9fff).contains(&code)
        || (0xf900..=0xfaff).contains(&code)
        || (0xff66..=0xff9f).contains(&code)
    {
        return 2;
    }
    0
}

fn term_weights(text: &str) -> HashMap<String, f64> {
    let mut weights = HashMap::new();
    for term in extract_terms(text) {
        let len = term.chars().count() as f64;
        let weight = if len >= 4.0 { 2.0 } else { 1.0 };
        *weights.entry(term).or_insert(0.0) += weight;
    }
    weights
}

fn embedding_for_text(text: &str) -> Vec<f32> {
    let mut vector = vec![0f32; EMBEDDING_DIMS];
    for (term, weight) in term_weights(text) {
        let digest = Sha256::digest(term.as_bytes());
        let index = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]) as usize % EMBEDDING_DIMS;
        let sign = if digest[4] & 1 == 0 { 1.0 } else { -1.0 };
        vector[index] += sign * weight as f32;
    }
    normalize_vector(&mut vector);
    vector
}

fn normalize_vector(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in vector.iter_mut() {
            *value /= norm;
        }
    }
}

fn encode_vector(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

fn decode_vector(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

fn dot_product(a: &[f32], b: &[f32]) -> f64 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum()
}

fn exact_bonus(query: &str, terms: &[String], text: &str) -> f64 {
    let mut bonus: f64 = 0.0;
    if !query.trim().is_empty() && text.contains(query.trim()) {
        bonus += 0.18;
    }
    for term in terms.iter().take(30) {
        if term.chars().count() >= 2 && text.contains(term) {
            bonus += if term.chars().count() >= 4 { 0.045 } else { 0.02 };
        }
    }
    bonus.min(0.45)
}

fn make_snippet(content: &str, terms: &[String], max_chars: usize) -> String {
    let chars: Vec<char> = content.chars().collect();
    if chars.len() <= max_chars {
        return content.to_string();
    }
    let mut hit_index = 0usize;
    for term in terms {
        if term.chars().count() < 2 {
            continue;
        }
        if let Some(byte_pos) = content.find(term) {
            hit_index = content[..byte_pos].chars().count();
            break;
        }
    }
    let start = hit_index.saturating_sub(max_chars / 3);
    let end = (start + max_chars).min(chars.len());
    let mut snippet: String = chars[start..end].iter().collect();
    if start > 0 {
        snippet.insert_str(0, "...");
    }
    if end < chars.len() {
        snippet.push_str("...");
    }
    snippet
}

fn diversify_hits(hits: &mut Vec<SearchHit>, limit: usize) {
    let mut by_doc: HashMap<i64, usize> = HashMap::new();
    for hit in hits.iter_mut() {
        let count = by_doc.entry(hit.document_id).or_insert(0);
        if *count >= 3 {
            hit.score *= 0.86;
        }
        *count += 1;
    }
    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
    if hits.len() > limit * 3 {
        hits.truncate(limit * 3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn chunker_preserves_japanese_regulation_headings() {
        let text = "第1章 総則\n（目的）\nこの規程は目的を定める。\n\n第2条 扶養手当\n扶養手当は条件を満たす職員に支給する。\n";
        let chunks = split_text_into_chunks(text);
        assert!(!chunks.is_empty());
        assert!(chunks.iter().any(|chunk| chunk.heading_path.contains("第2条 扶養手当")));
        assert!(chunks.iter().any(|chunk| chunk.content.contains("扶養手当")));
    }

    #[test]
    fn index_and_hybrid_search_find_short_japanese_terms() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("mini-lm-test.sqlite3");
        let source_dir = temp.path().join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let file_path = source_dir.join("rules.txt");
        fs::write(
            &file_path,
            "第1章 給与\n第1条 扶養手当\n扶養手当は、扶養親族を有する職員に支給する。\n第2条 通勤手当\n通勤手当は通勤距離に応じて支給する。\n",
        )
        .unwrap();

        let state = AppStateInner::new(db_path);
        init_db(&state).unwrap();
        let mut conn = state.conn().unwrap();
        set_setting(&conn, "source_path", source_dir.to_str().unwrap()).unwrap();
        let outcome = index_one_file(&mut conn, &file_path).unwrap();
        match outcome {
            FileIndexOutcome::Indexed(count) => assert!(count > 0),
            FileIndexOutcome::Skipped => panic!("first index should not be skipped"),
        }

        let response = hybrid_search_internal(&conn, "扶養手当", &[], 5).unwrap();
        assert!(!response.hits.is_empty());
        assert!(response.hits[0].snippet.contains("扶養手当"));
        assert!(response.hits[0].ngram_score > 0.0 || response.hits[0].vector_score > 0.0);
    }

    #[test]
    fn index_builds_hierarchy_contexts_for_parent_recall() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("mini-lm-test.sqlite3");
        let source_dir = temp.path().join("source");
        fs::create_dir_all(&source_dir).unwrap();
        let file_path = source_dir.join("rules.txt");
        fs::write(
            &file_path,
            "第1章 給与\n第1条 扶養手当\n扶養手当は、扶養親族を有する職員に支給する。\n第2条 住宅手当\n住宅手当は借家に居住する職員に支給する。\n",
        )
        .unwrap();

        let state = AppStateInner::new(db_path);
        init_db(&state).unwrap();
        let mut conn = state.conn().unwrap();
        index_one_file(&mut conn, &file_path).unwrap();

        let section_count = scalar_i64(&conn, "SELECT COUNT(*) FROM section_contexts").unwrap();
        let document_count = scalar_i64(&conn, "SELECT COUNT(*) FROM document_contexts").unwrap();
        assert!(section_count >= 2);
        assert_eq!(document_count, 1);

        let chunks = load_selected_chunks(&conn, &[]).unwrap();
        let mut profile = QuestionProfile::from_question("手当はどうなっていますか");
        enrich_profile_with_source_concepts(&mut profile, &chunks, "手当はどうなっていますか");
        let hierarchy = load_hierarchy_context(&conn, &[], &profile, &[]).unwrap();
        let context = build_batch_hierarchy_context(&chunks[..1], &hierarchy);

        assert!(context.contains("DOCUMENT parents"));
        assert!(context.contains("SECTION parents"));
        assert!(context.contains("扶養手当") || context.contains("住宅手当"));
    }

    #[test]
    fn deepseek_body_omits_reasoning_effort_when_thinking_is_disabled() {
        let settings = Settings {
            thinking_enabled: false,
            reasoning_effort: "high".to_string(),
            ..Settings::default()
        };
        let body = build_deepseek_body(
            &settings,
            vec![json!({"role": "user", "content": "test"})],
            100,
            false,
            false,
        );

        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn deepseek_body_sets_reasoning_effort_when_thinking_is_enabled() {
        let settings = Settings {
            thinking_enabled: true,
            reasoning_effort: "max".to_string(),
            ..Settings::default()
        };
        let body = build_deepseek_body(
            &settings,
            vec![json!({"role": "user", "content": "test"})],
            100,
            true,
            true,
        );

        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["reasoning_effort"], "max");
        assert_eq!(body["response_format"]["type"], "json_object");
    }

    #[test]
    fn broad_question_discovers_source_derived_lower_concepts() {
        let chunks = vec![
            ChunkRecord {
                id: 1,
                document_id: 1,
                file_name: "rules.txt".to_string(),
                path: "/tmp/rules.txt".to_string(),
                heading_path: "第1章 給与 > 第1条 扶養手当".to_string(),
                char_start: 0,
                char_end: 10,
                content: "扶養手当は、扶養親族を有する職員に支給する。".to_string(),
                prev_chunk_id: None,
                next_chunk_id: Some(2),
            },
            ChunkRecord {
                id: 2,
                document_id: 1,
                file_name: "rules.txt".to_string(),
                path: "/tmp/rules.txt".to_string(),
                heading_path: "第1章 給与 > 第2条 通勤手当".to_string(),
                char_start: 11,
                char_end: 20,
                content: "通勤手当は、通勤距離に応じて支給する。".to_string(),
                prev_chunk_id: Some(1),
                next_chunk_id: None,
            },
        ];
        let mut profile = QuestionProfile::from_question("手当はどうなっていますか");
        enrich_profile_with_source_concepts(&mut profile, &chunks, "手当はどうなっていますか");

        assert!(profile.is_broad);
        assert!(profile.lower_concepts.contains(&"扶養手当".to_string()));
        assert!(profile.lower_concepts.contains(&"通勤手当".to_string()));
    }

    #[test]
    fn broad_question_discovers_repeated_content_concepts_without_headings() {
        let chunks = vec![ChunkRecord {
            id: 1,
            document_id: 1,
            file_name: "rules.txt".to_string(),
            path: "/tmp/rules.txt".to_string(),
            heading_path: "第1章 給与".to_string(),
            char_start: 0,
            char_end: 80,
            content: "住宅手当は借家に居住する職員に支給する。住宅手当の額は別表で定める。".to_string(),
            prev_chunk_id: None,
            next_chunk_id: None,
        }];
        let mut profile = QuestionProfile::from_question("手当はどうなっていますか");
        enrich_profile_with_source_concepts(&mut profile, &chunks, "手当はどうなっていますか");

        assert!(profile.lower_concepts.contains(&"住宅手当".to_string()));
    }

    #[test]
    fn audit_revision_detector_checks_relationship_scope_and_polarity_errors() {
        assert!(audit_needs_revision(
            r#"{"verdict":"pass","unsupported_claims":[],"relationship_errors":[{"claim":"x"}],"scope_errors":[],"polarity_errors":[],"insufficient_evidence_overreach":[]}"#
        ));
        assert!(audit_needs_revision(
            r#"{"verdict":"warning","unsupported_claims":[],"relationship_errors":[],"scope_errors":[],"polarity_errors":[],"insufficient_evidence_overreach":[]}"#
        ));
        assert!(!audit_needs_revision(
            r#"{"verdict":"pass","unsupported_claims":[],"relationship_errors":[],"scope_errors":[],"polarity_errors":[],"insufficient_evidence_overreach":[]}"#
        ));
    }

    #[test]
    fn fact_statement_can_be_rebuilt_from_structured_fields() {
        let statement = compose_fact_statement("職員", "扶養手当", "扶養親族を有する場合", "支給する", "");
        assert!(statement.contains("subject=職員"));
        assert!(statement.contains("object=扶養手当"));
        assert!(statement.contains("condition=扶養親族を有する場合"));
        assert!(statement.contains("effect=支給する"));
    }
}
