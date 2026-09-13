<script lang="ts">
  import {
    REORG_OP_LABELS,
    REORG_STATUS_LABELS,
    fileBasename,
    formatBytes,
    formatDuration,
    reorgDefaultIncluded,
    reorgIsRename,
    type ReorgEntry,
    type ReorgOp,
    type ReorgStatus,
  } from "../api";
  import { reorg, doReorgAnalyze, doReorgExecute } from "../library.svelte";
  import { job } from "../job.svelte";
  import LogPanel from "./LogPanel.svelte";
  import Progress from "./Progress.svelte";

  let {
    libraryRoot,
    onNeedSettings,
    goDedup,
  }: {
    libraryRoot: string | null;
    onNeedSettings: () => void;
    goDedup: () => void;
  } = $props();

  let showDuplicates = $state(false);
  let confirming = $state(false);

  const statusBadge: Record<ReorgStatus, string> = {
    "ready": "bg-blue-100 text-blue-700",
    "in-place": "bg-emerald-100 text-emerald-700",
    "conflict": "bg-red-100 text-red-700",
    "untagged": "bg-slate-200 text-slate-600",
  };

  // ready rows are summarised by their op, every other row by its status
  // (mirrors the CLI report)
  const counts = $derived.by(() => {
    const out: { label: string; n: number; cls: string }[] = [];
    const tally = new Map<string, number>();
    for (const e of reorg.plan?.entries ?? []) {
      const key = e.status === "ready" ? e.op : e.status;
      tally.set(key, (tally.get(key) ?? 0) + 1);
    }
    const rows: [string, string][] = [
      ["move", "移动"],
      ["rename", "重命名"],
      ["move+rename", "移动+重命名"],
      ["in-place", "已就位"],
      ["conflict", "目标已存在"],
      ["untagged", "缺标签"],
    ];
    for (const [key, label] of rows) {
      const n = tally.get(key) ?? 0;
      if (!n) continue;
      const cls =
        key === "conflict" || key === "untagged"
          ? "text-red-600"
          : key === "in-place"
            ? "text-emerald-600"
            : "text-slate-800";
      out.push({ label, n, cls });
    }
    return out;
  });

  const selected = $derived.by(() => {
    const entries = reorg.plan?.entries ?? [];
    const picked = entries.filter(
      (e) => e.status === "ready" && (reorg.included[e.src] ?? reorgDefaultIncluded(e)),
    );
    return {
      total: picked.length,
      renames: picked.filter(reorgIsRename).length,
    };
  });

  const renameCount = $derived(
    (reorg.plan?.entries ?? []).filter((e) => reorgIsRename(e) && e.status === "ready").length,
  );

  const actionable = (e: ReorgEntry) => e.status === "ready";
</script>

{#if !libraryRoot}
  <p class="mt-2 text-sm text-slate-600">
    先把曲库位置设好，整理操作都在这个文件夹里进行。
  </p>
  <button
    class="mt-3 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700"
    onclick={onNeedSettings}
  >
    去设置曲库
  </button>
{:else}
  <p class="mt-2 text-sm text-slate-600">
    按标签把曲库文件归位到目录模板下，先分析出移动计划，逐条确认后再执行；执行完在 rekordbox 里
    File → Display All Missing Files → Relocate 指向曲库根目录即可。
  </p>
  <p class="mt-2 truncate text-xs text-slate-400" title={libraryRoot}>曲库：{libraryRoot}</p>

  <div class="mt-4 flex flex-wrap items-center gap-3">
    <label class="flex items-center gap-2 text-xs text-slate-600">
      目录模板
      <input
        class="w-64 rounded-lg border border-slate-300 px-2.5 py-1.5 font-mono text-xs focus:border-blue-500 focus:outline-none"
        type="text"
        bind:value={reorg.template}
      />
    </label>
    <button
      class="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
      disabled={job.running}
      onclick={() => {
        confirming = false;
        doReorgAnalyze();
      }}
    >
      {reorg.plan ? "重新分析" : "分析"}
    </button>
    <span class="text-xs text-slate-400">
      可用占位符：{`{artist} {artists} {title} {album} {filename} {ext}`}
    </span>
  </div>

  {#if reorg.error}
    <p class="mt-2 text-xs text-red-600">{reorg.error}</p>
  {/if}
  {#if job.running && job.kind === "reorg-analyze"}
    <div class="mt-3">
      <Progress current={job.current} total={job.total} />
      <LogPanel lines={job.log} />
    </div>
  {/if}

  {#if reorg.plan}
    {#if counts.length === 0}
      <p class="mt-4 text-sm text-slate-500">这个曲库里没有找到音频文件。</p>
    {:else}
      <div class="mt-4 flex flex-wrap gap-x-5 gap-y-1 rounded-lg bg-slate-50 px-4 py-3 text-sm">
        {#each counts as c (c.label)}
          <span class={c.cls}>{c.label} <span class="font-semibold">{c.n}</span></span>
        {/each}
      </div>

      {#if reorg.plan.duplicates.length}
        <button
          class="mt-3 text-xs text-amber-700 underline hover:text-amber-900"
          onclick={() => (showDuplicates = !showDuplicates)}
        >
          {showDuplicates ? "收起" : "查看"} {reorg.plan.duplicates.length} 组疑似重复
          （同歌手 + 同标题）
        </button>
        {#if showDuplicates}
          <div class="mt-2 max-h-56 overflow-y-auto rounded-lg border border-amber-200 bg-amber-50 p-3">
            {#each reorg.plan.duplicates as g (g.artist + g.title)}
              <div class="mb-2 last:mb-0">
                <p class="text-xs font-medium text-amber-900">{g.artist} / {g.title}</p>
                {#each g.files as f (f.path)}
                  <p class="truncate text-xs text-amber-800" title={f.path}>
                    · {fileBasename(f.path)}
                    <span class="text-amber-600">
                      {formatDuration(f.duration_secs)}{f.size ? ` · ${formatBytes(f.size)}` : ""}
                    </span>
                  </p>
                {/each}
              </div>
            {/each}
            <button
              class="mt-2 rounded-md border border-amber-300 bg-white px-2 py-0.5 text-xs text-amber-700 hover:bg-amber-100"
              onclick={goDedup}
            >
              去「去重」逐组处理
            </button>
          </div>
        {/if}
      {/if}

      <div class="mt-3 max-h-96 overflow-y-auto rounded-lg border border-slate-200">
        <table class="w-full text-left text-[13px]">
          <thead class="sticky top-0 bg-slate-50 text-xs text-slate-500">
            <tr>
              <th class="px-3 py-2 font-medium">文件</th>
              <th class="px-3 py-2 font-medium">操作</th>
              <th class="px-3 py-2 font-medium">去向 / 说明</th>
              <th class="px-3 py-2 text-right font-medium">应用</th>
            </tr>
          </thead>
          <tbody>
            {#each reorg.plan.entries as entry (entry.src)}
              {@const included = reorg.included[entry.src] ?? reorgDefaultIncluded(entry)}
              {@const rename = reorgIsRename(entry)}
              <tr class="border-t border-slate-100 align-top">
                <td class="max-w-48 truncate px-3 py-2" title={entry.src}>
                  {fileBasename(entry.src)}
                </td>
                <td class="px-3 py-2">
                  <span
                    class={`inline-block rounded-full px-2 py-0.5 text-xs whitespace-nowrap ${statusBadge[entry.status]}`}
                  >
                    {entry.status === "ready"
                      ? REORG_OP_LABELS[entry.op as ReorgOp]
                      : REORG_STATUS_LABELS[entry.status]}
                  </span>
                  {#if rename && entry.status === "ready"}
                    <span class="ml-1 text-xs text-amber-600">需手动 Relink</span>
                  {/if}
                </td>
                <td class="px-3 py-2 text-slate-500">
                  {#if entry.dst}
                    <span class="block max-w-72 truncate" title={entry.dst}>
                      {entry.dst.replace(reorg.plan!.root, "~")}
                    </span>
                  {/if}
                  {#if entry.note}
                    <span class="block max-w-72 truncate text-xs" title={entry.note}>
                      {entry.note}
                    </span>
                  {/if}
                </td>
                <td class="px-3 py-2 text-right">
                  <input
                    type="checkbox"
                    class="h-4 w-4 accent-blue-600"
                    disabled={!actionable(entry)}
                    checked={included}
                    onchange={(e) => (reorg.included[entry.src] = e.currentTarget.checked)}
                  />
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>

      <div class="mt-3 flex flex-wrap items-center gap-3">
        <button
          class="text-xs text-slate-500 underline hover:text-slate-700"
          onclick={() => {
            for (const e of reorg.plan!.entries) {
              reorg.included[e.src] = e.status === "ready" && e.op === "move";
            }
          }}
        >
          只选移动
        </button>
        <button
          class="text-xs text-slate-500 underline hover:text-slate-700"
          onclick={() => {
            for (const e of reorg.plan!.entries) {
              reorg.included[e.src] = e.status === "ready";
            }
          }}
        >
          全选
        </button>
        <button
          class="text-xs text-slate-500 underline hover:text-slate-700"
          onclick={() => {
            for (const e of reorg.plan!.entries) reorg.included[e.src] = false;
          }}
        >
          全不选
        </button>
        <span class="text-xs text-slate-500">
          已选 <span class="font-semibold text-slate-700">{selected.total}</span> 项
        </span>
        <button
          class="ml-auto rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
          disabled={job.running || selected.total === 0}
          onclick={() => (confirming = true)}
        >
          执行整理
        </button>
      </div>

      {#if renameCount > 0}
        <p class="mt-2 text-xs text-amber-700">
          有 {renameCount} 个文件会被重命名。rekordbox 靠文件名重新定位，重命名的文件需要之后再手动
          Relink，确认无误再勾选。
        </p>
      {/if}

      {#if confirming}
        <div class="mt-3 rounded-xl border border-amber-300 bg-amber-50 p-4">
          <p class="text-sm text-amber-900">
            将移动/重命名 {selected.total} 个文件
            {#if selected.renames}（其中 {selected.renames} 个是重命名）{/if}，
            目标位置已存在的文件会跳过。曲库里的文件会真的移动，确认继续？
          </p>
          <div class="mt-3 flex gap-2">
            <button
              class="rounded-lg bg-amber-600 px-4 py-2 text-sm font-medium text-white hover:bg-amber-700 disabled:opacity-50"
              disabled={job.running}
              onclick={() => {
                confirming = false;
                doReorgExecute();
              }}
            >
              确认执行
            </button>
            <button
              class="rounded-lg border border-amber-300 bg-white px-4 py-2 text-sm hover:bg-amber-100"
              onclick={() => (confirming = false)}
            >
              取消
            </button>
          </div>
        </div>
      {/if}
    {/if}
  {/if}

  {#if reorg.executeError}
    <p class="mt-3 text-xs text-red-600">{reorg.executeError}</p>
  {/if}
  {#if job.running && job.kind === "reorg-execute"}
    <div class="mt-3">
      <Progress current={job.current} total={job.total} />
      <LogPanel lines={job.log} />
    </div>
  {/if}
  {#if reorg.result}
    <div class="mt-3 rounded-lg bg-emerald-50 p-4 text-sm text-emerald-800">
      整理完成：移动 <span class="font-semibold">{reorg.result.moved}</span> 个
      {#if reorg.result.renamed}（其中重命名 {reorg.result.renamed} 个）{/if}，
      跳过 {reorg.result.skipped} 个，失败 {reorg.result.failed} 个。
      <p class="mt-2 text-xs text-emerald-700">
        接下来在 rekordbox 里：File → Display All Missing Files → 全选 → Relocate → 指向
        <span class="font-mono">{reorg.result.root}</span>
        {#if reorg.result.renamed}；重命名的 {reorg.result.renamed} 个文件需要手动 Relink{/if}。
      </p>
    </div>
  {/if}
{/if}
