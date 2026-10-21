<script lang="ts">
  import { onMount } from "svelte";
  import { batch, doConvert, importFolder, resetBatch, uploadFiles } from "../batch.svelte";
  import { detectNetease } from "../api";
  import { job } from "../job.svelte";
  import Dropzone from "./Dropzone.svelte";
  import LogPanel from "./LogPanel.svelte";
  import Progress from "./Progress.svelte";

  let noDownload = $state(false);
  let outputMode = $state<"staging" | "direct">("staging");
  let outputDir = $state("");

  const needsConvert = $derived(
    batch.stagingId !== null &&
      batch.counts.ncm > 0 &&
      !batch.convertDone &&
      !batch.convertSkipped,
  );

  // ---- netease download folder detection ----
  let netease = $state<{ path: string; count: number } | null>(null);
  let neteaseError = $state("");
  let neteaseLoading = $state(false);

  onMount(async () => {
    try {
      const found = await detectNetease();
      if (found.path) netease = { path: found.path, count: found.count ?? 0 };
    } catch {
      // detection is best-effort
    }
  });

  async function useNetease() {
    if (!netease) return;
    neteaseLoading = true;
    neteaseError = "";
    try {
      await importFolder(netease.path);
    } catch (e) {
      neteaseError = e instanceof Error ? e.message : String(e);
    } finally {
      neteaseLoading = false;
    }
  }
</script>

{#if batch.stagingId === null}
  {#if netease}
    <div class="mb-3 flex flex-wrap items-center gap-3 rounded-xl border border-blue-200 bg-blue-50 px-4 py-3">
      <div class="min-w-0 flex-1">
        <p class="text-sm font-medium text-blue-900">检测到网易云下载目录</p>
        <p class="truncate text-xs text-blue-700" title={netease.path}>
          {netease.path} · 含 {netease.count} 个 .ncm
        </p>
      </div>
      <button
        class="rounded-lg bg-blue-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-blue-700 disabled:opacity-50"
        disabled={neteaseLoading || job.running}
        onclick={useNetease}
      >
        直接使用
      </button>
    </div>
  {/if}
  {#if neteaseError}
    <p class="mb-2 text-xs text-red-600">{neteaseError}</p>
  {/if}
  <p class="text-sm text-slate-500">把 .ncm 文件或整个下载文件夹拖进来，解密成 mp3/flac 并写入标题、歌手、专辑、封面和歌词。</p>
  <div class="mt-3">
    <Dropzone onFiles={uploadFiles} folder />
    {#if batch.uploading}
      <p class="mt-2 text-xs text-slate-500">正在处理…</p>
    {/if}
    {#if batch.uploadError}
      <p class="mt-2 text-xs text-red-600">{batch.uploadError}</p>
    {/if}
  </div>
{:else}
  <div class="flex flex-wrap items-center gap-3 text-sm">
    <span class="font-medium text-emerald-700">✓ 已接收</span>
    {#if batch.fromFolder}<span class="rounded-full bg-slate-100 px-2.5 py-0.5 text-xs text-slate-500">原文件夹：{batch.fromFolder}</span>{/if}
    {#if batch.counts.ncm}<span class="rounded-full bg-blue-50 px-2.5 py-0.5 text-xs text-blue-700">{batch.counts.ncm} 个 NCM</span>{/if}
    {#if batch.counts.audio}<span class="rounded-full bg-blue-50 px-2.5 py-0.5 text-xs text-blue-700">{batch.counts.audio} 个音频</span>{/if}
    {#if batch.counts.lyrics}<span class="rounded-full bg-violet-50 px-2.5 py-0.5 text-xs text-violet-700">{batch.counts.lyrics} 个歌词</span>{/if}
    {#if batch.counts.image}<span class="rounded-full bg-violet-50 px-2.5 py-0.5 text-xs text-violet-700">{batch.counts.image} 张图片</span>{/if}
    {#if batch.counts.other}<span class="rounded-full bg-slate-100 px-2.5 py-0.5 text-xs text-slate-500">{batch.counts.other} 个其它</span>{/if}
    <button
      class="ml-auto text-xs text-slate-500 underline hover:text-slate-700"
      onclick={resetBatch}
      disabled={batch.clearing || job.running}
    >
      重新选择文件
    </button>
  </div>

  {#if batch.convertDone}
    <p class="mt-4 text-sm text-emerald-700">
      ✓ 转换完成：{batch.convertDone.total - batch.convertDone.failed} 个成功，{batch.convertDone.tagged}
        个写入标签{batch.convertDone.failed ? `，${batch.convertDone.failed} 个失败（见日志）` : ""}。
      {#if batch.convertDone.direct}
        已直接保存到指定目录，未进入曲库导入流程。
      {:else}
        到「导入」标签写进曲库。
      {/if}
    </p>
    {#if batch.convertDone.output_dir}
      <p class="mt-1 break-all font-mono text-xs text-slate-500">{batch.convertDone.output_dir}</p>
    {/if}
    {#if job.log.length}
      <LogPanel lines={job.log} />
    {/if}
  {:else if batch.convertSkipped}
    <p class="mt-4 text-sm text-slate-500">已跳过转换</p>
  {:else if batch.counts.ncm === 0}
    <p class="mt-4 text-sm text-slate-500">
      这批文件里没有 .ncm，不需要转换。音频文件可以直接到「导入」标签处理。
    </p>
  {:else if needsConvert}
    <p class="mt-4 text-sm text-slate-600">
      解密 {batch.counts.ncm} 个 .ncm 文件。
      选择“暂存并导入”时会先写进 Web 暂存目录；选择“直接保存”时写入你指定的目录，不进入导入流程。
    </p>
    <div class="mt-3 grid gap-3 rounded-lg bg-slate-50 p-3 text-xs text-slate-600">
      <label class="flex items-center gap-2">
        <input
          class="accent-blue-600"
          type="radio"
          name="convert-output-mode"
          value="staging"
          bind:group={outputMode}
        />
        暂存，稍后在「导入」标签写进曲库
      </label>
      <label class="flex items-center gap-2">
        <input
          class="accent-blue-600"
          type="radio"
          name="convert-output-mode"
          value="direct"
          bind:group={outputMode}
        />
        直接保存到指定目录（不要求曲库位置）
      </label>
      {#if outputMode === "direct"}
        <label class="flex flex-col gap-1">
          输出目录（必须是绝对路径；不存在时会自动创建）
          <input
            class="rounded-lg border border-slate-300 px-2.5 py-1.5 font-mono text-xs focus:border-blue-500 focus:outline-none"
            type="text"
            placeholder="/Users/you/Desktop/converted"
            bind:value={outputDir}
          />
        </label>
      {/if}
    </div>
    <div class="mt-3 flex items-center gap-4">
      <button
        class="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
        disabled={job.running || (outputMode === "direct" && !outputDir.trim())}
        onclick={() => doConvert(noDownload, outputMode, outputDir.trim())}
      >
        开始转换
      </button>
      <label class="flex items-center gap-1.5 text-xs text-slate-600">
        <input type="checkbox" class="accent-blue-600" bind:checked={noDownload} />
        不联网下载封面
      </label>
    </div>
    {#if batch.convertError}
      <p class="mt-2 text-xs text-red-600">{batch.convertError}</p>
    {/if}
    {#if job.running && job.kind === "convert"}
      <div class="mt-3">
        <Progress current={job.current} total={job.total} />
        <LogPanel lines={job.log} />
      </div>
    {/if}
  {/if}
{/if}
