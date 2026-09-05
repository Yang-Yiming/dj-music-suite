use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dj_music_core::config::{self, Config, Paths};
use dj_music_core::import::ImportPlan;

/// One job at a time (the core operations take over whole staging dirs and
/// the library index; parallel jobs would just fight each other).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Convert,
    ImportAnalyze,
    ImportExecute,
}

impl JobKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobKind::Convert => "convert",
            JobKind::ImportAnalyze => "import-analyze",
            JobKind::ImportExecute => "import-execute",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Running,
    Done,
    Failed,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
        }
    }
}

/// Shared log of the running/last job. Core events are serialized as they
/// arrive so SSE readers can replay from any index.
#[derive(Default)]
pub struct JobLog {
    pub events: Mutex<Vec<serde_json::Value>>,
}

impl JobLog {
    fn push(&self, value: serde_json::Value) {
        self.events.lock().unwrap().push(value);
    }

    pub fn snapshot(&self) -> Vec<serde_json::Value> {
        self.events.lock().unwrap().clone()
    }
}

pub struct Job {
    pub kind: JobKind,
    pub staging_id: Option<String>,
    pub status: JobStatus,
    pub log: Arc<JobLog>,
    /// JSON payload published when the job finishes (summary or plan)
    pub result: Mutex<Option<serde_json::Value>>,
    pub error: Mutex<Option<String>>,
}

pub struct AppState {
    pub paths: Paths,
    pub config: Mutex<Config>,
    pub job: Mutex<Option<Job>>,
    /// analyzed import plans by staging id, consumed by import/execute
    pub plans: Mutex<HashMap<String, ImportPlan>>,
}

impl AppState {
    pub fn new(paths: Paths) -> Self {
        let config = Mutex::new(config::load(&paths.config_file));
        AppState {
            paths,
            config,
            job: Mutex::new(None),
            plans: Mutex::new(HashMap::new()),
        }
    }

    pub fn library_root(&self) -> Option<PathBuf> {
        self.config
            .lock()
            .unwrap()
            .library_root
            .as_ref()
            .map(PathBuf::from)
    }

    pub fn staging_dir(&self, id: &str) -> Option<PathBuf> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return None;
        }
        Some(self.paths.staging_root.join(id))
    }
}

/// Spawn a background job; fails when another one is still running. The
/// runner receives an event sink wired into the job log.
pub fn start_job(
    state: &Arc<AppState>,
    kind: JobKind,
    staging_id: Option<String>,
    run: impl FnOnce(dj_music_core::Sink) -> Result<serde_json::Value, String> + Send + 'static,
) -> Result<(), String> {
    let mut slot = state.job.lock().unwrap();
    if slot.as_ref().is_some_and(|j| j.status == JobStatus::Running) {
        return Err("有任务正在运行，请等待它完成".to_string());
    }
    // log lines carry full filesystem paths from core; replace the staging
    // prefix so the web log stays readable (library paths stay untouched)
    let staging_prefix = staging_id
        .as_ref()
        .and_then(|id| state.staging_dir(id))
        .map(|p| p.to_string_lossy().into_owned());
    let log = Arc::new(JobLog::default());
    *slot = Some(Job {
        staging_id,
        kind,
        status: JobStatus::Running,
        log: Arc::clone(&log),
        result: Mutex::new(None),
        error: Mutex::new(None),
    });
    drop(slot);

    let state = Arc::clone(state);
    std::thread::spawn(move || {
        let sink = move |event: &dj_music_core::Event| {
            log.push(event_json(&shorten_event(event, staging_prefix.as_deref())));
        };
        let outcome = run(&sink);
        let mut slot = state.job.lock().unwrap();
        if let Some(job) = slot.as_mut() {
            match outcome {
                Ok(result) => {
                    *job.result.lock().unwrap() = Some(result);
                    job.status = JobStatus::Done;
                }
                Err(message) => {
                    *job.error.lock().unwrap() = Some(message);
                    job.status = JobStatus::Failed;
                }
            }
        }
    });
    Ok(())
}

fn shorten_event(
    event: &dj_music_core::Event,
    staging_prefix: Option<&str>,
) -> dj_music_core::Event {
    use dj_music_core::Event;
    let Some(prefix) = staging_prefix else {
        return event.clone();
    };
    let shorten = |text: &str| text.replace(&prefix, "~");
    match event {
        Event::Line(text) => Event::Line(shorten(text)),
        Event::Warn(text) => Event::Warn(shorten(text)),
        other => other.clone(),
    }
}

pub fn event_json(event: &dj_music_core::Event) -> serde_json::Value {
    use dj_music_core::Event;
    match event {
        Event::Start(total) => serde_json::json!({"type": "start", "total": total}),
        Event::Step(name) => serde_json::json!({"type": "step", "name": name}),
        Event::Line(text) => serde_json::json!({"type": "line", "text": text}),
        Event::Warn(text) => serde_json::json!({"type": "warn", "text": text}),
    }
}
