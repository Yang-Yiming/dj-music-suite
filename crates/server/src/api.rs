use std::path::Path;
use std::sync::Arc;

use axum::extract::{Multipart, Path as AxPath, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use serde_json::json;

use dj_music_core::convert::{self, ConvertOpts};
use dj_music_core::import::{self as core_import, Mode};

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
    let config = state.config.lock().unwrap();
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
        let mut config = state.config.lock().unwrap();
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
        state2.plans.lock().unwrap().insert(sid, plan.clone());
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
    let plan = state.plans.lock().unwrap().get(&staging_id).cloned();
    let Some(mut plan) = plan else {
        return bad_request("请先运行导入分析");
    };
    if let Some(include) = &include {
        plan.items
            .retain(|item| include.contains(&item.src.to_string_lossy().into_owned()));
    }

    let result = start_job(&state, JobKind::ImportExecute, Some(staging_id), move |sink: dj_music_core::Sink| {
        let summary = core_import::execute(&plan, mode, overwrite, sink);
        Ok(json!({"placed": summary.placed, "failed": summary.failed}))
    });
    match result {
        Ok(()) => Json(json!({"started": true})).into_response(),
        Err(e) => conflict(e),
    }
}

// ---------- job status ----------

async fn job_snapshot(State(state): State<Arc<AppState>>) -> Response {
    let guard = state.job.lock().unwrap();
    let Some(job) = guard.as_ref() else {
        return Json(json!({"status": "idle"})).into_response();
    };
    Json(json!({
        "kind": job.kind.as_str(),
        "staging_id": job.staging_id,
        "status": job.status.as_str(),
        "events": job.log.snapshot(),
        "result": *job.result.lock().unwrap(),
        "error": *job.error.lock().unwrap(),
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
                let guard = state.job.lock().unwrap();
                match guard.as_ref() {
                    Some(job) => {
                        let all = job.log.snapshot();
                        let fresh = all.into_iter().skip(sent).collect::<Vec<_>>();
                        let status = job.status;
                        let result = if status == JobStatus::Done {
                            job.result.lock().unwrap().clone()
                        } else {
                            None
                        };
                        let error = if status == JobStatus::Failed {
                            job.error.lock().unwrap().clone()
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
        let guard = state.job.lock().unwrap();
        if let Some(job) = guard.as_ref() {
            if job.status == JobStatus::Running && job.staging_id.as_deref() == Some(id.as_str()) {
                return conflict("该暂存目录正被任务使用");
            }
        }
    }
    state.plans.lock().unwrap().remove(&id);
    match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => Json(json!({"cleared": true})).into_response(),
        Err(e) => bad_request(format!("清理失败: {e}")),
    }
}
