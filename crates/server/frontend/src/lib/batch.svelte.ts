// Shared batch state (one upload batch flows through convert → import) and
// the actions that drive it. Components read `batch` directly and call the
// exported actions.

import {
  clearStaging,
  defaultIncluded,
  getJob,
  isReplacement,
  startAnalyze,
  startConvert,
  startExecute,
  upload,
  useFolder,
  type ConvertResult,
  type ExecuteResult,
  type ImportPlan,
  type JobSnapshot,
  type UploadResult,
} from "./api";
import { job, resumeRunningJob, watchJob } from "./job.svelte";

export const batch = $state<{
  stagingId: string | null;
  /** registered local folder (read in place, not uploaded) */
  fromFolder: string | null;
  counts: { ncm: number; audio: number; lyrics: number; image: number; other: number };
  uploading: boolean;
  uploadError: string;
  convertDone: ConvertResult | null;
  convertSkipped: boolean;
  convertError: string;
  plan: ImportPlan | null;
  decisions: Record<string, boolean>;
  analyzeError: string;
  analyzeWaiting: boolean;
  mode: "copy" | "move";
  executeResult: ExecuteResult | null;
  executeError: string;
  executing: boolean;
  clearing: boolean;
}>({
  stagingId: null,
  fromFolder: null,
  counts: { ncm: 0, audio: 0, lyrics: 0, image: 0, other: 0 },
  uploading: false,
  uploadError: "",
  convertDone: null,
  convertSkipped: false,
  convertError: "",
  plan: null,
  decisions: {},
  analyzeError: "",
  analyzeWaiting: false,
  mode: "copy",
  executeResult: null,
  executeError: "",
  executing: false,
  clearing: false,
});

function applyUpload(result: UploadResult) {
  batch.stagingId = result.staging_id;
  batch.fromFolder = null;
  batch.counts = { ncm: 0, audio: 0, lyrics: 0, image: 0, other: 0 };
  for (const f of result.files) batch.counts[f.kind] += 1;
  batch.convertDone = null;
  batch.convertSkipped = false;
  batch.convertError = "";
  batch.plan = null;
  batch.decisions = {};
  batch.executeResult = null;
  batch.executeError = "";
}

export async function uploadFiles(files: { file: File; path: string }[]) {
  if (!files.length || job.running) return;
  batch.uploading = true;
  batch.uploadError = "";
  try {
    applyUpload(await upload(files));
  } catch (e) {
    batch.uploadError = e instanceof Error ? e.message : String(e);
  } finally {
    batch.uploading = false;
  }
}

export async function importFolder(path: string) {
  if (job.running) return;
  batch.uploading = true;
  batch.uploadError = "";
  try {
    applyUpload(await useFolder(path));
    batch.fromFolder = path;
  } catch (e) {
    batch.uploadError = e instanceof Error ? e.message : String(e);
  } finally {
    batch.uploading = false;
  }
}

export async function doConvert(noDownload: boolean) {
  if (!batch.stagingId) return;
  batch.convertError = "";
  try {
    await startConvert(batch.stagingId, noDownload);
    const end = await watchJob("convert");
    if (end.type === "done") {
      batch.convertDone = end.result as ConvertResult;
    } else {
      batch.convertError = end.type === "error" ? end.message : "没有收到转换任务";
    }
  } catch (e) {
    batch.convertError = e instanceof Error ? e.message : String(e);
  }
}

export function decide(src: string, include: boolean) {
  batch.decisions[src] = include;
}

export async function doAnalyze(template: string) {
  if (!batch.stagingId) return;
  batch.analyzeError = "";
  batch.analyzeWaiting = true;
  batch.plan = null;
  batch.decisions = {};
  try {
    await startAnalyze(batch.stagingId, template.trim() || "{artist}/{filename}.{ext}");
    const end = await watchJob("import-analyze");
    if (end.type === "done") {
      batch.plan = end.result as ImportPlan;
    } else {
      batch.analyzeError = end.type === "error" ? end.message : "没有收到分析任务";
    }
  } catch (e) {
    batch.analyzeError = e instanceof Error ? e.message : String(e);
  } finally {
    batch.analyzeWaiting = false;
  }
}

export async function doExecute() {
  if (!batch.stagingId || !batch.plan) return;
  batch.executing = true;
  batch.executeError = "";
  try {
    const included = batch.plan.items.filter(
      (i) => batch.decisions[i.src] ?? defaultIncluded(i),
    );
    const overwrite = included.some(isReplacement);
    await startExecute(
      batch.stagingId,
      batch.mode,
      overwrite,
      included.map((i) => i.src),
    );
    const end = await watchJob("import-execute");
    if (end.type === "done") {
      batch.executeResult = end.result as ExecuteResult;
    } else {
      batch.executeError = end.type === "error" ? end.message : "没有收到导入任务";
    }
  } catch (e) {
    batch.executeError = e instanceof Error ? e.message : String(e);
  } finally {
    batch.executing = false;
  }
}

export async function resetBatch() {
  if (!batch.stagingId) return;
  batch.clearing = true;
  try {
    await clearStaging(batch.stagingId);
  } catch {
    // staging cleanup is best-effort
  }
  clearLocalBatch();
  batch.clearing = false;
}

function clearLocalBatch() {
  batch.stagingId = null;
  batch.fromFolder = null;
  batch.counts = { ncm: 0, audio: 0, lyrics: 0, image: 0, other: 0 };
  batch.convertDone = null;
  batch.convertSkipped = false;
  batch.convertError = "";
  batch.plan = null;
  batch.decisions = {};
  batch.executeResult = null;
  batch.executeError = "";
}

function applyEnd(kind: string, end: { type: string; result?: unknown; message?: string }) {
  if (end.type !== "done") {
    const message = end.message ?? "任务失败";
    if (kind === "convert") batch.convertError = message;
    else if (kind === "import-analyze") batch.analyzeError = message;
    else if (kind === "import-execute") batch.executeError = message;
    return;
  }
  if (kind === "convert") batch.convertDone = end.result as ConvertResult;
  else if (kind === "import-analyze") batch.plan = end.result as ImportPlan;
  else if (kind === "import-execute") batch.executeResult = end.result as ExecuteResult;
}

/// Restore state after a page reload. Returns the tab that owns the
/// restored job, if any.
export async function restore(): Promise<"convert" | "import" | null> {
  let snapshot: JobSnapshot | { status: "idle" };
  try {
    snapshot = await getJob();
  } catch {
    return null;
  }
  if (!("kind" in snapshot)) return null;
  if (snapshot.staging_id) batch.stagingId = snapshot.staging_id;
  if (snapshot.status === "running") {
    const kind = snapshot.kind;
    if (kind === "import-analyze") batch.analyzeWaiting = true;
    if (kind === "import-execute") batch.executing = true;
    const end = await resumeRunningJob(snapshot);
    batch.analyzeWaiting = false;
    batch.executing = false;
    applyEnd(kind, end);
    return kind === "convert" ? "convert" : "import";
  }
  if (snapshot.status === "done" && snapshot.staging_id) {
    applyEnd(snapshot.kind, { type: "done", result: snapshot.result });
    return snapshot.kind === "convert" ? "convert" : "import";
  }
  return null;
}
