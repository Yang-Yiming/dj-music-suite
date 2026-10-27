<script lang="ts">
  import {
    COVER_PROVIDER_LABELS,
    COVER_STATUS_LABELS,
    coverArtworkUrl,
    fileBasename,
    formatDuration,
    type CoverEntry,
    type CoverProvider,
    type CoverStatus,
  } from "../api";
  import {
    cover,
    coverCandidateIndex,
    doCoverAnalyze,
    doCoverExecute,
    setCoverCandidate,
  } from "../library.svelte";
  import { job } from "../job.svelte";
  import LogPanel from "./LogPanel.svelte";
  import Progress from "./Progress.svelte";

  let { libraryRoot, onNeedSettings }: {
    libraryRoot: string | null;
    onNeedSettings: () => void;
  } = $props();

  const providers = Object.keys(COVER_PROVIDER_LABELS) as CoverProvider[];

  const badge: Record<CoverStatus, string> = {
    existing: "bg-emerald-100 text-emerald-700",
    ready: "bg-blue-100 text-blue-700",
    review: "bg-amber-100 text-amber-800",
    "not-found": "bg-slate-200 text-slate-600",
    unsupported: "bg-slate-200 text-slate-600",
    error: "bg-red-100 text-red-700",
  };

  const counts = $derived.by(() => {
    const tally = new Map<CoverStatus, number>();
    for (const entry of cover.plan?.entries ?? []) {
      tally.set(entry.status, (tally.get(entry.status) ?? 0) + 1);
    }
    return (Object.keys(COVER_STATUS_LABELS) as CoverStatus[])
      .filter((status) => (tally.get(status) ?? 0) > 0)
      .map((status) => ({ status, n: tally.get(status) ?? 0 }));
  });

  const selected = $derived.by(() => {
    const entries = cover.plan?.entries ?? [];
    return entries.filter(
      (entry) => cover.included[entry.path] && entry.candidates.length > 0,
    );
  });

  const selectable = (entry: CoverEntry) => entry.candidates.length > 0;

  function toggle(entry: CoverEntry, checked: boolean) {
    cover.included[entry.path] = checked;
  }

  function selectedCandidate(entry: CoverEntry) {
    return entry.candidates[coverCandidateIndex(entry.path)];
  }
</script>

{#if !libraryRoot}
  <p class="mt-2 text-sm text-slate-600">
    先把曲库位置设好，补封面会默认扫描这个文件夹。
  </p>
  <button
    class="mt-3 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700"
    onclick={onNeedSettings}
  >
    去设置曲库
  </button>
{:else}
  <p class="mt-2 text-sm text-slate-600">
    根据标签（缺失时尝试 <span class="font-mono text-xs">Artist - Title</span> 文件名）在线搜索封面，先分析候选，勾选后再写入。
    只发送歌手、标题和专辑文字，音频不会上传。当前支持 MP3、FLAC、M4A。
  </p>
  <p class="mt-2 truncate text-xs text-slate-400" title={libraryRoot}>曲库：{libraryRoot}</p>

  <div class="mt-4 grid gap-3 sm:grid-cols-2">
    <label class="flex flex-col gap-1 text-xs text-slate-600 sm:col-span-2">
      扫描范围（留空 = 整个曲库；也可以填单曲或子目录路径）
      <input
        class="rounded-lg border border-slate-300 px-2.5 py-1.5 font-mono text-xs focus:border-blue-500 focus:outline-none"
        type="text"
        placeholder={libraryRoot}
        bind:value={cover.input}
      />
    </label>
    <label class="flex flex-col gap-1 text-xs text-slate-600">
      搜索来源
      <select
        class="rounded-lg border border-slate-300 px-2.5 py-1.5 text-sm focus:border-blue-500 focus:outline-none"
        bind:value={cover.provider}
      >
        {#each providers as provider (provider)}
          <option value={provider}>{COVER_PROVIDER_LABELS[provider]}</option>
        {/each}
      </select>
    </label>
    <label class="flex flex-col gap-1 text-xs text-slate-600">
      iTunes 地区
      <input
        class="w-24 rounded-lg border border-slate-300 px-2.5 py-1.5 font-mono text-xs uppercase focus:border-blue-500 focus:outline-none"
        type="text"
        maxlength="2"
        bind:value={cover.country}
      />
    </label>
    <label class="flex flex-col gap-1 text-xs text-slate-600">
      自动匹配阈值
      <div class="flex items-center gap-2">
        <input
          class="w-full accent-blue-600"
          type="range"
          min="0.65"
          max="0.99"
          step="0.01"
          bind:value={cover.minConfidence}
        />
        <span class="w-10 text-right font-mono text-xs">{cover.minConfidence.toFixed(2)}</span>
      </div>
    </label>
    <label class="mt-5 flex items-center gap-2 text-xs text-slate-600">
      <input class="h-4 w-4 accent-blue-600" type="checkbox" bind:checked={cover.overwrite} />
      替换已有封面（默认跳过）
    </label>
  </div>

  <div class="mt-4 flex flex-wrap items-center gap-3">
    <button
      class="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
      disabled={job.running}
      onclick={doCoverAnalyze}
    >
      {cover.plan ? "重新分析" : "分析封面"}
    </button>
    <span class="text-xs text-slate-400">MusicBrainz 有速率限制；同一专辑会复用一次搜索。</span>
  </div>

  {#if cover.error}
    <p class="mt-2 text-xs text-red-600">{cover.error}</p>
  {/if}
  {#if job.running && job.kind === "cover-analyze"}
    <div class="mt-3">
      <Progress current={job.current} total={job.total} />
      <LogPanel lines={job.log} />
    </div>
  {/if}

  {#if cover.plan}
    {#if counts.length === 0}
      <p class="mt-4 text-sm text-slate-500">没有找到音频文件。</p>
    {:else}
      <div class="mt-4 flex flex-wrap gap-x-5 gap-y-1 rounded-lg bg-slate-50 px-4 py-3 text-sm">
        {#each counts as count (count.status)}
          <span class="text-slate-700">
            {COVER_STATUS_LABELS[count.status]}
            <span class="font-semibold">{count.n}</span>
          </span>
        {/each}
      </div>

      <div class="mt-3 max-h-[34rem] overflow-auto rounded-lg border border-slate-200">
        <table class="w-full table-fixed text-left text-[13px]">
          <thead class="sticky top-0 z-10 bg-slate-50 text-xs text-slate-500">
            <tr>
              <th class="w-10 px-2 py-2 font-medium">写入</th>
              <th class="w-20 px-2 py-2 font-medium">封面</th>
              <th class="w-44 px-2 py-2 font-medium">文件 / 标签</th>
              <th class="w-56 px-2 py-2 font-medium">匹配</th>
              <th class="w-24 px-2 py-2 font-medium">状态</th>
            </tr>
          </thead>
          <tbody>
            {#each cover.plan.entries as entry (entry.path)}
              {@const candidate = selectedCandidate(entry)}
              <tr class="border-t border-slate-100 align-top">
                <td class="px-2 py-2">
                  {#if selectable(entry)}
                    <input
                      class="h-4 w-4 accent-blue-600"
                      type="checkbox"
                      checked={cover.included[entry.path] ?? false}
                      onchange={(event) => toggle(entry, event.currentTarget.checked)}
                    />
                  {:else}
                    <span class="text-slate-300">—</span>
                  {/if}
                </td>
                <td class="px-2 py-2">
                  {#if candidate}
                    <img
                      class="h-16 w-16 rounded-md border border-slate-200 bg-slate-100 object-cover"
                      src={coverArtworkUrl(entry.path, coverCandidateIndex(entry.path))}
                      alt=""
                      loading="lazy"
                      onload={(event) => (event.currentTarget.style.opacity = "1")}
                      onerror={(event) => (event.currentTarget.style.opacity = "0.2")}
                    />
                  {:else}
                    <div class="flex h-16 w-16 items-center justify-center rounded-md border border-dashed border-slate-200 text-xs text-slate-300">
                      无
                    </div>
                  {/if}
                </td>
                <td class="px-2 py-2">
                  <span class="block truncate font-medium text-slate-800" title={entry.path}>
                    {fileBasename(entry.path)}
                  </span>
                  <span class="mt-0.5 block truncate text-xs text-slate-500" title={`${entry.artist ?? "?"} / ${entry.title ?? "?"} / ${entry.album ?? "?"}`}>
                    {entry.artist ?? "?"} · {entry.title ?? "?"} · {entry.album ?? "?"}
                  </span>
                  <span class="block text-xs text-slate-400">{formatDuration(entry.duration_secs)}</span>
                </td>
                <td class="px-2 py-2">
                  {#if entry.candidates.length}
                    <select
                      class="w-full rounded-md border border-slate-300 px-2 py-1 text-xs focus:border-blue-500 focus:outline-none"
                      value={String(coverCandidateIndex(entry.path))}
                      onchange={(event) => setCoverCandidate(entry.path, Number(event.currentTarget.value))}
                    >
                      {#each entry.candidates as item, index}
                        <option value={String(index)}>
                          {item.artist ?? "?"} / {item.album ?? item.title ?? "?"} · {item.provider === "musicbrainz" ? "MB" : "iTunes"} · {item.score.toFixed(2)}
                        </option>
                      {/each}
                    </select>
                    {#if candidate}
                      <span class="mt-1 block truncate text-xs text-slate-400" title={candidate.note}>
                        {candidate.year ?? "年份未知"}
                        {#if candidate.note} · {candidate.note}{/if}
                      </span>
                    {/if}
                  {:else}
                    <span class="text-xs text-slate-400">{entry.note ?? "没有候选"}</span>
                  {/if}
                </td>
                <td class="px-2 py-2">
                  <span class={`inline-flex whitespace-nowrap rounded-full px-2 py-0.5 text-xs font-medium ${badge[entry.status]}`}>
                    {COVER_STATUS_LABELS[entry.status]}
                  </span>
                  {#if entry.status === "review"}
                    <span class="mt-1 block text-xs text-amber-700">选择候选并勾选后才会写入</span>
                  {/if}
                  {#if entry.status === "unsupported" || entry.status === "error"}
                    <span class="mt-1 block text-xs text-slate-500">{entry.note ?? ""}</span>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>

      <div class="mt-4 flex flex-wrap items-center gap-3">
        <button
          class="rounded-lg bg-emerald-600 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-50"
          disabled={job.running || selected.length === 0}
          onclick={doCoverExecute}
        >
          写入 {selected.length} 个封面
        </button>
        <span class="text-xs text-slate-400">写入前会再次读取文件；分析后文件有变化则跳过。</span>
      </div>
    {/if}
  {/if}

  {#if cover.executeError}
    <p class="mt-2 text-xs text-red-600">{cover.executeError}</p>
  {/if}
  {#if job.running && job.kind === "cover-execute"}
    <div class="mt-3">
      <Progress current={job.current} total={job.total} />
      <LogPanel lines={job.log} />
    </div>
  {/if}
  {#if cover.result}
    <div class="mt-4 rounded-lg border border-emerald-200 bg-emerald-50 px-4 py-3 text-sm text-emerald-800">
      写入完成：{cover.result.embedded} 个新增封面，{cover.result.replaced} 个替换，
      {cover.result.skipped} 个跳过，{cover.result.failed} 个失败。
      <span class="mt-1 block text-xs text-emerald-700">
        在 rekordbox 里刷新对应曲目或专辑即可读取内嵌封面。
      </span>
    </div>
  {/if}
{/if}
