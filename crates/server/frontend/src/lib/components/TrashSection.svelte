<script lang="ts">
  import {
    TRASH_KIND_LABELS,
    fileBasename,
    formatAge,
    formatBytes,
    type TrashEntry,
  } from "../api";
  import {
    clearTrash,
    deleteSelectedTrash,
    refreshTrash,
    restoreAllTrash,
    restoreSelectedTrash,
    trash,
  } from "../library.svelte";
  import { job } from "../job.svelte";
  import LogPanel from "./LogPanel.svelte";
  import Progress from "./Progress.svelte";

  let confirmKind: null | "delete" | "empty" = $state(null);

  const entries = $derived(trash.listing?.entries ?? []);
  const selected = $derived(entries.filter((e) => trash.selected[e.rel]));
  const selectedBytes = $derived(selected.reduce((sum, e) => sum + e.size, 0));
  const restorableCount = $derived(entries.filter((e) => e.restorable).length);
  const unknownCount = $derived(entries.length - restorableCount);
  const busy = $derived(trash.busy || job.running);

  /// Shorten an absolute path under the library root to `~/…`.
  const shorten = (path: string) => {
    const root = trash.listing?.root;
    return root && path.startsWith(root) ? `~${path.slice(root.length)}` : path;
  };
</script>

<div class="mt-8 border-t border-slate-200 pt-5">
  <div class="flex flex-wrap items-center gap-3">
    <h3 class="font-semibold">回收目录</h3>
    {#if trash.listing}
      <span class="text-xs text-slate-500">
        {entries.length} 个文件 · {formatBytes(trash.listing.bytes)}
        {#if unknownCount}· {unknownCount} 个来源未知{/if}
      </span>
    {/if}
    <button
      class="ml-auto text-xs text-slate-500 underline hover:text-slate-700 disabled:opacity-50"
      disabled={busy || trash.loading}
      onclick={refreshTrash}
    >
      刷新
    </button>
  </div>

  <p class="mt-2 text-sm text-slate-600">
    去重移出的文件都放在这里，可以放回原位，也可以永久删除。原位置已经有文件的会跳过，
    不会覆盖（rekordbox 认路径，悄悄改名反而更乱）。
  </p>

  {#if trash.error}
    <p class="mt-2 text-xs text-red-600">{trash.error}</p>
  {/if}

  {#if trash.loading && !trash.listing}
    <p class="mt-3 text-xs text-slate-500">正在读取…</p>
  {:else if entries.length === 0}
    <p class="mt-3 text-sm text-slate-500">回收目录是空的。</p>
  {:else}
    <div class="mt-3 max-h-80 overflow-y-auto rounded-lg border border-slate-200">
      <table class="w-full text-left text-[13px]">
        <thead class="sticky top-0 bg-slate-50 text-xs text-slate-500">
          <tr>
            <th class="w-8 px-3 py-2"></th>
            <th class="px-3 py-2 font-medium">文件</th>
            <th class="px-3 py-2 font-medium">原位置</th>
            <th class="px-3 py-2 font-medium">类型</th>
            <th class="px-3 py-2 text-right font-medium">大小</th>
            <th class="px-3 py-2 text-right font-medium">移入</th>
          </tr>
        </thead>
        <tbody>
          {#each entries as entry (entry.rel)}
            <tr class="border-t border-slate-100 align-top">
              <td class="px-3 py-2">
                <input
                  type="checkbox"
                  class="h-4 w-4 accent-blue-600"
                  checked={trash.selected[entry.rel] ?? false}
                  onchange={(e) => (trash.selected[entry.rel] = e.currentTarget.checked)}
                />
              </td>
              <td class="max-w-48 truncate px-3 py-2" title={entry.path}>
                {fileBasename(entry.rel)}
                {#if !entry.restorable}
                  <span
                    class="ml-1 rounded bg-slate-100 px-1.5 py-0.5 text-[11px] text-slate-500"
                    title="没有来源记录，只能删除"
                  >
                    来源未知
                  </span>
                {/if}
              </td>
              <td class="px-3 py-2 text-slate-500">
                {#if entry.original}
                  <span class="block max-w-72 truncate" title={entry.original}>
                    {shorten(entry.original)}
                  </span>
                {:else}
                  <span class="text-slate-400">—</span>
                {/if}
              </td>
              <td class="px-3 py-2 text-slate-500">
                {TRASH_KIND_LABELS[entry.kind] ?? (entry.kind || "—")}
              </td>
              <td class="px-3 py-2 text-right text-slate-500">{formatBytes(entry.size)}</td>
              <td class="px-3 py-2 text-right text-slate-500">{formatAge(entry.moved_at)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>

    <div class="mt-3 flex flex-wrap items-center gap-3">
      <button
        class="rounded-lg border border-slate-300 px-3 py-1.5 text-xs hover:bg-slate-50 disabled:opacity-50"
        disabled={busy || selected.length === 0}
        onclick={restoreSelectedTrash}
      >
        放回选中 {selected.length} 项
      </button>
      <button
        class="rounded-lg border border-red-300 px-3 py-1.5 text-xs text-red-600 hover:bg-red-50 disabled:opacity-50"
        disabled={busy || selected.length === 0}
        onclick={() => (confirmKind = "delete")}
      >
        删除选中 {selected.length} 项
      </button>
      {#if selected.length > 0}
        <span class="text-xs text-slate-500">共 {formatBytes(selectedBytes)}</span>
      {/if}
      <div class="ml-auto flex items-center gap-2">
        <button
          class="rounded-lg border border-slate-300 px-3 py-1.5 text-xs hover:bg-slate-50 disabled:opacity-50"
          disabled={busy || restorableCount === 0}
          onclick={restoreAllTrash}
        >
          全部放回
        </button>
        <button
          class="rounded-lg border border-red-300 px-3 py-1.5 text-xs text-red-600 hover:bg-red-50 disabled:opacity-50"
          disabled={busy || entries.length === 0}
          onclick={() => (confirmKind = "empty")}
        >
          清空回收目录
        </button>
      </div>
    </div>

    {#if confirmKind}
      <div class="mt-3 rounded-xl border border-red-300 bg-red-50 p-4">
        <p class="text-sm text-red-900">
          {#if confirmKind === "empty"}
            将永久删除回收目录里的 {entries.length} 个文件（{formatBytes(trash.listing?.bytes ?? 0)}），
            不可恢复。
          {:else}
            将永久删除选中的 {selected.length} 个文件（{formatBytes(selectedBytes)}），不可恢复。
          {/if}
        </p>
        <div class="mt-3 flex gap-2">
          <button
            class="rounded-lg bg-red-600 px-4 py-2 text-sm font-medium text-white hover:bg-red-700 disabled:opacity-50"
            disabled={busy}
            onclick={() => {
              const which = confirmKind;
              confirmKind = null;
              if (which === "empty") clearTrash();
              else deleteSelectedTrash();
            }}
          >
            确认删除
          </button>
          <button
            class="rounded-lg border border-red-300 bg-white px-4 py-2 text-sm hover:bg-red-100"
            onclick={() => (confirmKind = null)}
          >
            取消
          </button>
        </div>
      </div>
    {/if}
  {/if}

  {#if job.running && job.kind?.startsWith("trash")}
    <div class="mt-3">
      <Progress current={job.current} total={job.total} />
      <LogPanel lines={job.log} />
    </div>
  {/if}
  {#if trash.result && trash.resultVerb}
    <div class="mt-3 rounded-lg bg-emerald-50 p-4 text-sm text-emerald-800">
      {#if trash.resultVerb === "restored"}
        放回 <span class="font-semibold">{trash.result.restored ?? 0}</span> 个文件
        （{formatBytes(trash.result.bytes)}）
      {:else}
        删除 <span class="font-semibold">{trash.result.removed ?? 0}</span> 个文件
        （{formatBytes(trash.result.bytes)}）
      {/if}
      ；跳过 {trash.result.skipped} 个，失败 {trash.result.failed} 个。
      {#if trash.result.skipped > 0}
        <p class="mt-1 text-xs text-emerald-700">
          被跳过的（原位置已有文件、或来源未知）仍然留在回收目录里。
        </p>
      {/if}
    </div>
  {/if}
</div>
