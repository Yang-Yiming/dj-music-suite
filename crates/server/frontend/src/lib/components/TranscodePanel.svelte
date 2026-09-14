<script lang="ts">
  import {
    TRANSCODE_QUALITY_HINT,
    TRANSCODE_TARGET_LABELS,
    TRANSCODE_STATUS_LABELS,
    fileBasename,
    formatBytes,
    formatDuration,
    type TranscodeEntry,
    type TranscodeStatus,
    type TranscodeTarget,
  } from "../api";
  import {
    transcode,
    doTranscodeAnalyze,
    doTranscodeExecute,
    checkFfmpeg,
    ffmpegSupports,
  } from "../library.svelte";
  import { job } from "../job.svelte";
  import LogPanel from "./LogPanel.svelte";
  import Progress from "./Progress.svelte";

  let { libraryRoot, onNeedSettings }: {
    libraryRoot: string | null;
    onNeedSettings: () => void;
  } = $props();

  let confirming = $state(false);

  const targets = Object.keys(TRANSCODE_TARGET_LABELS) as TranscodeTarget[];

  const statusBadge: Record<TranscodeStatus, string> = {
    "ready": "bg-blue-100 text-blue-700",
    "same-codec": "bg-emerald-100 text-emerald-700",
    "conflict": "bg-red-100 text-red-700",
    "needs-replace": "bg-amber-100 text-amber-800",
  };

  const supported = $derived(ffmpegSupports(transcode.to));
  const targetOk = (t: TranscodeTarget) => ffmpegSupports(t) !== false;

  const counts = $derived.by(() => {
    const tally = new Map<string, number>();
    for (const e of transcode.plan?.entries ?? []) {
      tally.set(e.status, (tally.get(e.status) ?? 0) + 1);
    }
    const out: { label: string; n: number; cls: string }[] = [];
    for (const status of [
      "ready",
      "same-codec",
      "conflict",
      "needs-replace",
    ] as TranscodeStatus[]) {
      const n = tally.get(status) ?? 0;
      if (!n) continue;
      out.push({
        label: TRANSCODE_STATUS_LABELS[status],
        n,
        cls:
          status === "conflict"
            ? "text-red-600"
            : status === "needs-replace"
              ? "text-amber-700"
              : status === "same-codec"
                ? "text-emerald-600"
                : "text-slate-800",
      });
    }
    return out;
  });

  const included = (e: TranscodeEntry) => transcode.included[e.src] ?? e.status === "ready";

  const selected = $derived.by(() => {
    const picked = (transcode.plan?.entries ?? []).filter(
      (e) => e.status === "ready" && included(e),
    );
    return {
      total: picked.length,
      bytes: picked.reduce((sum, e) => sum + e.size, 0),
    };
  });

  const notes = $derived(
    (transcode.plan?.entries ?? []).filter((e) => e.note && e.status !== "same-codec"),
  );

  const sizeDelta = $derived.by(() => {
    const r = transcode.result;
    if (!r || !r.bytes_before) return "";
    const pct = ((r.bytes_after - r.bytes_before) / r.bytes_before) * 100;
    return `${formatBytes(r.bytes_before)} → ${formatBytes(r.bytes_after)}（${pct >= 0 ? "+" : ""}${pct.toFixed(0)}%）`;
  });

  // probe ffmpeg once so the tab can grey out targets this build cannot encode
  $effect(() => {
    if (!transcode.ffmpeg) checkFfmpeg();
  });
</script>

{#if !libraryRoot}
  <p class="mt-2 text-sm text-slate-600">
    先把曲库位置设好，转码默认作用在这个文件夹上（也可以只转一个子目录）。
  </p>
  <button
    class="mt-3 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700"
    onclick={onNeedSettings}
  >
    去设置曲库
  </button>
{:else}
  <p class="mt-2 text-sm text-slate-600">
    用 ffmpeg 把曲库里的音频转成另一种格式：标签、封面、歌词都会带过去，已经是目标格式的文件会跳过
    （重复编码只会掉音质）。先分析出清单，逐条确认后再执行。
  </p>
  <p class="mt-2 truncate text-xs text-slate-400" title={libraryRoot}>曲库：{libraryRoot}</p>

  {#if transcode.ffmpeg && !transcode.ffmpeg.available}
    <div class="mt-3 rounded-xl border border-red-300 bg-red-50 p-3 text-xs text-red-800">
      <p class="font-medium">没有找到 ffmpeg，转码不可用。</p>
      <p class="mt-1">
        安装：macOS <span class="font-mono">brew install ffmpeg</span>、Debian/Ubuntu
        <span class="font-mono">apt install ffmpeg</span>、Windows
        <span class="font-mono">winget install ffmpeg</span>；装好后重启本程序。
      </p>
      {#if transcode.ffmpeg.error}
        <p class="mt-1 font-mono break-all text-red-700">{transcode.ffmpeg.error}</p>
      {/if}
    </div>
  {:else if transcode.ffmpeg}
    <p class="mt-2 text-xs text-emerald-600">
      已检测到 ffmpeg {transcode.ffmpeg.version}
    </p>
  {/if}

  <div class="mt-4 flex flex-wrap items-end gap-3">
    <label class="flex flex-col gap-1 text-xs text-slate-600">
      目标格式
      <select
        class="rounded-lg border border-slate-300 px-2.5 py-1.5 text-sm focus:border-blue-500 focus:outline-none disabled:bg-slate-100"
        bind:value={transcode.to}
      >
        {#each targets as t (t)}
          <option value={t} disabled={!targetOk(t)}>
            {TRANSCODE_TARGET_LABELS[t]}{targetOk(t) ? "" : "（本机 ffmpeg 不支持）"}
          </option>
        {/each}
      </select>
    </label>
    <label class="flex flex-col gap-1 text-xs text-slate-600">
      质量
      <input
        class="w-32 rounded-lg border border-slate-300 px-2.5 py-1.5 font-mono text-xs focus:border-blue-500 focus:outline-none disabled:bg-slate-100"
        type="text"
        placeholder={TRANSCODE_QUALITY_HINT[transcode.to]}
        disabled={transcode.to === "alac" || transcode.to === "wav"}
        bind:value={transcode.quality}
      />
    </label>
    <label class="flex flex-col gap-1 text-xs text-slate-600">
      输出目录
      <div class="flex items-center gap-2">
        <select
          class="rounded-lg border border-slate-300 px-2.5 py-1.5 text-sm focus:border-blue-500 focus:outline-none"
          value={transcode.output ? "sub" : "inplace"}
          onchange={(e) =>
            (transcode.output = e.currentTarget.value === "sub" ? "converted" : "")}
        >
          <option value="inplace">就地（写回每个文件所在的目录）</option>
          <option value="sub">输到曲库内的子目录</option>
        </select>
        {#if transcode.output}
          <input
            class="w-40 rounded-lg border border-slate-300 px-2.5 py-1.5 font-mono text-xs focus:border-blue-500 focus:outline-none"
            type="text"
            bind:value={transcode.output}
          />
        {/if}
      </div>
    </label>
    <button
      class="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
      disabled={job.running || supported === false}
      onclick={() => {
        confirming = false;
        doTranscodeAnalyze();
      }}
    >
      {transcode.plan ? "重新分析" : "分析"}
    </button>
  </div>

  <label class="mt-3 flex items-start gap-2 text-xs text-slate-600">
    <input
      type="checkbox"
      class="mt-0.5 h-4 w-4 accent-blue-600"
      bind:checked={transcode.replace}
    />
    <span>
      转换完成后把源文件移入回收站（<span class="font-mono">.dedup-trash</span>）
      <span class="block text-slate-400">
        只有在勾选后，源文件和目标同名的转换（例如 ALAC 的 m4a 转 AAC）才会执行；源文件在回收站里
        可以随时还原。
      </span>
    </span>
  </label>

  {#if transcode.error}
    <p class="mt-2 text-xs text-red-600">{transcode.error}</p>
  {/if}
  {#if job.running && job.kind === "transcode-analyze"}
    <div class="mt-3">
      <Progress current={job.current} total={job.total} />
      <LogPanel lines={job.log} />
    </div>
  {/if}

  {#if transcode.plan}
    {#if counts.length === 0}
      <p class="mt-4 text-sm text-slate-500">这个曲库里没有找到音频文件。</p>
    {:else}
      <div class="mt-4 flex flex-wrap gap-x-5 gap-y-1 rounded-lg bg-slate-50 px-4 py-3 text-sm">
        {#each counts as c (c.label)}
          <span class={c.cls}>{c.label} <span class="font-semibold">{c.n}</span></span>
        {/each}
      </div>

      {#if notes.length}
        <ul class="mt-3 space-y-1 rounded-lg border border-amber-200 bg-amber-50 p-3 text-xs text-amber-800">
          {#each notes as e (e.src)}
            <li class="truncate" title={e.note}>
              · {fileBasename(e.src)}：{e.note}
            </li>
          {/each}
        </ul>
      {/if}

      <div class="mt-3 max-h-96 overflow-y-auto rounded-lg border border-slate-200">
        <table class="w-full text-left text-[13px]">
          <thead class="sticky top-0 bg-slate-50 text-xs text-slate-500">
            <tr>
              <th class="px-3 py-2 font-medium">文件</th>
              <th class="px-3 py-2 font-medium">源格式</th>
              <th class="px-3 py-2 font-medium">状态</th>
              <th class="px-3 py-2 font-medium">去向</th>
              <th class="px-3 py-2 text-right font-medium">转换</th>
            </tr>
          </thead>
          <tbody>
            {#each transcode.plan.entries as entry (entry.src)}
              <tr class="border-t border-slate-100 align-top">
                <td class="max-w-48 truncate px-3 py-2" title={entry.src}>
                  {fileBasename(entry.src)}
                  <span class="block text-xs text-slate-400">
                    {formatDuration(entry.duration_secs)} · {formatBytes(entry.size)}
                  </span>
                </td>
                <td class="px-3 py-2 text-slate-500">
                  {entry.src_codec ?? "未知"}
                  {#if entry.src_bit_depth}{entry.src_bit_depth}bit{/if}
                </td>
                <td class="px-3 py-2">
                  <span
                    class={`inline-block rounded-full px-2 py-0.5 text-xs whitespace-nowrap ${statusBadge[entry.status]}`}
                  >
                    {TRANSCODE_STATUS_LABELS[entry.status]}
                  </span>
                </td>
                <td class="px-3 py-2 text-slate-500">
                  {#if entry.dst}
                    <span class="block max-w-72 truncate" title={entry.dst}>
                      {entry.dst.replace(transcode.plan!.input, "~")}
                    </span>
                  {/if}
                </td>
                <td class="px-3 py-2 text-right">
                  <input
                    type="checkbox"
                    class="h-4 w-4 accent-blue-600"
                    disabled={entry.status !== "ready"}
                    checked={included(entry)}
                    onchange={(e) => (transcode.included[entry.src] = e.currentTarget.checked)}
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
            for (const e of transcode.plan!.entries) transcode.included[e.src] = e.status === "ready";
          }}
        >
          全选
        </button>
        <button
          class="text-xs text-slate-500 underline hover:text-slate-700"
          onclick={() => {
            for (const e of transcode.plan!.entries) transcode.included[e.src] = false;
          }}
        >
          全不选
        </button>
        <span class="text-xs text-slate-500">
          已选 <span class="font-semibold text-slate-700">{selected.total}</span> 项（{formatBytes(
            selected.bytes,
          )}）
        </span>
        <button
          class="ml-auto rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
          disabled={job.running || selected.total === 0}
          onclick={() => (confirming = true)}
        >
          执行转码
        </button>
      </div>

      {#if confirming}
        <div class="mt-3 rounded-xl border border-amber-300 bg-amber-50 p-4">
          <p class="text-sm text-amber-900">
            将转换 {selected.total} 个文件（源文件共 {formatBytes(selected.bytes)}）为
            {TRANSCODE_TARGET_LABELS[transcode.plan.target]}。
            {#if transcode.plan.replace}
              转换完成后源文件会被移入回收站，之后可以在「去重」标签页里还原或彻底删除。
            {:else}
              源文件保留在原处。
            {/if}
            有损格式转成无损只会让文件更大、音质不会变好，确认继续？
          </p>
          <div class="mt-3 flex gap-2">
            <button
              class="rounded-lg bg-amber-600 px-4 py-2 text-sm font-medium text-white hover:bg-amber-700 disabled:opacity-50"
              disabled={job.running}
              onclick={() => {
                confirming = false;
                doTranscodeExecute();
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

  {#if transcode.executeError}
    <p class="mt-3 text-xs text-red-600">{transcode.executeError}</p>
  {/if}
  {#if job.running && job.kind === "transcode-execute"}
    <div class="mt-3">
      <Progress current={job.current} total={job.total} />
      <LogPanel lines={job.log} />
    </div>
  {/if}
  {#if transcode.result}
    <div class="mt-3 rounded-lg bg-emerald-50 p-4 text-sm text-emerald-800">
      转码完成：转换 <span class="font-semibold">{transcode.result.converted}</span> 个，
      跳过 {transcode.result.skipped} 个，失败 {transcode.result.failed} 个。
      {#if sizeDelta}
        <p class="mt-1 text-xs text-emerald-700">体积：{sizeDelta}</p>
      {/if}
      {#if transcode.result.trashed}
        <p class="mt-1 text-xs text-emerald-700">
          {transcode.result.trashed} 个源文件已移入
          <span class="font-mono">{transcode.result.trash}</span>，可在「去重」标签页还原。
        </p>
      {/if}
      <p class="mt-2 text-xs text-emerald-700">
        接下来在 rekordbox 里刷新曲库根目录即可看到新文件。
      </p>
    </div>
  {/if}
{/if}
