/* dj-music-suite web UI */

const state = {
  stagingId: null,
  fileCounts: { ncm: 0, audio: 0, other: 0 },
  eventSource: null,
};

const $ = (id) => document.getElementById(id);

// ---------- helpers ----------

async function api(path, options) {
  const resp = await fetch(path, options);
  let body = null;
  try { body = await resp.json(); } catch { /* empty body is fine */ }
  if (!resp.ok) {
    throw new Error((body && body.error) || `请求失败 (${resp.status})`);
  }
  return body;
}

function setStatus(id, text, cls) {
  const el = $(id);
  el.textContent = text || "";
  el.className = "status" + (cls ? " " + cls : "");
}

function setProgress(id, current, total) {
  const el = $(id);
  if (total > 0) {
    el.hidden = false;
    el.querySelector(".bar").style.width = `${Math.min(100, (current / total) * 100)}%`;
    el.querySelector(".count").textContent = `${current} / ${total}`;
  } else {
    el.hidden = false;
    el.querySelector(".count").textContent = `${current} / ?`;
  }
}

function hideProgress(id) {
  $(id).hidden = true;
}

function appendLog(id, text, warn) {
  const el = $(id);
  const line = document.createElement("span");
  if (warn) line.className = "warn";
  line.textContent = text + "\n";
  el.appendChild(line);
  while (el.childNodes.length > 300) el.removeChild(el.firstChild);
  el.scrollTop = el.scrollHeight;
}

function clearLog(id) {
  $(id).textContent = "";
}

const DISPOSITION_LABELS = {
  "new": "新增",
  "alt-version": "替代版本",
  "duplicate": "重复",
  "conflict": "冲突",
  "untagged": "缺标签",
};

function refreshButtons() {
  $("btn-convert").disabled = state.stagingId === null || state.fileCounts.ncm === 0;
  $("btn-analyze").disabled = state.stagingId === null;
}

// ---------- SSE job stream ----------

function watchJob(kind, { onEvent, onDone, onError }) {
  if (state.eventSource) state.eventSource.close();
  clearLog(kind === "convert" ? "convert-log" : "execute-log");
  let current = 0;
  const progressId = kind === "convert" ? "convert-progress"
    : kind === "import-analyze" ? "analyze-progress" : "execute-progress";

  const es = new EventSource("/api/events");
  state.eventSource = es;
  let total = 0;

  es.addEventListener("job", (e) => {
    const ev = JSON.parse(e.data);
    switch (ev.type) {
      case "start":
        total = ev.total;
        current = 0;
        setProgress(progressId, 0, total);
        break;
      case "step":
        current += 1;
        setProgress(progressId, current, total);
        if (onEvent) onEvent(ev);
        break;
      case "line":
        appendLog(kind === "convert" ? "convert-log" : "execute-log", ev.text);
        if (onEvent) onEvent(ev);
        break;
      case "warn":
        appendLog(kind === "convert" ? "convert-log" : "execute-log", ev.text, true);
        break;
    }
  });

  es.addEventListener("end", (e) => {
    es.close();
    state.eventSource = null;
    hideProgress(progressId);
    const payload = JSON.parse(e.data);
    if (payload.type === "done") {
      onDone && onDone(payload.result);
    } else if (payload.type === "error") {
      onError && onError(payload.message || "任务失败");
    }
    refreshButtons();
  });
}

// ---------- library config ----------

async function loadConfig() {
  try {
    const cfg = await api("/api/config");
    if (cfg.library_root) {
      $("library-root").value = cfg.library_root;
      setStatus("library-status", "✓ 曲库已设置", "ok");
    } else {
      setStatus("library-status", "尚未设置曲库位置", "err");
    }
  } catch (e) {
    setStatus("library-status", e.message, "err");
  }
}

async function saveConfig() {
  try {
    const cfg = await api("/api/config", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ library_root: $("library-root").value.trim() }),
    });
    $("library-root").value = cfg.library_root;
    setStatus("library-status", "✓ 已保存", "ok");
  } catch (e) {
    setStatus("library-status", e.message, "err");
  }
}

// ---------- upload ----------

async function uploadFiles(fileList) {
  const files = Array.from(fileList);
  if (!files.length) return;
  setStatus("upload-status", `正在上传 ${files.length} 个文件…`);
  const form = new FormData();
  for (const f of files) form.append("files", f, f.name);
  try {
    const result = await api("/api/upload", { method: "POST", body: form });
    state.stagingId = result.staging_id;
    state.fileCounts = { ncm: 0, audio: 0, other: 0 };
    for (const f of result.files) state.fileCounts[f.kind] += 1;
    const parts = [];
    if (state.fileCounts.ncm) parts.push(`${state.fileCounts.ncm} 个 NCM`);
    if (state.fileCounts.audio) parts.push(`${state.fileCounts.audio} 个音频`);
    if (state.fileCounts.other) parts.push(`${state.fileCounts.other} 个其它`);
    setStatus("upload-status", `✓ 已接收：${parts.join("、")}`, "ok");
    refreshButtons();
  } catch (e) {
    setStatus("upload-status", e.message, "err");
  }
}

// ---------- convert ----------

function startConvert() {
  if (!state.stagingId) return;
  $("btn-convert").disabled = true;
  setStatus("upload-status", "", "");
  const body = {
    staging_id: state.stagingId,
    no_download: $("no-download").checked,
  };
  api("/api/convert", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  }).then(() => {
    watchJob("convert", {
      onDone: (result) => {
        setStatus("convert-status-ok", "", "");
        appendLog("convert-log", `完成：转换 ${result.total - result.failed} 个，写标签 ${result.tagged} 个，失败 ${result.failed} 个`);
        if (result.failed === 0) {
          setStatus("upload-status", `✓ 转换完成，可以继续第 4 步导入`, "ok");
        } else {
          setStatus("upload-status", `转换完成但有 ${result.failed} 个失败，详见日志`, "err");
        }
      },
      onError: (msg) => setStatus("upload-status", msg, "err"),
    });
  }).catch((e) => {
    $("btn-convert").disabled = false;
    setStatus("upload-status", e.message, "err");
  });
}

// ---------- import ----------

function startAnalyze() {
  if (!state.stagingId) return;
  $("btn-analyze").disabled = true;
  $("plan-area").hidden = true;
  const body = { staging_id: state.stagingId, template: $("template").value.trim() };
  api("/api/import/analyze", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  }).then(() => {
    watchJob("import-analyze", {
      onDone: (plan) => renderPlan(plan),
      onError: (msg) => {
        setStatus("plan-summary", msg, "err");
        refreshButtons();
      },
    });
  }).catch((e) => {
    refreshButtons();
    setStatus("plan-summary", e.message, "err");
  });
}

function renderPlan(plan) {
  const tbody = $("plan-table").querySelector("tbody");
  tbody.textContent = "";
  const counts = {};
  const important = [];
  for (const item of plan.items) {
    counts[item.disposition] = (counts[item.disposition] || 0) + 1;
    if (item.disposition !== "new") important.push(item);
  }
  for (const item of important) {
    const tr = document.createElement("tr");
    const name = document.createElement("td");
    name.textContent = item.src.split("/").pop();
    const badge = document.createElement("td");
    const span = document.createElement("span");
    span.className = "badge " + item.disposition;
    span.textContent = DISPOSITION_LABELS[item.disposition] || item.disposition;
    badge.appendChild(span);
    const note = document.createElement("td");
    note.className = "note";
    note.textContent = item.note || "";
    tr.append(name, badge, note);
    tbody.appendChild(tr);
  }
  const parts = [];
  for (const [key, label] of Object.entries(DISPOSITION_LABELS)) {
    if (counts[key]) parts.push(`${label} ${counts[key]}`);
  }
  setStatus("plan-summary", parts.join(" · "));
  $("plan-area").hidden = false;
  $("btn-execute").disabled = false;
}

function startExecute() {
  const mode = document.querySelector('input[name="mode"]:checked').value;
  const body = {
    staging_id: state.stagingId,
    mode,
    overwrite: $("overwrite").checked,
  };
  $("btn-execute").disabled = true;
  api("/api/import/execute", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  }).then(() => {
    watchJob("import-execute", {
      onDone: (result) => {
        setStatus("import-result",
          `✓ 完成：写入 ${result.placed} 个，失败 ${result.failed} 个。到 rekordbox 里导入/刷新曲库根目录即可看到新歌。`,
          result.failed ? "err" : "ok");
        $("btn-clear").hidden = false;
      },
      onError: (msg) => {
        setStatus("import-result", msg, "err");
        $("btn-execute").disabled = false;
      },
    });
  }).catch((e) => {
    $("btn-execute").disabled = false;
    setStatus("import-result", e.message, "err");
  });
}

async function clearStaging() {
  if (!state.stagingId) return;
  try {
    await api(`/api/staging/${state.stagingId}`, { method: "DELETE" });
    state.stagingId = null;
    state.fileCounts = { ncm: 0, audio: 0, other: 0 };
    setStatus("upload-status", "暂存文件已清理", "ok");
    $("plan-area").hidden = true;
    $("btn-clear").hidden = true;
    refreshButtons();
  } catch (e) {
    setStatus("upload-status", e.message, "err");
  }
}

// ---------- reload recovery ----------

async function restoreJob() {
  try {
    const job = await api("/api/job");
    if (job.status === "done" && job.kind === "import-analyze" && job.result) {
      renderPlan(job.result);
    } else if (job.status === "running") {
      setStatus("upload-status", "有一个任务正在进行，等待它完成…");
      $("btn-convert").disabled = true;
      $("btn-analyze").disabled = true;
    }
  } catch { /* fresh start */ }
}

// ---------- wiring ----------

function init() {
  $("save-library").addEventListener("click", saveConfig);

  const dz = $("dropzone");
  const input = $("file-input");
  dz.addEventListener("click", () => input.click());
  dz.querySelector(".browse").addEventListener("click", (e) => e.stopPropagation());
  input.addEventListener("change", () => uploadFiles(input.files));
  dz.addEventListener("dragover", (e) => { e.preventDefault(); dz.classList.add("drag"); });
  dz.addEventListener("dragleave", () => dz.classList.remove("drag"));
  dz.addEventListener("drop", (e) => {
    e.preventDefault();
    dz.classList.remove("drag");
    uploadFiles(e.dataTransfer.files);
  });

  $("btn-convert").addEventListener("click", startConvert);
  $("btn-analyze").addEventListener("click", startAnalyze);
  $("btn-execute").addEventListener("click", startExecute);
  $("btn-clear").addEventListener("click", clearStaging);

  loadConfig();
  restoreJob();
}

init();
