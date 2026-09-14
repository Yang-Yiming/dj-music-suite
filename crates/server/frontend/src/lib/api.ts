// Typed client for the dj-music-suite-web HTTP API.

export type Disposition = "new" | "alt-version" | "duplicate" | "conflict" | "untagged";

export interface ImportItem {
  src: string;
  dst?: string;
  replace?: string;
  disposition: Disposition;
  note?: string;
}

export interface ImportPlan {
  input: string;
  root: string;
  template: string;
  items: ImportItem[];
}

export type FileKind = "ncm" | "audio" | "lyrics" | "image" | "other";

export interface UploadedFile {
  name: string;
  kind: FileKind;
}

export interface UploadResult {
  staging_id: string;
  files: UploadedFile[];
}

export interface ConvertResult {
  total: number;
  tagged: number;
  failed: number;
  output: string;
}

export interface ExecuteResult {
  placed: number;
  failed: number;
}

export type JobEvent =
  | { type: "start"; total: number }
  | { type: "step"; name: string }
  | { type: "tick"; percent: number }
  | { type: "line"; text: string }
  | { type: "warn"; text: string };

export type EndPayload =
  | { type: "done"; result: unknown }
  | { type: "error"; message: string }
  | { type: "idle" };

export interface JobSnapshot {
  kind: JobKind;
  staging_id: string | null;
  status: "running" | "done" | "failed";
  events: JobEvent[];
  result: unknown;
  error: string | null;
}

export type JobKind =
  | "convert"
  | "import-analyze"
  | "import-execute"
  | "reorg-analyze"
  | "reorg-execute"
  | "dedup-analyze"
  | "dedup-execute"
  | "transcode-analyze"
  | "transcode-execute"
  | "trash-restore"
  | "trash-delete";

// ---------- reorg ----------

/** "move", "rename", "move+rename" or "none" */
export type ReorgOp = "move" | "rename" | "move+rename" | "none";
/** "ready", "in-place", "conflict" or "untagged" */
export type ReorgStatus = "ready" | "in-place" | "conflict" | "untagged";

export interface ReorgEntry {
  src: string;
  dst?: string;
  op: ReorgOp;
  status: ReorgStatus;
  note?: string;
}

export interface ReorgDupeFile {
  path: string;
  duration_secs?: number;
  size?: number;
}

export interface ReorgDupeGroup {
  artist: string;
  title: string;
  files: ReorgDupeFile[];
}

export interface ReorgPlan {
  root: string;
  template: string;
  entries: ReorgEntry[];
  duplicates: ReorgDupeGroup[];
}

export interface ReorgResult {
  root: string;
  moved: number;
  renamed: number;
  skipped: number;
  deferred_renames: number;
  failed: number;
}

// ---------- dedup ----------

export interface DedupScore {
  format_rank: number;
  bitrate?: number;
  sample_rate?: number;
  bits_per_sample?: number;
  has_cover: boolean;
  has_lyrics: boolean;
  tags_complete: boolean;
  score: number;
}

export interface DedupFile {
  path: string;
  size: number;
  mtime_secs: number;
  sha256: string;
  score: DedupScore;
  duration_secs?: number;
  identity?: [string, string];
}

export interface DedupGroup {
  /** "identical" (same content hash) or "same-identity" */
  kind: "identical" | "same-identity";
  identity?: [string, string];
  keep: DedupFile;
  losers: DedupFile[];
}

export interface DedupPlan {
  root: string;
  groups: DedupGroup[];
  stats: { scanned: number; unreadable: number };
}

export interface DedupResult {
  root: string;
  removed: number;
  skipped: number;
  failed: number;
  freed_bytes: number;
  trash: string;
}

// ---------- trash ----------

export interface TrashEntry {
  /** key for restore/delete: path inside the trash folder */
  rel: string;
  kind: string;
  size: number;
  moved_at?: number;
  sha256?: string;
  /** where it came from; absent for entries with no manifest record */
  original?: string;
  path: string;
  restorable: boolean;
}

export interface TrashListing {
  root: string;
  trash: string;
  entries: TrashEntry[];
  bytes: number;
}

export interface TrashResult {
  restored?: number;
  removed?: number;
  skipped: number;
  failed: number;
  bytes: number;
  trash: string;
}

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
  }
}

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  const resp = await fetch(path, init);
  let body: unknown = null;
  try {
    body = await resp.json();
  } catch {
    // empty body is fine
  }
  if (!resp.ok) {
    const message =
      body && typeof body === "object" && "error" in body
        ? String((body as { error: unknown }).error)
        : `请求失败 (${resp.status})`;
    throw new ApiError(resp.status, message);
  }
  return body as T;
}

const jsonHeaders = { "Content-Type": "application/json" };

export const getConfig = () =>
  call<{ library_root: string | null }>("/api/config");

export const setConfig = (library_root: string) =>
  call<{ library_root: string }>("/api/config", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ library_root }),
  });

export const upload = (files: { file: File; path: string }[]) => {
  const form = new FormData();
  // relative paths (folder structure) ride along in one JSON field; the
  // server pairs them with the file parts by order
  form.append("paths", JSON.stringify(files.map((f) => f.path)));
  for (const f of files) {
    form.append("file", f.file, f.path.split("/").pop() ?? f.path);
  }
  return call<UploadResult>("/api/upload", { method: "POST", body: form });
};

export const detectNetease = () =>
  call<{ path: string | null; count?: number }>("/api/detect-netease");

export const useFolder = (path: string) =>
  call<UploadResult>("/api/staging/from-folder", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ path }),
  });

export const startConvert = (stagingId: string, noDownload: boolean) =>
  call<{ started: boolean }>("/api/convert", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ staging_id: stagingId, no_download: noDownload }),
  });

export const startAnalyze = (stagingId: string, template: string) =>
  call<{ started: boolean }>("/api/import/analyze", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ staging_id: stagingId, template }),
  });

export const startExecute = (
  stagingId: string,
  mode: "copy" | "move",
  overwrite: boolean,
  include: string[] | null,
) =>
  call<{ started: boolean }>("/api/import/execute", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({
      staging_id: stagingId,
      mode,
      overwrite,
      include,
    }),
  });

export const getJob = () =>
  call<JobSnapshot | { status: "idle" }>("/api/job");

// ---------- reorg / dedup ----------

export const startReorgAnalyze = (template: string) =>
  call<{ started: boolean }>("/api/reorg/analyze", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ template }),
  });

/// `include` = the src paths ticked in the preview table; the server applies
/// exactly those (and never anything else).
export const startReorgExecute = (include: string[]) =>
  call<{ started: boolean }>("/api/reorg/execute", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ include }),
  });

export const getReorgPlan = () =>
  call<{ plan: ReorgPlan | null }>("/api/reorg/plan");

export const startDedupAnalyze = (keep: "best" | "first") =>
  call<{ started: boolean }>("/api/dedup/analyze", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ keep }),
  });

/// `keepers` maps a group index to the path that must survive; groups left
/// out keep the keeper the analysis chose.
export const startDedupExecute = (keepers: Record<number, string>) =>
  call<{ started: boolean }>("/api/dedup/execute", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ keepers }),
  });

export const getDedupPlan = () =>
  call<{ plan: DedupPlan | null }>("/api/dedup/plan");

// ---------- transcode ----------

export type TranscodeTarget = "mp3" | "flac" | "m4a" | "alac" | "wav";

/** "ready", "same-codec", "conflict" or "needs-replace" */
export type TranscodeStatus = "ready" | "same-codec" | "conflict" | "needs-replace";

export interface TranscodeEntry {
  src: string;
  dst?: string;
  /** "transcode", or "replace" when the destination is the source itself */
  op: string;
  status: TranscodeStatus;
  src_codec?: string;
  src_bit_depth?: number;
  duration_secs?: number;
  size: number;
  note?: string;
}

export interface TranscodePlan {
  input: string;
  output?: string;
  target: TranscodeTarget;
  quality?: string;
  replace: boolean;
  trash: string;
  entries: TranscodeEntry[];
}

export interface TranscodeResult {
  converted: number;
  skipped: number;
  failed: number;
  trashed: number;
  bytes_before: number;
  bytes_after: number;
  trash: string;
}

export interface FfmpegTarget {
  target: TranscodeTarget;
  label: string;
  ok: boolean;
  missing: string[];
}

export interface FfmpegStatus {
  available: boolean;
  version?: string;
  path?: string;
  error?: string;
  targets?: FfmpegTarget[];
}

export const startTranscodeAnalyze = (opts: {
  to: TranscodeTarget;
  quality: string;
  /** a folder inside the library root, or "" for "next to each source" */
  output: string;
  replace: boolean;
}) =>
  call<{ started: boolean }>("/api/transcode/analyze", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify(opts),
  });

/** `include` = the src paths ticked in the preview table. */
export const startTranscodeExecute = (include: string[]) =>
  call<{ started: boolean }>("/api/transcode/execute", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ include }),
  });

export const getTranscodePlan = () =>
  call<{ plan: TranscodePlan | null }>("/api/transcode/plan");

export const getFfmpeg = () => call<FfmpegStatus>("/api/transcode/ffmpeg");

// ---------- trash ----------

export const getTrash = () => call<TrashListing>("/api/trash");

/// `clear` restores everything at once (unknown-origin files are skipped);
/// otherwise only the listed entry names are touched.
export const restoreTrash = (entries: string[], clear: boolean) =>
  call<{ started: boolean }>("/api/trash/restore", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ entries, clear }),
  });

export const deleteTrash = (entries: string[], clear: boolean) =>
  call<{ started: boolean }>("/api/trash/delete", {
    method: "POST",
    headers: jsonHeaders,
    body: JSON.stringify({ entries, clear }),
  });

export const TRASH_KIND_LABELS: Record<string, string> = {
  identical: "内容相同",
  "same-identity": "同曲异版",
};

export const formatAge = (movedAt?: number, now = Date.now() / 1000): string => {
  if (!movedAt) return "—";
  const days = Math.max(0, Math.floor((now - movedAt) / 86400));
  if (days === 0) return "今天";
  if (days === 1) return "昨天";
  return `${days} 天前`;
};

export const REORG_STATUS_LABELS: Record<ReorgStatus, string> = {
  "ready": "待处理",
  "in-place": "已就位",
  "conflict": "目标已存在",
  "untagged": "缺标签",
};

export const REORG_OP_LABELS: Record<ReorgOp, string> = {
  "move": "移动",
  "rename": "重命名",
  "move+rename": "移动+重命名",
  "none": "不动",
};

export const DEDUP_KIND_LABELS: Record<DedupGroup["kind"], string> = {
  identical: "内容相同",
  "same-identity": "同曲异版",
};

export const TRANSCODE_TARGET_LABELS: Record<TranscodeTarget, string> = {
  mp3: "mp3（有损，兼容性最好）",
  flac: "flac（无损，体积大）",
  m4a: "m4a / AAC（有损）",
  alac: "m4a / ALAC（无损）",
  wav: "wav（无损，无封面）",
};

/// Encoder quality each target defaults to, mirroring core's defaults.
export const TRANSCODE_QUALITY_HINT: Record<TranscodeTarget, string> = {
  mp3: "v0（默认）或 320k",
  flac: "0-12，默认 8",
  m4a: "256k（默认）",
  alac: "无此参数",
  wav: "无此参数",
};

export const TRANSCODE_STATUS_LABELS: Record<TranscodeStatus, string> = {
  "ready": "待转换",
  "same-codec": "已是目标格式",
  "conflict": "目标已存在",
  "needs-replace": "需要「转完后移入回收站」",
};

/// Rows the reorg table ticks by default: only what can actually move, and
/// never a rename (rekordbox relinks by file name, so renaming forces a
/// manual relink). Renames become available once ticked explicitly.
export const reorgDefaultIncluded = (entry: ReorgEntry): boolean =>
  entry.status === "ready" && entry.op === "move";

export const reorgIsRename = (entry: ReorgEntry): boolean =>
  entry.op === "rename" || entry.op === "move+rename";

export const formatBytes = (bytes: number): string => {
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${bytes} B`;
};

export const formatDuration = (secs?: number): string => {
  if (secs === undefined) return "—";
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
};

export const clearStaging = (id: string) =>
  call<{ cleared: boolean }>(`/api/staging/${id}`, { method: "DELETE" });

export const DISPOSITION_LABELS: Record<Disposition, string> = {
  "new": "新增",
  "alt-version": "替代版本",
  "duplicate": "重复",
  "conflict": "冲突",
  "untagged": "缺标签",
};

/// Default per-item decision: everything actionable is included; untagged
/// rows can't be imported at all.
export const defaultIncluded = (item: ImportItem): boolean =>
  item.disposition !== "untagged";

export const isReplacement = (item: ImportItem): boolean =>
  item.disposition === "duplicate" || item.disposition === "conflict";

export const fileBasename = (p: string): string =>
  p.split("/").pop() ?? p;
