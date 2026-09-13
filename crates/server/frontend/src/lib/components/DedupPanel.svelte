<script lang="ts">
  import {
    DEDUP_KIND_LABELS,
    fileBasename,
    formatBytes,
    formatDuration,
    type DedupFile,
  } from "../api";
  import {
    dedup,
    dedupKeeperPath,
    dedupLosers,
    dedupRemovalBytes,
    dedupRemovalCount,
    doDedupAnalyze,
    doDedupExecute,
    setDedupKeeper,
  } from "../library.svelte";
  import { job } from "../job.svelte";
  import LogPanel from "./LogPanel.svelte";
  import Progress from "./Progress.svelte";
  import TrashSection from "./TrashSection.svelte";

  let {
    libraryRoot,
    onNeedSettings,
  }: {
    libraryRoot: string | null;
    onNeedSettings: () => void;
  } = $props();

  let confirming = $state(false);

  const removalCount = $derived(dedup.plan ? dedupRemovalCount() : 0);
  const removalBytes = $derived(dedup.plan ? dedupRemovalBytes() : 0);

  const qualityBits = (f: DedupFile): string[] => {
    const out: string[] = [];
    // lofty reports kbps and Hz directly
    if (f.score.bitrate) out.push(`${f.score.bitrate} kbps`);
    if (f.score.sample_rate) out.push(`${(f.score.sample_rate / 1000).toFixed(1)} kHz`);
    if (f.score.bits_per_sample) out.push(`${f.score.bits_per_sample} bit`);
    return out;
  };

  const chips = (f: DedupFile): { label: string; on: boolean }[] => [
    { label: "封面", on: f.score.has_cover },
    { label: "歌词", on: f.score.has_lyrics },
    { label: "标签齐", on: f.score.tags_complete },
  ];
</script>

{#if !libraryRoot}
  <p class="mt-2 text-sm text-slate-600">
    先把曲库位置设好，去重扫描的是整个曲库文件夹。
  </p>
  <button
    class="mt-3 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700"
    onclick={onNeedSettings}
  >
    去设置曲库
  </button>
{:else}
  <p class="mt-2 text-sm text-slate-600">
    扫描整个曲库：内容完全相同的文件直接归组，同一首歌的不同版本按音质评分保留最好的一份。
    这里只把其余副本移入下方的回收目录，不会直接删除 —— 确认无误后再在回收目录里放回或清空。
  </p>
  <p class="mt-2 truncate text-xs text-slate-400" title={libraryRoot}>曲库：{libraryRoot}</p>

  <div class="mt-4 flex flex-wrap items-center gap-4">
    <label class="flex items-center gap-1.5 text-xs text-slate-600">
      <input type="radio" class="accent-blue-600" name="keep" value="best" bind:group={dedup.keep} />
      保留音质最好
    </label>
    <label class="flex items-center gap-1.5 text-xs text-slate-600">
      <input type="radio" class="accent-blue-600" name="keep" value="first" bind:group={dedup.keep} />
      保留第一个
    </label>
    <button
      class="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
      disabled={job.running}
      onclick={() => {
        confirming = false;
        doDedupAnalyze();
      }}
    >
      {dedup.plan ? "重新扫描" : "扫描重复"}
    </button>
  </div>

  {#if dedup.error}
    <p class="mt-2 text-xs text-red-600">{dedup.error}</p>
  {/if}
  {#if job.running && job.kind === "dedup-analyze"}
    <div class="mt-3">
      <Progress current={job.current} total={job.total} />
      <LogPanel lines={job.log} />
    </div>
  {/if}

  {#if dedup.plan}
    <p class="mt-4 text-sm text-slate-600">
      扫描 {dedup.plan.stats.scanned} 个文件，找到
      <span class="font-semibold">{dedup.plan.groups.length}</span> 组重复
      {#if dedup.plan.stats.unreadable}（{dedup.plan.stats.unreadable} 个无法读取，已忽略）{/if}
      。
    </p>

    {#if dedup.plan.groups.length === 0}
      <p class="mt-2 text-sm text-emerald-700">没有发现重复文件。</p>
    {:else}
      <div class="mt-3 max-h-96 space-y-3 overflow-y-auto pr-1">
        {#each dedup.plan.groups as group, index (group.keep.path + index)}
          {@const keeper = dedupKeeperPath(index)}
          {@const losers = dedupLosers(index)}
          <div class="rounded-lg border border-slate-200">
            <div class="flex items-center justify-between border-b border-slate-100 bg-slate-50 px-3 py-2">
              <span class="text-xs font-medium text-slate-700">
                {DEDUP_KIND_LABELS[group.kind]}
                {#if group.identity}
                  <span class="ml-1 font-normal text-slate-500">
                    {group.identity[0]} / {group.identity[1]}
                  </span>
                {/if}
              </span>
              <span class="text-xs text-slate-500">
                {[group.keep, ...group.losers].length} 个文件 · 移出 {losers.length} 个
              </span>
            </div>
            <ul class="divide-y divide-slate-100">
              {#each [group.keep, ...group.losers] as file (file.path)}
                {@const isKeeper = file.path === keeper}
                <li
                  class={`flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 text-[13px] ${
                    isKeeper ? "bg-emerald-50" : ""
                  }`}
                >
                  <input
                    type="radio"
                    class="h-4 w-4 accent-emerald-600"
                    name={`keep-${index}`}
                    checked={isKeeper}
                    onchange={() => setDedupKeeper(index, file.path)}
                    title="设为保留项"
                  />
                  <span class="min-w-0 flex-1">
                    <span class="block truncate" title={file.path}>{fileBasename(file.path)}</span>
                    <span class="block truncate text-xs text-slate-400" title={file.path}>
                      {file.path.replace(dedup.plan!.root, "~")}
                    </span>
                  </span>
                  <span class="text-xs text-slate-500">
                    {formatDuration(file.duration_secs)} · {formatBytes(file.size)}
                  </span>
                  <span class="w-32 text-right text-xs text-slate-500">
                    {qualityBits(file).join(" · ")}
                  </span>
                  <span class="flex gap-1">
                    {#each chips(file) as chip (chip.label)}
                      <span
                        class={`rounded px-1.5 py-0.5 text-[11px] ${
                          chip.on ? "bg-emerald-100 text-emerald-700" : "bg-slate-100 text-slate-400"
                        }`}
                      >
                        {chip.label}
                      </span>
                    {/each}
                  </span>
                  <span class="w-14 text-right text-xs text-slate-400" title="音质评分">
                    {file.score.score}
                  </span>
                  <span
                    class={`w-12 text-right text-xs ${
                      isKeeper ? "font-medium text-emerald-700" : "text-amber-600"
                    }`}
                  >
                    {isKeeper ? "保留" : "移出"}
                  </span>
                </li>
              {/each}
            </ul>
          </div>
        {/each}
      </div>

      <div class="mt-3 flex flex-wrap items-center gap-3">
        <span class="text-sm text-slate-600">
          将移出 <span class="font-semibold">{removalCount}</span> 个文件，释放
          <span class="font-semibold">{formatBytes(removalBytes)}</span>
        </span>
        <button
          class="ml-auto rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
          disabled={job.running || removalCount === 0}
          onclick={() => (confirming = true)}
        >
          执行去重
        </button>
      </div>
      {#if Object.keys(dedup.keepers).length}
        <button
          class="mt-2 text-xs text-slate-500 underline hover:text-slate-700"
          onclick={() => (dedup.keepers = {})}
        >
          恢复默认保留项
        </button>
      {/if}

      {#if confirming}
        <div class="mt-3 rounded-xl border border-amber-300 bg-amber-50 p-4">
          <p class="text-sm text-amber-900">
            将把 {removalCount} 个文件（{formatBytes(removalBytes)}）移入
            <span class="font-mono text-xs">{dedup.plan.root}/.dedup-trash</span>，
            每组保留你选中的那一个。确认继续？
          </p>
          <div class="mt-3 flex gap-2">
            <button
              class="rounded-lg bg-amber-600 px-4 py-2 text-sm font-medium text-white hover:bg-amber-700 disabled:opacity-50"
              disabled={job.running}
              onclick={() => {
                confirming = false;
                doDedupExecute();
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

  {#if dedup.executeError}
    <p class="mt-3 text-xs text-red-600">{dedup.executeError}</p>
  {/if}
  {#if job.running && job.kind === "dedup-execute"}
    <div class="mt-3">
      <Progress current={job.current} total={job.total} />
      <LogPanel lines={job.log} />
    </div>
  {/if}
  {#if dedup.result}
    <div class="mt-3 rounded-lg bg-emerald-50 p-4 text-sm text-emerald-800">
      去重完成：移出 <span class="font-semibold">{dedup.result.removed}</span> 个文件，释放
      {formatBytes(dedup.result.freed_bytes)}；跳过 {dedup.result.skipped} 个，失败
      {dedup.result.failed} 个。
      <p class="mt-2 text-xs text-emerald-700">
        文件都在回收目录里，确认无误后再删除。
      </p>
    </div>
  {/if}

  <TrashSection />
{/if}
