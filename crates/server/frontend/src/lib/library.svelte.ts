// Library-wide reorg/dedup state. Unlike the upload batch these operate on
// the configured library root, so there is no staging id: the server keeps
// the analyzed plan in memory and the execute call refers back to it.

import {
  getDedupPlan,
  getJob,
  getReorgPlan,
  reorgDefaultIncluded,
  startDedupAnalyze,
  startDedupExecute,
  startReorgAnalyze,
  startReorgExecute,
  type DedupPlan,
  type DedupResult,
  type JobSnapshot,
  type ReorgPlan,
  type ReorgResult,
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
    } else {
      dedup.executeError = end.type === "error" ? end.message : "没有收到去重任务";
    }
  } catch (e) {
    dedup.executeError = e instanceof Error ? e.message : String(e);
  } finally {
    dedup.executing = false;
  }
}

function applyReorgEnd(end: { type: string; result?: unknown; message?: string }) {
  if (end.type === "done") applyReorgPlan(end.result as ReorgPlan);
  else if (end.type === "error") reorg.error = end.message ?? "任务失败";
}

function applyDedupEnd(end: { type: string; result?: unknown; message?: string }) {
  if (end.type === "done") applyDedupPlan(end.result as DedupPlan);
  else if (end.type === "error") dedup.error = end.message ?? "任务失败";
}

/// Restore the reorg/dedup tab after a page reload: a still-running job is
/// resumed from the event stream, a finished one replays its result, and
/// otherwise the in-memory plan is fetched (it survives reloads, not server
/// restarts). Returns the tab that owns the work, if any.
///
/// A running job is resumed in the background: awaiting it here would keep
/// the user on the default tab, without progress, until the job finished.
export async function restoreLibrary(): Promise<"reorg" | "dedup" | null> {
  let snapshot: JobSnapshot | { status: "idle" };
  try {
    snapshot = await getJob();
  } catch {
    return null;
  }
  if ("kind" in snapshot && snapshot.kind.startsWith("reorg")) {
    if (snapshot.status === "running") {
      const analyzing = snapshot.kind === "reorg-analyze";
      reorg.waiting = analyzing;
      reorg.executing = !analyzing;
      resumeRunningJob(snapshot).then((end) => {
        reorg.waiting = false;
        reorg.executing = false;
        applyReorgEnd(end);
      });
    } else {
      if (snapshot.status === "done") applyReorgEnd({ type: "done", result: snapshot.result });
      else reorg.error = snapshot.error ?? "任务失败";
    }
    return "reorg";
  }
  if ("kind" in snapshot && snapshot.kind.startsWith("dedup")) {
    if (snapshot.status === "running") {
      const analyzing = snapshot.kind === "dedup-analyze";
      dedup.waiting = analyzing;
      dedup.executing = !analyzing;
      resumeRunningJob(snapshot).then((end) => {
        dedup.waiting = false;
        dedup.executing = false;
        applyDedupEnd(end);
      });
    } else {
      if (snapshot.status === "done") applyDedupEnd({ type: "done", result: snapshot.result });
      else dedup.error = snapshot.error ?? "任务失败";
    }
    return "dedup";
  }
  // no job involved: a plan analyzed earlier in this server session
  try {
    const [r, d] = await Promise.all([getReorgPlan(), getDedupPlan()]);
    if (r.plan) {
      applyReorgPlan(r.plan);
      return "reorg";
    }
    if (d.plan) {
      applyDedupPlan(d.plan);
      return "dedup";
    }
  } catch {
    // nothing to restore
  }
  return null;
}
