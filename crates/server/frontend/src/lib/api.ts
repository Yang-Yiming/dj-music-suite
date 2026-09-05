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
  | { type: "line"; text: string }
  | { type: "warn"; text: string };

export type EndPayload =
  | { type: "done"; result: unknown }
  | { type: "error"; message: string }
  | { type: "idle" };

export interface JobSnapshot {
  kind: "convert" | "import-analyze" | "import-execute";
  staging_id: string | null;
  status: "running" | "done" | "failed";
  events: JobEvent[];
  result: unknown;
  error: string | null;
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
