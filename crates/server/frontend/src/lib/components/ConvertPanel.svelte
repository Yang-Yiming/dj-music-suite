<script lang="ts">
  import { batch, doConvert, resetBatch, uploadFiles } from "../batch.svelte";
  import { job } from "../job.svelte";
  import Dropzone from "./Dropzone.svelte";
  import LogPanel from "./LogPanel.svelte";
  import Progress from "./Progress.svelte";

  let noDownload = $state(false);

  const needsConvert = $derived(
    batch.stagingId !== null &&
      batch.counts.ncm > 0 &&
      !batch.convertDone &&
      !batch.convertSkipped,
  );
</script>

{#if batch.stagingId === null}
  <p class="text-sm text-slate-500">把 .ncm 文件拖进来，解密成 mp3/flac 并写入标题、歌手、专辑、封面和歌词。</p>
  <div class="mt-3">
    <Dropzone onFiles={uploadFiles} />
    {#if batch.uploading}
      <p class="mt-2 text-xs text-slate-500">正在上传…</p>
    {/if}
    {#if batch.uploadError}
      <p class="mt-2 text-xs text-red-600">{batch.uploadError}</p>
    {/if}
  </div>
{:else}
  <div class="flex flex-wrap items-center gap-3 text-sm">
    <span class="font-medium text-emerald-700">✓ 已接收</span>
    {#if batch.counts.ncm}<span class="rounded-full bg-blue-50 px-2.5 py-0.5 text-xs text-blue-700">{batch.counts.ncm} 个 NCM</span>{/if}
    {#if batch.counts.audio}<span class="rounded-full bg-blue-50 px-2.5 py-0.5 text-xs text-blue-700">{batch.counts.audio} 个音频</span>{/if}
    {#if batch.counts.other}<span class="rounded-full bg-slate-100 px-2.5 py-0.5 text-xs text-slate-500">{batch.counts.other} 个其它</span>{/if}
    <button
      class="ml-auto text-xs text-slate-500 underline hover:text-slate-700"
      onclick={resetBatch}
      disabled={batch.clearing || job.running}
    >
      重新选择文件
    </button>
  </div>

  {#if batch.counts.ncm === 0}
    <p class="mt-4 text-sm text-slate-500">
      这批文件里没有 .ncm，不需要转换。音频文件可以直接到「导入」标签处理。
    </p>
  {:else if needsConvert}
    <p class="mt-4 text-sm text-slate-600">
      解密 {batch.counts.ncm} 个 .ncm 文件。转换结果会暂存在本机，稍后在「导入」标签写进曲库。
    </p>
    <div class="mt-3 flex items-center gap-4">
      <button
        class="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
        disabled={job.running}
        onclick={() => doConvert(noDownload)}
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
  {:else if batch.convertDone}
    <p class="mt-4 text-sm text-emerald-700">
      ✓ 转换完成：{batch.convertDone.total - batch.convertDone.failed} 个成功，{batch.convertDone.tagged}
        个写入标签{batch.convertDone.failed ? `，${batch.convertDone.failed} 个失败（见日志）` : ""}。
        到「导入」标签写进曲库。
    </p>
    {#if job.log.length}
      <LogPanel lines={job.log} />
    {/if}
  {:else if batch.convertSkipped}
    <p class="mt-4 text-sm text-slate-500">已跳过转换</p>
  {/if}
{/if}
