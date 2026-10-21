use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Multipart, Path as AxPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use serde_json::json;

use dj_music_core::convert::{self, ConvertOpts};
use dj_music_core::cover as core_cover;
use dj_music_core::dedup as core_dedup;
use dj_music_core::import::{self as core_import, Mode};
use dj_music_core::reorg as core_reorg;
use dj_music_core::transcode as core_transcode;
use dj_music_core::trash as core_trash;

use crate::state::{start_job, JobKind, JobStatus, AppState};

/// Built frontend (crates/server/frontend/dist), embedded into release
/// builds; debug builds read the folder live so a `bun run build` output is
/// picked up on restart.
#[derive(rust_embed::Embed)]
#[folder = "frontend/dist"]
struct Assets;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
        .route("/api/config", get(get_config).post(set_config))
        .route("/api/upload", post(upload))
        .route("/api/detect-netease", get(detect_netease))
        .route("/api/staging/from-folder", post(staging_from_folder))
        .route("/api/convert", post(start_convert))
        .route("/api/import/analyze", post(import_analyze))
        .route("/api/import/execute", post(import_execute))
        .route("/api/reorg/analyze", post(reorg_analyze))
        .route("/api/reorg/execute", post(reorg_execute))
        .route("/api/reorg/plan", get(reorg_plan))
        .route("/api/dedup/analyze", post(dedup_analyze))
        .route("/api/dedup/execute", post(dedup_execute))
        .route("/api/dedup/plan", get(dedup_plan))
        .route("/api/transcode/analyze", post(transcode_analyze))
        .route("/api/transcode/execute", post(transcode_execute))
        .route("/api/transcode/plan", get(transcode_plan))
        .route("/api/transcode/ffmpeg", get(transcode_ffmpeg))
        .route("/api/cover/analyze", post(cover_analyze))
        .route("/api/cover/execute", post(cover_execute))
        .route("/api/cover/plan", get(cover_plan))
        .route("/api/cover/artwork", get(cover_artwork))
        .route("/api/trash", get(trash_list))
        .route("/api/trash/restore", post(trash_restore))
        .route("/api/trash/delete", post(trash_delete))
        .route("/api/job", get(job_snapshot))
        .route("/api/events", get(events))
        .route("/api/staging/{id}", delete(clear_staging))
        .layer(axum::extract::DefaultBodyLimit::max(2usize << 30))
        .with_state(state)
}

async fn index() -> Response {
    match Assets::get("index.html") {
        Some(file) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            file.data,
        )
            .into_response(),
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "frontend not built (run `just web`)",
        )
            .into_response(),
    }
}

async fn asset(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    // the route strips the /assets prefix; the embed keys keep it
    let key = format!("assets/{path}");
    match Assets::get(&key) {
        Some(file) => {
            let mime = mime_guess::from_path(&key).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.as_ref())],
                [("cache-control", "public, max-age=3600")],
                file.data,
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

fn bad_request(message: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": message.into()}))).into_response()
}

fn conflict(message: impl Into<String>) -> Response {
    (StatusCode::CONFLICT, Json(json!({"error": message.into()}))).into_response()
}

// a tiny alias so the helper signatures above stay readable
use axum::Json;

// ---------- config ----------

async fn get_config(State(state): State<Arc<AppState>>) -> Response {
    let config = state.config.lock();
    Json(json!({
        "library_root": config.library_root,
    }))
    .into_response()
}

async fn set_config(State(state): State<Arc<AppState>>, Json(body): Json<serde_json::Value>) -> Response {
    let Some(root) = body.get("library_root").and_then(|v| v.as_str()) else {
        return bad_request("缺少 library_root");
    };
    let root = root.trim();
    if root.is_empty() {
        return bad_request("library_root 不能为空");
    }
    let path = std::path::PathBuf::from(root);
    if !path.is_dir() {
        return bad_request(format!("目录不存在: {root}"));
    }
    let canonical = std::fs::canonicalize(&path)
        .map_err(|e| format!("无法解析路径: {e}"))
        .and_then(|p| p.to_str().map(str::to_string).ok_or_else(|| "路径包含非 UTF-8 字符".to_string()));
    let canonical = match canonical {
        Ok(p) => p,
        Err(e) => return bad_request(e),
    };
    {
        let mut config = state.config.lock();
        config.library_root = Some(canonical.clone());
        if let Err(e) = dj_music_core::config::save(&state.paths.config_file, &config) {
            return bad_request(format!("保存配置失败: {e}"));
        }
    }
    Json(json!({"library_root": canonical})).into_response()
}

// ---------- upload ----------

fn file_kind(name: &str) -> &'static str {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "ncm" => "ncm",
        "mp3" | "flac" | "m4a" | "aac" | "wav" | "aiff" | "aif" => "audio",
        "lrc" => "lyrics",
        "jpg" | "jpeg" | "png" | "webp" | "gif" => "image",
        _ => "other",
    }
}

/// Make an uploaded relative path safe to join onto the staging dir:
/// no traversal, no hidden components, forward slashes only.
fn sanitize_rel(rel: &str) -> Option<String> {
    let comps: Vec<&str> = rel
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    if comps.is_empty() || comps.iter().any(|c| *c == ".." || c.starts_with('.')) {
        return None;
    }
    Some(comps.join("/"))
}

async fn upload(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Response {
    let id = uuid::Uuid::new_v4().to_string();
    let dir = state.paths.staging_root.join(&id);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return bad_request(format!("无法创建暂存目录: {e}"));
    }

    // the client sends a "paths" field (JSON array of relative paths) before
    // the "file" parts, so folder structure survives the upload; files sent
    // without one land flat under their basename
    let mut rel_paths: Option<Vec<String>> = None;
    let mut files = Vec::new();
    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => match field.name() {
                Some("paths") => {
                    let text = field.text().await.unwrap_or_default();
                    rel_paths = serde_json::from_str::<Vec<String>>(&text).ok();
                }
                Some("file") => {
                    let raw_name = field.file_name().map(str::to_string);
                    let Some(raw_name) = raw_name else { continue };
                    let Some(name) = std::path::Path::new(&raw_name)
                        .file_name()
                        .and_then(|n| n.to_str())
                    else {
                        continue;
                    };
                    let idx = files.len();
                    let rel = rel_paths
                        .as_ref()
                        .and_then(|p| p.get(idx))
                        .and_then(|r| sanitize_rel(r));
                    let stored = rel.unwrap_or_else(|| name.to_string());
                    let path = dir.join(&stored);
                    if let Some(parent) = path.parent() {
                        if let Err(e) = tokio::fs::create_dir_all(parent).await {
                            let _ = tokio::fs::remove_dir_all(&dir).await;
                            return bad_request(format!("无法创建目录: {e}"));
                        }
                    }
                    let mut file = match tokio::fs::File::create(&path).await {
                        Ok(f) => f,
                        Err(e) => {
                            let _ = tokio::fs::remove_dir_all(&dir).await;
                            return bad_request(format!("无法写入文件 {name}: {e}"));
                        }
                    };
                    if let Err(e) = write_field(&mut file, field).await {
                        let _ = tokio::fs::remove_dir_all(&dir).await;
                        return bad_request(format!("接收文件 {name} 失败: {e}"));
                    }
                    files.push(json!({"name": stored, "kind": file_kind(&stored)}));
                }
                _ => continue,
            },
            Ok(None) => break,
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&dir).await;
                return bad_request(format!("上传中断: {e}"));
            }
        }
    }
    if files.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
        return bad_request("没有收到文件");
    }
    Json(json!({"staging_id": id, "files": files})).into_response()
}

async fn write_field(
    file: &mut tokio::fs::File,
    mut field: axum::extract::multipart::Field<'_>,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    while let Some(chunk) = field.chunk().await.map_err(|e| e.to_string())? {
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
    }
    file.flush().await.map_err(|e| e.to_string())
}

// ---------- netease folder detection ----------

/// Deepest directory containing every .ncm under `root` (None when there are
/// none). Lets convert scan one folder even when uploads kept a structure.
fn ncm_common_dir(root: &Path) -> Option<std::path::PathBuf> {
    let mut files: Vec<std::path::PathBuf> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("ncm"))
                .unwrap_or(false)
        })
        .map(|e| e.into_path())
        .collect();
    if files.is_empty() {
        return None;
    }
    files.sort();
    let mut common = files[0].parent()?.to_path_buf();
    for file in &files[1..] {
        let parent = file.parent().unwrap();
        while !parent.starts_with(&common) {
            common = common.parent()?.to_path_buf();
        }
    }
    Some(common)
}

fn count_ncm(dir: &Path) -> usize {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("ncm"))
                .unwrap_or(false)
        })
        .count()
}

/// Best-effort detection of the NetEase Cloud Music download folder across
/// platforms: the first candidate that exists and actually contains .ncm
/// files wins.
async fn detect_netease() -> Response {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(music) = dirs::audio_dir() {
        for name in ["网易云音乐", "CloudMusic", "Netease Cloud Music"] {
            candidates.push(music.join(name));
        }
    }
    for candidate in candidates {
        if candidate.is_dir() {
            let count = count_ncm(&candidate);
            if count > 0 {
                return Json(json!({
                    "path": candidate.display().to_string(),
                    "count": count,
                }))
                .into_response();
            }
        }
    }
    Json(json!({"path": null})).into_response()
}

/// Register an existing local folder as a batch: nothing is copied; convert
/// reads the folder in place and only the converted output lands in staging.
async fn staging_from_folder(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(raw) = body.get("path").and_then(|v| v.as_str()) else {
        return bad_request("缺少 path");
    };
    let Ok(path) = std::fs::canonicalize(raw) else {
        return bad_request(format!("目录不存在: {raw}"));
    };
    if !path.is_dir() {
        return bad_request(format!("不是目录: {raw}"));
    }

    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(&path)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let Ok(rel) = entry.path().strip_prefix(&path) else {
            continue;
        };
        let Some(rel) = rel.to_str() else { continue };
        files.push(json!({"name": rel, "kind": file_kind(rel)}));
        if files.len() >= 2000 {
            break;
        }
    }
    let has_input = files
        .iter()
        .any(|f| f["kind"] == "ncm" || f["kind"] == "audio");
    if !has_input {
        return bad_request("该文件夹里没有 .ncm 或音频文件");
    }

    let id = uuid::Uuid::new_v4().to_string();
    let dir = state.paths.staging_root.join(&id);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return bad_request(format!("无法创建暂存目录: {e}"));
    }
    let Some(folder) = path.to_str().map(str::to_string) else {
        return bad_request("路径包含非 UTF-8 字符");
    };
    if let Err(e) = tokio::fs::write(dir.join("input-folder"), &folder).await {
        return bad_request(format!("无法登记文件夹: {e}"));
    }
    Json(json!({"staging_id": id, "files": files})).into_response()
}

// ---------- job starters ----------

fn staging_dir_or_error(state: &AppState, id: &str) -> Result<std::path::PathBuf, Response> {
    state
        .staging_dir(id)
        .filter(|p| p.is_dir())
        .ok_or_else(|| bad_request(format!("暂存目录不存在: {id}")))
}

async fn start_convert(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(staging_id) = body.get("staging_id").and_then(|v| v.as_str()).map(str::to_string) else {
        return bad_request("缺少 staging_id");
    };
    let dir = match staging_dir_or_error(&state, &staging_id) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let threads = body.get("threads").and_then(|v| v.as_u64()).unwrap_or(8) as usize;
    let no_download = body.get("no_download").and_then(|v| v.as_bool()).unwrap_or(false);

    // two batch kinds: uploaded files live in the staging dir itself, while
    // "from folder" batches carry an input-folder pointer and are read in
    // place (only the converted output lands in staging)
    let mut input = match tokio::fs::read_to_string(dir.join("input-folder")).await {
        Ok(p) if Path::new(&p).is_dir() => std::path::PathBuf::from(p),
        _ => dir.clone(),
    };
    // convert scans one folder non-recursively; when uploads kept a folder
    // structure, narrow the input to where the .ncm files actually are
    if let Some(ncm_dir) = ncm_common_dir(&input) {
        if ncm_dir != input {
            input = ncm_dir;
        }
    }
    let out_dir = dir.join("converted");
    let meta_dir = input.join("meta");
    let opts = ConvertOpts {
        input,
        output: out_dir,
        threads: threads.max(1),
        meta_dir: Some(meta_dir),
        no_download,
    };

    let result = start_job(&state, JobKind::Convert, Some(staging_id.clone()), move |sink: dj_music_core::Sink| {
        let summary = convert::run(opts, sink).map_err(|e| e.to_string())?;
        Ok(json!({
            "total": summary.total,
            "tagged": summary.tagged,
            "failed": summary.failed,
            "output": "converted",
        }))
    });
    match result {
        Ok(()) => Json(json!({"started": true})).into_response(),
        Err(e) => conflict(e),
    }
}

async fn import_analyze(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(staging_id) = body.get("staging_id").and_then(|v| v.as_str()).map(str::to_string) else {
        return bad_request("缺少 staging_id");
    };
    let dir = match staging_dir_or_error(&state, &staging_id) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let Some(root) = state.library_root() else {
        return bad_request("请先在设置里指定曲库根目录");
    };
    let template = body
        .get("template")
        .and_then(|v| v.as_str())
        .unwrap_or("{artist}/{filename}.{ext}")
        .to_string();

    let state2 = Arc::clone(&state);
    let sid = staging_id.clone();
    let result = start_job(&state, JobKind::ImportAnalyze, Some(staging_id), move |sink: dj_music_core::Sink| {
        let plan = core_import::analyze(&dir, Some(&root), &template, sink).map_err(|e| e.to_string())?;
        state2.plans.lock().insert(sid, plan.clone());
        serde_json::to_value(&plan).map_err(|e| e.to_string())
    });
    match result {
        Ok(()) => Json(json!({"started": true})).into_response(),
        Err(e) => conflict(e),
    }
}

async fn import_execute(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(staging_id) = body.get("staging_id").and_then(|v| v.as_str()).map(str::to_string) else {
        return bad_request("缺少 staging_id");
    };
    let mode = match body.get("mode").and_then(|v| v.as_str()) {
        Some("move") => Mode::Move,
        _ => Mode::Copy,
    };
    let overwrite = body.get("overwrite").and_then(|v| v.as_bool()).unwrap_or(false);
    // per-item decisions from the preview table: only the listed files are
    // placed; None (or missing) keeps the whole plan
    let include: Option<Vec<String>> = body.get("include").and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect()
    });
    let plan = state.plans.lock().get(&staging_id).cloned();
    let Some(mut plan) = plan else {
        return bad_request("请先运行导入分析");
    };
    if let Some(include) = &include {
        plan.items
            .retain(|item| include.contains(&item.src.to_string_lossy().into_owned()));
    }

    let staging_dir = state
        .staging_dir(&staging_id)
        .filter(|p| p.is_dir());
    let state2 = Arc::clone(&state);
    let sid = staging_id.clone();
    let result = start_job(&state, JobKind::ImportExecute, Some(staging_id), move |sink: dj_music_core::Sink| {
        let summary = core_import::execute(&plan, mode, overwrite, sink);
        // fully successful batch: drop the staging dir (uploads + converted
        // output) and the stored plan; "from folder" originals live outside
        // staging and are not touched. Failures keep the scene for a retry.
        let cleaned = summary.failed == 0;
        if cleaned {
            if let Some(dir) = &staging_dir {
                let _ = std::fs::remove_dir_all(dir);
            }
            state2.plans.lock().remove(&sid);
        }
        Ok(json!({
            "placed": summary.placed,
            "failed": summary.failed,
            "cleaned": cleaned,
        }))
    });
    match result {
        Ok(()) => Json(json!({"started": true})).into_response(),
        Err(e) => conflict(e),
    }
}

// ---------- reorg / dedup ----------
//
// Both operate on the configured library root (no staging) and follow the
// same analyze -> review -> execute shape as import: the analysis is kept in
// memory and the execute request only refers to it, never to a client
// supplied path list, so the HTTP surface can't be talked into moving or
// deleting files the user never saw.

fn library_root_or_error(state: &AppState) -> Result<PathBuf, Response> {
    state
        .library_root()
        .ok_or_else(|| bad_request("请先在设置里指定曲库根目录"))
}

/// The reorg plan is exactly what the preview table needs (root, template,
/// entries, suspected duplicates), so it goes out as-is.
fn reorg_plan_json(plan: &core_reorg::Plan) -> serde_json::Value {
    json!({
        "root": plan.root,
        "template": plan.template,
        "entries": plan.entries,
        "duplicates": plan.duplicates,
    })
}

/// Dedup plan minus the hash/score cache: that map is large and the browser
/// renders neither part of it.
fn dedup_plan_json(plan: &core_dedup::Plan, stats: core_dedup::DedupStats) -> serde_json::Value {
    json!({
        "root": plan.root,
        "groups": plan.groups,
        "stats": { "scanned": stats.scanned, "unreadable": stats.unreadable },
    })
}

// ---------- transcode ----------
//
// The one command that rewrites audio instead of moving it, so it is the one
// that needs an external encoder: ffmpeg is located once per request and the
// target's encoder is checked before a plan exists. Like reorg/dedup, execute
// only ever refers to the plan held in memory.

/// A transcode plan is just entries + settings; it goes out as-is.
fn transcode_plan_json(plan: &core_transcode::Plan) -> serde_json::Value {
    json!({
        "input": plan.input,
        "output": plan.output,
        "target": plan.target,
        "quality": plan.quality,
        "replace": plan.replace,
        "trash": plan.trash,
        "entries": plan.entries,
    })
}

fn transcode_target_or_error(raw: Option<&str>) -> Result<core_transcode::TargetFormat, Response> {
    let raw = raw.unwrap_or_default();
    core_transcode::TargetFormat::parse(raw)
        .ok_or_else(|| bad_request(format!("不支持的目标格式: {raw}（可选 mp3 / flac / m4a / alac / wav）")))
}

/// Where the converted files go. Absent/empty means "next to each source";
/// otherwise a folder *inside* the library root, never an arbitrary path: the
/// web surface only ever writes inside the library it manages.
fn transcode_output_or_error(root: &Path, raw: Option<&str>) -> Result<Option<PathBuf>, Response> {
    let raw = raw.map(str::trim).unwrap_or_default();
    if raw.is_empty() {
        return Ok(None);
    }
    let rel = Path::new(raw);
    let escapes = rel.is_absolute()
        || rel
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)));
    if escapes {
        return Err(bad_request("输出目录必须是曲库内的相对子目录"));
    }
    Ok(Some(root.join(rel)))
}

/// The ffmpeg this server would use: `ffmpeg` on $PATH. Reported up front so
/// the tab can disable targets this build cannot encode instead of failing a
/// run halfway.
async fn transcode_ffmpeg() -> Response {
    match core_transcode::Ffmpeg::detect(None) {
        Ok(ff) => {
            let targets: Vec<serde_json::Value> = core_transcode::TargetFormat::all()
                .iter()
                .map(|target| {
                    let missing: Vec<&str> = target
                        .encoders()
                        .iter()
                        .copied()
                        .filter(|e| !ff.has_encoder(e))
                        .collect();
                    json!({
                        "target": target,
                        "label": target.label(),
                        "ok": missing.is_empty(),
                        "missing": missing,
                    })
                })
                .collect();
            Json(json!({
                "available": true,
                "version": ff.version,
                "path": ff.path,
                "targets": targets,
            }))
            .into_response()
        }
        Err(e) => Json(json!({
            "available": false,
            "error": e.to_string(),
        }))
        .into_response(),
    }
}

async fn transcode_analyze(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let root = match library_root_or_error(&state) {
        Ok(root) => root,
        Err(resp) => return resp,
    };
    let target = match transcode_target_or_error(body.get("to").and_then(|v| v.as_str())) {
        Ok(target) => target,
        Err(resp) => return resp,
    };
    let output = match transcode_output_or_error(&root, body.get("output").and_then(|v| v.as_str()))
    {
        Ok(output) => output,
        Err(resp) => return resp,
    };
    let quality = body
        .get("quality")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_string);
    let replace = body
        .get("replace")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let state2 = Arc::clone(&state);
    let result = start_job(
        &state,
        JobKind::TranscodeAnalyze,
        None,
        move |sink: dj_music_core::Sink| {
            let opts = core_transcode::TranscodeOpts {
                input: root,
                output,
                target,
                quality,
                replace,
                threads: core_transcode::DEFAULT_THREADS,
                // no --ffmpeg override in the web UI: it uses $PATH
                ffmpeg: None,
                trash: None,
                // no plan file for the web UI: the plan stays in memory
                plan: None,
                from: None,
            };
            let plan = core_transcode::analyze(&opts, sink).map_err(|e| e.to_string())?;
            let value = transcode_plan_json(&plan);
            state2.library.lock().transcode = Some(plan);
            Ok(value)
        },
    );
    match result {
        Ok(()) => Json(json!({"started": true})).into_response(),
        Err(e) => conflict(e),
    }
}

async fn transcode_execute(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(items) = body.get("include").and_then(|v| v.as_array()) else {
        return bad_request("缺少 include（要转换的条目列表）");
    };
    let include: BTreeSet<PathBuf> = items
        .iter()
        .filter_map(|v| v.as_str())
        .map(PathBuf::from)
        .collect();
    let held = state.library.lock().transcode.clone();
    let Some(plan) = held else {
        return bad_request("请先运行转码分析");
    };

    let state2 = Arc::clone(&state);
    let result = start_job(
        &state,
        JobKind::TranscodeExecute,
        None,
        move |sink: dj_music_core::Sink| {
            // convert exactly the ticked rows
            let mut plan = plan;
            plan.entries.retain(|e| include.contains(&e.src));
            let trash = plan.trash.clone();
            let summary = core_transcode::execute_plan(
                &plan,
                core_transcode::DEFAULT_THREADS,
                None,
                sink,
            )
            .map_err(|e| e.to_string())?;
            // the plan described the library as it was: drop it so a reload
            // can't restore a table of conversions that already happened
            state2.library.lock().transcode = None;
            Ok(json!({
                "converted": summary.converted,
                "skipped": summary.skipped,
                "failed": summary.failed,
                "trashed": summary.trashed,
                "bytes_before": summary.bytes_before,
                "bytes_after": summary.bytes_after,
                "trash": trash,
            }))
        },
    );
    match result {
        Ok(()) => Json(json!({"started": true})).into_response(),
        Err(e) => conflict(e),
    }
}

async fn transcode_plan(State(state): State<Arc<AppState>>) -> Response {
    let plan = state
        .library
        .lock()
        .transcode
        .as_ref()
        .map(transcode_plan_json);
    Json(json!({ "plan": plan })).into_response()
}

fn cover_provider(value: Option<&str>) -> core_cover::Provider {
    match value.unwrap_or("auto") {
        "musicbrainz" => core_cover::Provider::MusicBrainz,
        "itunes" => core_cover::Provider::Itunes,
        _ => core_cover::Provider::Auto,
    }
}

fn cover_path(value: &str) -> PathBuf {
    // Plans store canonical paths, while a browser or curl caller may use a
    // symlinked spelling such as /tmp on macOS. Normalize when possible.
    std::fs::canonicalize(value).unwrap_or_else(|_| PathBuf::from(value))
}

async fn cover_analyze(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let input = body
        .get("input")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let provider = cover_provider(body.get("provider").and_then(|v| v.as_str()));
    let min_confidence = body
        .get("min_confidence")
        .and_then(|v| v.as_f64())
        .unwrap_or(core_cover::DEFAULT_MIN_CONFIDENCE as f64) as f32;
    let country = body
        .get("country")
        .and_then(|v| v.as_str())
        .unwrap_or("US")
        .to_string();
    let overwrite = body
        .get("overwrite")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let state2 = Arc::clone(&state);
    let result = start_job(
        &state,
        JobKind::CoverAnalyze,
        None,
        move |sink: dj_music_core::Sink| {
            let opts = core_cover::CoverOpts {
                input,
                provider,
                min_confidence,
                overwrite,
                country,
                // the web keeps the plan in memory instead of writing a file
                plan: None,
                cache_dir: None,
            };
            let plan = core_cover::analyze(&opts, sink).map_err(|e| e.to_string())?;
            let value = serde_json::to_value(&plan).map_err(|e| e.to_string())?;
            state2.library.lock().cover = Some(plan);
            Ok(value)
        },
    );
    match result {
        Ok(()) => Json(json!({"started": true})).into_response(),
        Err(e) => conflict(e),
    }
}

async fn cover_execute(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(items) = body.get("include").and_then(|v| v.as_array()) else {
        return bad_request("缺少 include（要写入封面的条目列表）");
    };
    let include: BTreeSet<PathBuf> = items
        .iter()
        .filter_map(|v| v.as_str())
        .map(cover_path)
        .collect();
    let mut selections = std::collections::BTreeMap::new();
    if let Some(map) = body.get("selections").and_then(|v| v.as_object()) {
        for (path, value) in map {
            let Some(index) = value.as_u64() else {
                return bad_request(format!("候选序号无效: {path}"));
            };
            selections.insert(cover_path(path), index as usize);
        }
    }
    let plan = state.library.lock().cover.clone();
    let Some(plan) = plan else {
        return bad_request("请先运行封面分析");
    };

    let state2 = Arc::clone(&state);
    let result = start_job(
        &state,
        JobKind::CoverExecute,
        None,
        move |sink: dj_music_core::Sink| {
            let options = core_cover::ExecuteOptions {
                include: Some(include),
                selections,
            };
            let summary = core_cover::execute_with_options(&plan, &options, sink)
                .map_err(|e| e.to_string())?;
            // the plan described the files as they were before writing; a
            // reload should not offer to apply the same rows again
            state2.library.lock().cover = None;
            Ok(json!({
                "embedded": summary.embedded,
                "replaced": summary.replaced,
                "skipped": summary.skipped,
                "failed": summary.failed,
            }))
        },
    );
    match result {
        Ok(()) => Json(json!({"started": true})).into_response(),
        Err(e) => conflict(e),
    }
}

async fn cover_plan(State(state): State<Arc<AppState>>) -> Response {
    let plan = state.library.lock().cover.clone();
    Json(json!({ "plan": plan })).into_response()
}

async fn cover_artwork(
    State(state): State<Arc<AppState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let Some(path) = params.get("path") else {
        return bad_request("缺少 path");
    };
    let Ok(candidate) = params
        .get("candidate")
        .map(String::as_str)
        .unwrap_or("0")
        .parse::<usize>()
    else {
        return bad_request("candidate 必须是数字");
    };
    let plan = state.library.lock().cover.clone();
    let Some(plan) = plan else {
        return (StatusCode::NOT_FOUND, "cover plan not found").into_response();
    };
    let path = cover_path(path);
    let result = tokio::task::spawn_blocking(move || {
        core_cover::preview_artwork(&plan, &path, candidate)
    })
    .await;
    match result {
        Ok(Ok(artwork)) => (
            [
                (header::CONTENT_TYPE, artwork.mime),
                (header::CACHE_CONTROL, "private, max-age=3600"),
            ],
            artwork.bytes,
        )
            .into_response(),
        Ok(Err(e)) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("artwork task failed: {e}"),
        )
            .into_response(),
    }
}

async fn reorg_analyze(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let root = match library_root_or_error(&state) {
        Ok(root) => root,
        Err(resp) => return resp,
    };
    let template = body
        .get("template")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| "{artist}/{filename}.{ext}".to_string());

    let state2 = Arc::clone(&state);
    let result = start_job(
        &state,
        JobKind::ReorgAnalyze,
        None,
        move |sink: dj_music_core::Sink| {
            let opts = core_reorg::ReorgOpts {
                root: Some(root),
                template,
                execute: false,
                from: None,
                allow_rename: false,
                // no plan file for the web UI: the plan stays in memory
                plan: None,
            };
            let plan = core_reorg::analyze(&opts, sink).map_err(|e| e.to_string())?;
            let value = reorg_plan_json(&plan);
            state2.library.lock().reorg = Some(plan);
            Ok(value)
        },
    );
    match result {
        Ok(()) => Json(json!({"started": true})).into_response(),
        Err(e) => conflict(e),
    }
}

async fn reorg_execute(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let Some(items) = body.get("include").and_then(|v| v.as_array()) else {
        return bad_request("缺少 include（要应用的条目列表）");
    };
    let include: BTreeSet<PathBuf> = items
        .iter()
        .filter_map(|v| v.as_str())
        .map(PathBuf::from)
        .collect();
    let plan = state.library.lock().reorg.clone();
    let Some(plan) = plan else {
        return bad_request("请先运行整理分析");
    };

    let state2 = Arc::clone(&state);
    let result = start_job(
        &state,
        JobKind::ReorgExecute,
        None,
        move |sink: dj_music_core::Sink| {
            // apply exactly the ticked rows; renames included, because a tick
            // on a rename row is the explicit opt-in the CLI spells --allow-rename
            let mut plan = plan;
            plan.entries.retain(|e| include.contains(&e.src));
            let summary = core_reorg::execute_plan(&plan, true, sink);
            // the plan described the library as it was: drop it so a reload
            // can't restore a table of moves that already happened
            state2.library.lock().reorg = None;
            Ok(json!({
                "root": summary.root,
                "moved": summary.moved,
                "renamed": summary.renamed,
                "skipped": summary.skipped,
                "deferred_renames": summary.deferred_renames,
                "failed": summary.failed,
            }))
        },
    );
    match result {
        Ok(()) => Json(json!({"started": true})).into_response(),
        Err(e) => conflict(e),
    }
}

async fn reorg_plan(State(state): State<Arc<AppState>>) -> Response {
    let plan = state
        .library
        .lock()
        .reorg
        .as_ref()
        .map(reorg_plan_json);
    Json(json!({ "plan": plan })).into_response()
}

async fn dedup_analyze(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let root = match library_root_or_error(&state) {
        Ok(root) => root,
        Err(resp) => return resp,
    };
    let keep = match body.get("keep").and_then(|v| v.as_str()) {
        Some("first") => core_dedup::KeepMode::First,
        _ => core_dedup::KeepMode::Best,
    };
    // seed from the previous scan so re-running (another keep mode, or a
    // fresher scan) reuses the hashes instead of reading the whole library
    let seed = std::mem::take(&mut state.library.lock().dedup_cache);

    let state2 = Arc::clone(&state);
    let result = start_job(
        &state,
        JobKind::DedupAnalyze,
        None,
        move |sink: dj_music_core::Sink| {
            let opts = core_dedup::DedupOpts {
                root: Some(root),
                execute: false,
                from: None,
                keep,
                trash: None,
                delete: false,
                plan: None,
            };
            let (mut plan, stats) =
                core_dedup::analyze_reusing(&opts, seed, sink).map_err(|e| e.to_string())?;
            let value = dedup_plan_json(&plan, stats);
            let mut library = state2.library.lock();
            // the cache lives on its own so executing (which discards the
            // plan) does not throw away the expensive part
            library.dedup_cache = std::mem::take(&mut plan.cache);
            library.dedup = Some((plan, stats));
            Ok(value)
        },
    );
    match result {
        Ok(()) => Json(json!({"started": true})).into_response(),
        Err(e) => conflict(e),
    }
}

async fn dedup_execute(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let plan = state
        .library
        .lock()
        .dedup
        .as_ref()
        .map(|(plan, _)| plan.clone());
    let Some(mut plan) = plan else {
        return bad_request("请先运行去重分析");
    };

    // "keepers" maps a group index to the file that must survive; groups
    // absent from the map keep the keeper the analysis chose. Resolving it
    // here keeps a bad request a 400 instead of a failed job.
    if let Some(map) = body.get("keepers").and_then(|v| v.as_object()) {
        for (index, value) in map {
            let Ok(index) = index.parse::<usize>() else {
                return bad_request(format!("组号无效: {index}"));
            };
            let Some(path) = value.as_str() else {
                return bad_request("keepers 的值必须是文件路径");
            };
            let Some(group) = plan.groups.get_mut(index) else {
                return bad_request(format!("组号越界: {index}"));
            };
            let mut files: Vec<core_dedup::FileEntry> = std::iter::once(group.keep.clone())
                .chain(group.losers.iter().cloned())
                .collect();
            let Some(position) = files.iter().position(|f| f.path == Path::new(path)) else {
                return bad_request(format!("该文件不在第 {index} 组里: {path}"));
            };
            group.keep = files.remove(position);
            group.losers = files;
        }
    }

    let state2 = Arc::clone(&state);
    let result = start_job(
        &state,
        JobKind::DedupExecute,
        None,
        move |sink: dj_music_core::Sink| {
            // web never deletes outright: losers go to <root>/.dedup-trash
            let trash = plan.root.join(".dedup-trash");
            let summary = core_dedup::execute_plan(&plan, false, None, sink);
            // groups describe the library as it was; the hash cache stays
            state2.library.lock().dedup = None;
            Ok(json!({
                "root": summary.root,
                "removed": summary.removed,
                "skipped": summary.skipped,
                "failed": summary.failed,
                "freed_bytes": summary.freed_bytes,
                "trash": trash,
            }))
        },
    );
    match result {
        Ok(()) => Json(json!({"started": true})).into_response(),
        Err(e) => conflict(e),
    }
}

async fn dedup_plan(State(state): State<Arc<AppState>>) -> Response {
    let plan = state
        .library
        .lock()
        .dedup
        .as_ref()
        .map(|(plan, stats)| dedup_plan_json(plan, *stats));
    Json(json!({ "plan": plan })).into_response()
}

// ---------- trash ----------
//
// The trash folder is written by dedup (CLI and web alike); the manifest it
// records is what makes a file restorable, so listing joins the two and flags
// anything without a record as unknown origin.

/// Absolute trash folder for the configured library root (custom `--trash`
/// locations are a CLI concern).
fn trash_dir_or_error(state: &AppState) -> Result<PathBuf, Response> {
    let root = library_root_or_error(state)?;
    Ok(core_trash::default_dir(&root))
}

fn trash_json(trash: &Path, entry: &core_trash::Entry) -> serde_json::Value {
    json!({
        "rel": entry.rel,
        "kind": entry.kind,
        "size": entry.size,
        "moved_at": entry.moved_at,
        "sha256": entry.sha256,
        "original": entry.original,
        "path": entry.path,
        "restorable": entry.original.is_some(),
        "trash": trash,
    })
}

async fn trash_list(State(state): State<Arc<AppState>>) -> Response {
    let trash = match trash_dir_or_error(&state) {
        Ok(trash) => trash,
        Err(resp) => return resp,
    };
    let root = match library_root_or_error(&state) {
        Ok(root) => root,
        Err(resp) => return resp,
    };
    let entries_list = core_trash::list(&root, &trash);
    let bytes: u64 = entries_list.iter().map(|e| e.size).sum();
    let entries: Vec<serde_json::Value> = entries_list
        .iter()
        .map(|entry| trash_json(&trash, entry))
        .collect();
    Json(json!({
        "root": root,
        "trash": trash,
        "entries": entries,
        "bytes": bytes,
    }))
    .into_response()
}

/// Entry names from the request, rejected unless they name a path inside the
/// trash folder. `clear: true` means "everything currently in the trash".
fn trash_targets(
    state: &AppState,
    body: &serde_json::Value,
) -> Result<(PathBuf, PathBuf, bool, Vec<String>), Response> {
    let root = library_root_or_error(state)?;
    let trash = core_trash::default_dir(&root);
    let clear = body.get("clear").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut entries: Vec<String> = Vec::new();
    if let Some(list) = body.get("entries").and_then(|v| v.as_array()) {
        for value in list {
            let Some(rel) = value.as_str() else {
                return Err(bad_request("entries 必须是字符串数组"));
            };
            if !core_trash::is_safe_rel(rel) {
                return Err(bad_request(format!("条目名无效: {rel}")));
            }
            entries.push(rel.to_string());
        }
    }
    if !clear && entries.is_empty() {
        return Err(bad_request("缺少 entries（要处理的条目），或设 clear 为 true"));
    }
    if clear && !entries.is_empty() {
        return Err(bad_request("entries 与 clear 不能同时给出"));
    }
    Ok((root, trash, clear, entries))
}

async fn trash_restore(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let (root, trash, clear, entries) = match trash_targets(&state, &body) {
        Ok(parts) => parts,
        Err(resp) => return resp,
    };
    // "put everything back" only covers what has a remembered origin: files
    // of unknown origin can't be restored
    let entries = if clear { core_trash::restorable(&root, &trash) } else { entries };
    if entries.is_empty() {
        return bad_request("回收目录里没有可放回的条目");
    }
    let result = start_job(&state, JobKind::TrashRestore, None, move |sink: dj_music_core::Sink| {
        let summary = core_trash::restore(&root, &trash, &entries, sink).map_err(|e| e.to_string())?;
        Ok(json!({
            "restored": summary.restored,
            "skipped": summary.skipped,
            "failed": summary.failed,
            "bytes": summary.bytes,
            "trash": trash,
        }))
    });
    match result {
        Ok(()) => Json(json!({"started": true})).into_response(),
        Err(e) => conflict(e),
    }
}

async fn trash_delete(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let (_root, trash, clear, entries) = match trash_targets(&state, &body) {
        Ok(parts) => parts,
        Err(resp) => return resp,
    };
    let result = start_job(
        &state,
        JobKind::TrashDelete,
        None,
        move |sink: dj_music_core::Sink| {
            let summary = if clear {
                core_trash::clear(&trash, sink)
            } else {
                core_trash::remove(&trash, &entries, sink).map_err(|e| e.to_string())?
            };
            Ok(json!({
                "removed": summary.removed,
                "skipped": summary.skipped,
                "failed": summary.failed,
                "bytes": summary.bytes,
                "trash": trash,
            }))
        },
    );
    match result {
        Ok(()) => Json(json!({"started": true})).into_response(),
        Err(e) => conflict(e),
    }
}

// ---------- job status ----------

async fn job_snapshot(State(state): State<Arc<AppState>>) -> Response {
    let guard = state.job.lock();
    let Some(job) = guard.as_ref() else {
        return Json(json!({"status": "idle"})).into_response();
    };
    Json(json!({
        "kind": job.kind.as_str(),
        "staging_id": job.staging_id,
        "status": job.status.as_str(),
        "events": job.log.snapshot(),
        "result": *job.result.lock(),
        "error": *job.error.lock(),
    }))
    .into_response()
}

/// Streams the current job's events: replays what already happened, then
/// follows live. Ends with a terminal event (done/error) or idle when no job
/// exists at all.
async fn events(State(state): State<Arc<AppState>>) -> impl axum::response::IntoResponse {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<SseEvent, std::convert::Infallible>>(64);
    tokio::spawn(async move {
        let mut sent = 0usize;
        let mut ticks = 0u32;
        loop {
            let snapshot = {
                let guard = state.job.lock();
                match guard.as_ref() {
                    Some(job) => {
                        let all = job.log.snapshot();
                        let fresh = all.into_iter().skip(sent).collect::<Vec<_>>();
                        let status = job.status;
                        let result = if status == JobStatus::Done {
                            job.result.lock().clone()
                        } else {
                            None
                        };
                        let error = if status == JobStatus::Failed {
                            job.error.lock().clone()
                        } else {
                            None
                        };
                        sent += fresh.len();
                        (Some(fresh), Some((status, result, error)))
                    }
                    None => (None, None),
                }
            };
            let (fresh, terminal) = snapshot;
            if let Some(fresh) = fresh {
                for value in fresh {
                    let data = serde_json::to_string(&value).unwrap_or_default();
                    if tx.send(Ok(SseEvent::default().event("job").data(data))).await.is_err() {
                        return; // client went away
                    }
                }
            }
            if let Some((status, result, error)) = terminal {
                let payload = match status {
                    JobStatus::Done => json!({"type": "done", "result": result}),
                    JobStatus::Failed => json!({"type": "error", "message": error.unwrap_or_default()}),
                    JobStatus::Running => {
                        ticks += 1;
                        if ticks % 40 == 0 {
                            if tx.send(Ok(SseEvent::default().comment("keep-alive"))).await.is_err() {
                                return;
                            }
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                        continue;
                    }
                };
                let _ = tx.send(Ok(SseEvent::default().event("end").data(serde_json::to_string(&payload).unwrap_or_default()))).await;
                return;
            }
            // no job at all: tell the client and close; it reopens on demand
            let payload = json!({"type": "idle"});
            let _ = tx.send(Ok(SseEvent::default().event("end").data(serde_json::to_string(&payload).unwrap_or_default()))).await;
            return;
        }
    });
    Sse::new(tokio_stream::wrappers::ReceiverStream::new(rx)).keep_alive(KeepAlive::default())
}

// ---------- staging cleanup ----------

async fn clear_staging(State(state): State<Arc<AppState>>, AxPath(id): AxPath<String>) -> Response {
    let Some(dir) = state.staging_dir(&id).filter(|p| p.is_dir()) else {
        return bad_request(format!("暂存目录不存在: {id}"));
    };
    // never touch files that are still referenced by a running job
    {
        let guard = state.job.lock();
        if let Some(job) = guard.as_ref() {
            if job.status == JobStatus::Running && job.staging_id.as_deref() == Some(id.as_str()) {
                return conflict("该暂存目录正被任务使用");
            }
        }
    }
    state.plans.lock().remove(&id);
    match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => Json(json!({"cleared": true})).into_response(),
        Err(e) => bad_request(format!("清理失败: {e}")),
    }
}
