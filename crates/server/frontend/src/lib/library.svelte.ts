// Library-wide reorg/dedup state. Unlike the upload batch these operate on
// the configured library root, so there is no staging id: the server keeps
// the analyzed plan in memory and the execute call refers back to it.

import {
  deleteTrash,
  getDedupPlan,
  getFfmpeg,
  getJob,
  getReorgPlan,
  getTranscodePlan,
  getTrash,
  reorgDefaultIncluded,
  restoreTrash,
  startDedupAnalyze,
  startDedupExecute,
  startReorgAnalyze,
  startReorgExecute,
  startTranscodeAnalyze,
  startTranscodeExecute,
  type DedupPlan,
  type DedupResult,
  type FfmpegStatus,
  type JobSnapshot,
  type ReorgPlan,
  type ReorgResult,
  type TranscodePlan,
  type TranscodeResult,
  type TranscodeTarget,
  type TrashEntry,
  type TrashListing,
  type TrashResult,
} from "./api";
import { resumeRunningJob, watchJob } from "./job.svelte";

export const reorg = $state<{
  template: string;
  plan: ReorgPlan | null;
  /** src -> ticked in the preview table */
  included: Record<string, boolean>;
  error: string;
  waiting: boolean;
  executing: boolean;
  result: ReorgResult | null;
  executeError: string;
}>({
  template: "{artist}/{filename}.{ext}",
  plan: null,
  included: {},
  error: "",
  waiting: false,
  executing: false,
  result: null,
  executeError: "",
});

export const dedup = $state<{
  keep: "best" | "first";
  plan: DedupPlan | null;
  /** group index -> path overriding the analyzed keeper */
  keepers: Record<number, string>;
  error: string;
  waiting: boolean;
  executing: boolean;
  result: DedupResult | null;
  executeError: string;
}>({
  keep: "best",
  plan: null,
  keepers: {},
  error: "",
  waiting: false,
  executing: false,
  result: null,
  executeError: "",
});

export const transcode = $state<{
  to: TranscodeTarget;
  quality: string;
  /** "" = next to each source; otherwise a subfolder of the library root */
  output: string;
  replace: boolean;
  ffmpeg: FfmpegStatus | null;
  plan: TranscodePlan | null;
  /** src -> ticked in the preview table */
  included: Record<string, boolean>;
  error: string;
  waiting: boolean;
  executing: boolean;
  result: TranscodeResult | null;
  executeError: string;
}>({
  to: "mp3",
  quality: "",
  output: "",
  replace: false,
  ffmpeg: null,
  plan: null,
  included: {},
  error: "",
  waiting: false,
  executing: false,
  result: null,
  executeError: "",
});

/// Whether the local ffmpeg can encode this target; null = not probed yet.
export function ffmpegSupports(target: TranscodeTarget): boolean | null {
  if (!transcode.ffmpeg) return null;
  if (!transcode.ffmpeg.available) return false;
  return transcode.ffmpeg.targets?.find((t) => t.target === target)?.ok ?? false;
}

export async function checkFfmpeg() {
  try {
    transcode.ffmpeg = await getFfmpeg();
  } catch (e) {
    transcode.ffmpeg = {
      available: false,
      error: e instanceof Error ? e.message : String(e),
    };
  }
}

export const trash = $state<{
  listing: TrashListing | null;
  /** rel names ticked in the list */
  selected: Record<string, boolean>;
  loading: boolean;
  error: string;
  busy: boolean;
  result: TrashResult | null;
  resultVerb: "restored" | "removed" | null;
}>({
  listing: null,
  selected: {},
  loading: false,
  error: "",
  busy: false,
  result: null,
  resultVerb: null,
});

/// Refresh the trash listing (also used after a dedup run, which adds files).
export async function refreshTrash() {
  trash.loading = true;
  trash.error = "";
  try {
    trash.listing = await getTrash();
    // drop ticks for entries that are no longer there
    const present = new Set(trash.listing.entries.map((e) => e.rel));
    for (const rel of Object.keys(trash.selected)) {
      if (!present.has(rel)) delete trash.selected[rel];
    }
  } catch (e) {
    trash.error = e instanceof Error ? e.message : String(e);
  } finally {
    trash.loading = false;
  }
}

export const trashSelected = (): TrashEntry[] => {
  const entries = trash.listing?.entries ?? [];
  return entries.filter((e) => trash.selected[e.rel]);
};

/// Run a trash job and refresh the listing afterwards (the job moves or
/// deletes files, so the old listing is always stale).
async function runTrashJob(
  kind: "trash-restore" | "trash-delete",
  entries: string[],
  clear: boolean,
) {
  trash.busy = true;
  trash.error = "";
  trash.result = null;
  trash.resultVerb = null;
  try {
    if (kind === "trash-restore") await restoreTrash(entries, clear);
    else await deleteTrash(entries, clear);
    const end = await watchJob(kind);
    if (end.type === "done") {
      trash.result = end.result as TrashResult;
      trash.resultVerb = kind === "trash-restore" ? "restored" : "removed";
      trash.selected = {};
      await refreshTrash();
    } else {
      trash.error = end.type === "error" ? end.message : "没有收到任务";
    }
  } catch (e) {
    trash.error = e instanceof Error ? e.message : String(e);
  } finally {
    trash.busy = false;
  }
}

export function restoreSelectedTrash() {
  const entries = trashSelected();
  if (entries.length) runTrashJob("trash-restore", entries.map((e) => e.rel), false);
}

export function deleteSelectedTrash() {
  const entries = trashSelected();
  if (entries.length) runTrashJob("trash-delete", entries.map((e) => e.rel), false);
}

export function restoreAllTrash() {
  runTrashJob("trash-restore", [], true);
}

export function clearTrash() {
  runTrashJob("trash-delete", [], true);
}

export async function doReorgAnalyze() {
  reorg.error = "";
  reorg.waiting = true;
  reorg.result = null;
  reorg.executeError = "";
  try {
    await startReorgAnalyze(reorg.template.trim() || "{artist}/{filename}.{ext}");
    const end = await watchJob("reorg-analyze");
    if (end.type === "done") applyReorgPlan(end.result as ReorgPlan);
    else reorg.error = end.type === "error" ? end.message : "没有收到整理分析任务";
  } catch (e) {
    reorg.error = e instanceof Error ? e.message : String(e);
  } finally {
    reorg.waiting = false;
  }
}

function applyReorgPlan(plan: ReorgPlan) {
  reorg.plan = plan;
  // tick exactly the rows the CLI would move without --allow-rename; the
  // rest is a deliberate per-row opt-in
  reorg.included = {};
  for (const entry of plan.entries) {
    reorg.included[entry.src] = reorgDefaultIncluded(entry);
  }
}

export async function doReorgExecute() {
  if (!reorg.plan) return;
  reorg.executing = true;
  reorg.executeError = "";
  reorg.result = null;
  try {
    const include = reorg.plan.entries
      .filter((e) => (reorg.included[e.src] ?? reorgDefaultIncluded(e)))
      .map((e) => e.src);
    if (!include.length) {
      reorg.executeError = "没有勾选任何条目";
      return;
    }
    await startReorgExecute(include);
    const end = await watchJob("reorg-execute");
    if (end.type === "done") {
      reorg.result = end.result as ReorgResult;
      // the moved files no longer match the analyzed plan: make the stale
      // table disappear instead of inviting a second run on the same rows
      reorg.plan = null;
      reorg.included = {};
    } else {
      reorg.executeError = end.type === "error" ? end.message : "没有收到整理任务";
    }
  } catch (e) {
    reorg.executeError = e instanceof Error ? e.message : String(e);
  } finally {
    reorg.executing = false;
  }
}

export async function doDedupAnalyze() {
  dedup.error = "";
  dedup.waiting = true;
  dedup.result = null;
  dedup.executeError = "";
  try {
    await startDedupAnalyze(dedup.keep);
    const end = await watchJob("dedup-analyze");
    if (end.type === "done") applyDedupPlan(end.result as DedupPlan);
    else dedup.error = end.type === "error" ? end.message : "没有收到去重分析任务";
  } catch (e) {
    dedup.error = e instanceof Error ? e.message : String(e);
  } finally {
    dedup.waiting = false;
  }
}

function applyDedupPlan(plan: DedupPlan) {
  dedup.plan = plan;
  // keeper overrides are keyed by group index: a new grouping invalidates them
  dedup.keepers = {};
}

/// Point a group at another keeper; passing the analyzed keeper clears the
/// override again.
export function setDedupKeeper(index: number, path: string) {
  const group = dedup.plan?.groups[index];
  if (!group) return;
  if (group.keep.path === path) delete dedup.keepers[index];
  else dedup.keepers[index] = path;
}

export function dedupKeeperPath(index: number): string | null {
  const group = dedup.plan?.groups[index];
  if (!group) return null;
  return dedup.keepers[index] ?? group.keep.path;
}

/// The rows a group would lose, given the current keeper.
export function dedupLosers(index: number) {
  const group = dedup.plan?.groups[index];
  if (!group) return [];
  const keeper = dedupKeeperPath(index);
  return [group.keep, ...group.losers].filter((f) => f.path !== keeper);
}

export function dedupRemovalCount(): number {
  if (!dedup.plan) return 0;
  let n = 0;
  for (let i = 0; i < dedup.plan.groups.length; i++) n += dedupLosers(i).length;
  return n;
}

export function dedupRemovalBytes(): number {
  if (!dedup.plan) return 0;
  let bytes = 0;
  for (let i = 0; i < dedup.plan.groups.length; i++) {
    for (const f of dedupLosers(i)) bytes += f.size;
  }
  return bytes;
}

export async function doDedupExecute() {
  if (!dedup.plan) return;
  dedup.executing = true;
  dedup.executeError = "";
  dedup.result = null;
  try {
    await startDedupExecute({ ...dedup.keepers });
    const end = await watchJob("dedup-execute");
    if (end.type === "done") {
      dedup.result = end.result as DedupResult;
      dedup.plan = null;
      dedup.keepers = {};
      // the run just added files to the trash
      refreshTrash();
    } else {
      dedup.executeError = end.type === "error" ? end.message : "没有收到去重任务";
    }
  } catch (e) {
    dedup.executeError = e instanceof Error ? e.message : String(e);
  } finally {
    dedup.executing = false;
  }
}

export async function doTranscodeAnalyze() {
  transcode.error = "";
  transcode.waiting = true;
  transcode.result = null;
  transcode.executeError = "";
  try {
    await startTranscodeAnalyze({
      to: transcode.to,
      quality: transcode.quality.trim(),
      output: transcode.output.trim(),
      replace: transcode.replace,
    });
    const end = await watchJob("transcode-analyze");
    if (end.type === "done") applyTranscodePlan(end.result as TranscodePlan);
    else transcode.error = end.type === "error" ? end.message : "没有收到转码分析任务";
  } catch (e) {
    transcode.error = e instanceof Error ? e.message : String(e);
  } finally {
    transcode.waiting = false;
  }
}

function applyTranscodePlan(plan: TranscodePlan) {
  transcode.plan = plan;
  // tick exactly the rows the CLI would convert; the rest stays an explicit
  // per-row opt-in
  transcode.included = {};
  for (const entry of plan.entries) {
    transcode.included[entry.src] = entry.status === "ready";
  }
}

export async function doTranscodeExecute() {
  if (!transcode.plan) return;
  transcode.executing = true;
  transcode.executeError = "";
  transcode.result = null;
  try {
    const include = transcode.plan.entries
      .filter((e) => transcode.included[e.src] ?? e.status === "ready")
      .map((e) => e.src);
    if (!include.length) {
      transcode.executeError = "没有勾选任何条目";
      return;
    }
    await startTranscodeExecute(include);
    const end = await watchJob("transcode-execute");
    if (end.type === "done") {
      transcode.result = end.result as TranscodeResult;
      // the plan described the library as it was, and --replace may have
      // moved sources into the trash: both make the table stale
      transcode.plan = null;
      transcode.included = {};
      refreshTrash();
    } else {
      transcode.executeError = end.type === "error" ? end.message : "没有收到转码任务";
    }
  } catch (e) {
    transcode.executeError = e instanceof Error ? e.message : String(e);
  } finally {
    transcode.executing = false;
  }
}

function applyTranscodeFinish(
  kind: JobSnapshot["kind"],
  end: { type: string; result?: unknown; message?: string },
) {
  if (end.type === "error") {
    transcode.error = end.message ?? "任务失败";
    return;
  }
  if (end.type !== "done") return;
  if (kind === "transcode-analyze") applyTranscodePlan(end.result as TranscodePlan);
  else {
    transcode.result = end.result as TranscodeResult;
    refreshTrash();
  }
}

/// Restore the reorg/dedup tab after a page reload (see [`restoreLibrary`]).
/// The result payloads differ per kind — an analyze yields a plan, an execute
/// yields a summary — so each kind routes to its own slot; feeding a summary
/// into the plan would leave the UI reading fields that aren't there.
function applyReorgFinish(kind: JobSnapshot["kind"], end: { type: string; result?: unknown; message?: string }) {
  if (end.type === "error") {
    reorg.error = end.message ?? "任务失败";
    return;
  }
  if (end.type !== "done") return;
  if (kind === "reorg-analyze") applyReorgPlan(end.result as ReorgPlan);
  else reorg.result = end.result as ReorgResult;
}

function applyDedupFinish(kind: JobSnapshot["kind"], end: { type: string; result?: unknown; message?: string }) {
  if (end.type === "error") {
    dedup.error = end.message ?? "任务失败";
    return;
  }
  if (end.type !== "done") return;
  if (kind === "dedup-analyze") applyDedupPlan(end.result as DedupPlan);
  else {
    dedup.result = end.result as DedupResult;
    refreshTrash();
  }
}

/// Restore the reorg/dedup tab after a page reload: a still-running job is
/// resumed from the event stream, a finished one replays its result, and
/// otherwise the in-memory plan is fetched (it survives reloads, not server
/// restarts). Returns the tab that owns the work, if any.
///
/// A running job is resumed in the background: awaiting it here would keep
/// the user on the default tab, without progress, until the job finished.
export async function restoreLibrary(): Promise<"reorg" | "dedup" | "transcode" | null> {
  let snapshot: JobSnapshot | { status: "idle" };
  try {
    snapshot = await getJob();
  } catch {
    return null;
  }
  if ("kind" in snapshot && (snapshot.kind === "reorg-analyze" || snapshot.kind === "reorg-execute")) {
    const kind = snapshot.kind;
    if (snapshot.status === "running") {
      reorg.waiting = kind === "reorg-analyze";
      reorg.executing = !reorg.waiting;
      resumeRunningJob(snapshot).then((end) => {
        reorg.waiting = false;
        reorg.executing = false;
        applyReorgFinish(kind, end);
      });
    } else if (snapshot.status === "done") {
      applyReorgFinish(kind, { type: "done", result: snapshot.result });
    } else {
      reorg.error = snapshot.error ?? "任务失败";
    }
    return "reorg";
  }
  if ("kind" in snapshot && (snapshot.kind === "dedup-analyze" || snapshot.kind === "dedup-execute")) {
    const kind = snapshot.kind;
    if (snapshot.status === "running") {
      dedup.waiting = kind === "dedup-analyze";
      dedup.executing = !dedup.waiting;
      resumeRunningJob(snapshot).then((end) => {
        dedup.waiting = false;
        dedup.executing = false;
        applyDedupFinish(kind, end);
      });
    } else if (snapshot.status === "done") {
      applyDedupFinish(kind, { type: "done", result: snapshot.result });
    } else {
      dedup.error = snapshot.error ?? "任务失败";
    }
    return "dedup";
  }
  if (
    "kind" in snapshot &&
    (snapshot.kind === "transcode-analyze" || snapshot.kind === "transcode-execute")
  ) {
    const kind = snapshot.kind;
    if (snapshot.status === "running") {
      transcode.waiting = kind === "transcode-analyze";
      transcode.executing = !transcode.waiting;
      resumeRunningJob(snapshot).then((end) => {
        transcode.waiting = false;
        transcode.executing = false;
        applyTranscodeFinish(kind, end);
      });
    } else if (snapshot.status === "done") {
      applyTranscodeFinish(kind, { type: "done", result: snapshot.result });
    } else {
      transcode.error = snapshot.error ?? "任务失败";
    }
    return "transcode";
  }
  if ("kind" in snapshot && (snapshot.kind === "trash-restore" || snapshot.kind === "trash-delete")) {
    const restoring = snapshot.kind === "trash-restore";
    const finish = (end: { type: string; result?: unknown; message?: string }) => {
      trash.busy = false;
      if (end.type === "done") {
        trash.result = end.result as TrashResult;
        trash.resultVerb = restoring ? "restored" : "removed";
        trash.selected = {};
      } else if (end.type === "error") {
        trash.error = end.message ?? "任务失败";
      }
      refreshTrash();
    };
    if (snapshot.status === "running") {
      trash.busy = true;
      resumeRunningJob(snapshot).then(finish);
    } else if (snapshot.status === "done") {
      finish({ type: "done", result: snapshot.result });
    } else {
      trash.error = snapshot.error ?? "任务失败";
      refreshTrash();
    }
    return "dedup";
  }
  // no job involved: a plan analyzed earlier in this server session, plus
  // whatever is sitting in the trash folder
  refreshTrash();
  try {
    const [r, d, t] = await Promise.all([
      getReorgPlan(),
      getDedupPlan(),
      getTranscodePlan(),
    ]);
    if (r.plan) {
      applyReorgPlan(r.plan);
      return "reorg";
    }
    if (d.plan) {
      applyDedupPlan(d.plan);
      return "dedup";
    }
    if (t.plan) {
      applyTranscodePlan(t.plan);
      return "transcode";
    }
  } catch {
    // nothing to restore
  }
  return null;
}
