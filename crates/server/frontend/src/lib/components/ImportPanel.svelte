<script lang="ts">
  import {
    batch,
    decide,
    doAnalyze,
    doExecute,
    resetBatch,
    uploadFiles,
  } from "../batch.svelte";
  import { job } from "../job.svelte";
  import Dropzone from "./Dropzone.svelte";
  import LogPanel from "./LogPanel.svelte";
  import PlanTable from "./PlanTable.svelte";
  import Progress from "./Progress.svelte";

  let template = $state("{artist}/{filename}.{ext}");

  let { goConvert }: { goConvert?: () => void } = $props();

  const needsConvertFirst = $derived(
    batch.stagingId !== null &&
      batch.counts.ncm > 0 &&
      !batch.convertDone &&
      !batch.convertSkipped,
  );
  const importReady = $derived(
    batch.stagingId !== null && !job.running && !needsConvertFirst,
  );

  const includedCount = $derived(
    batch.plan
      ? batch.plan.items.filter(
          (i) => batch.decisions[i.src] ?? (i.disposition !== "untagged"),
        ).length
      : 0,
  );
  const replacementCount = $derived(
    batch.plan
      ? batch.plan.items.filter(
          (i) =>
            (batch.decisions[i.src] ?? i.disposition !== "untagged") &&
            (i.disposition === "duplicate" || i.disposition === "conflict"),
        ).length
      : 0,
  );
  const newCount = $derived(includedCount - replacementCount);
</script>

{#if batch.stagingId === null}
  <p class="text-sm text-slate-500">
    把音频文件（mp3 / flac / m4a / wav…）拖进来，对照曲库查重后写入。
    如果文件来自「转换」标签的 .ncm 结果，也在这里导入。
  </p>
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
    <span class="font-medium text-emerald-700">✓ 批次就绪</span>
    {#if batch.counts.ncm}<span class="rounded-full bg-blue-50 px-2.5 py-0.5 text-xs text-blue-700">{batch.counts.ncm} 个 NCM</span>{/if}
    {#if batch.counts.audio}<span class="rounded-full bg-blue-50 px-2.5 py-0.5 text-xs text-blue-700">{batch.counts.audio} 个音频</span>{/if}
    {#if batch.convertDone}<span class="rounded-full bg-emerald-50 px-2.5 py-0.5 text-xs text-emerald-700">已转换 {batch.convertDone.total - batch.convertDone.failed} 个</span>{/if}
    <button
      class="ml-auto text-xs text-slate-500 underline hover:text-slate-700"
      onclick={resetBatch}
      disabled={batch.clearing || job.running}
    >
      重新选择文件
    </button>
  </div>

  {#if needsConvertFirst}
    <div class="mt-4 rounded-lg bg-amber-50 p-3 text-sm text-amber-800">
      这批还有 {batch.counts.ncm} 个 .ncm 没有转换。先到「转换」标签处理，
      或跳过转换（未转换的文件不会被导入）。
      <button
        class="ml-2 rounded-md border border-amber-300 bg-white px-2 py-0.5 text-xs text-amber-700 hover:bg-amber-100"
        onclick={() => goConvert?.()}
      >
        去转换
      </button>
      <button
        class="ml-1 rounded-md px-2 py-0.5 text-xs text-amber-700 underline"
        onclick={() => (batch.convertSkipped = true)}
      >
        跳过转换
      </button>
    </div>
  {/if}

  {#if importReady}
    {#if !batch.plan}
      <div class="mt-4 flex flex-wrap items-center gap-3">
        <label class="flex items-center gap-2 text-xs text-slate-600">
          目录模板
          <input
            class="w-64 rounded-lg border border-slate-300 px-2.5 py-1.5 font-mono text-xs focus:border-blue-500 focus:outline-none"
            type="text"
            bind:value={template}
          />
        </label>
        <button
          class="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
          disabled={job.running || batch.analyzeWaiting}
          onclick={() => doAnalyze(template)}
        >
          分析
        </button>
      </div>
      {#if batch.analyzeError}
        <p class="mt-2 text-xs text-red-600">{batch.analyzeError}</p>
      {/if}
      {#if job.running && job.kind === "import-analyze"}
        <div class="mt-3">
          <Progress current={job.current} total={job.total} />
        </div>
      {/if}
    {:else}
      <div class="mt-4">
        <PlanTable plan={batch.plan} decisions={batch.decisions} onchange={decide} />
        <p class="mt-3 text-sm">
          <span class="font-medium">{newCount} 个文件</span>将{batch.mode === "copy" ? "复制" : "移动"}进曲库{replacementCount
            ? `，并覆盖 ${replacementCount} 个重复/冲突文件`
            : ""}
        </p>
        <div class="mt-3 flex flex-wrap items-center gap-4">
          <label class="flex items-center gap-1.5 text-xs text-slate-600">
            <input type="radio" class="accent-blue-600" name="mode" value="copy" bind:group={batch.mode} />
            复制（保留原文件）
          </label>
          <label class="flex items-center gap-1.5 text-xs text-slate-600">
            <input type="radio" class="accent-blue-600" name="mode" value="move" bind:group={batch.mode} />
            移动（源文件删除）
          </label>
          <button
            class="ml-auto rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
            disabled={batch.executing || newCount + replacementCount === 0}
            onclick={doExecute}
          >
            确认导入 {newCount + replacementCount} 个文件
          </button>
        </div>
        {#if batch.executeError}
          <p class="mt-2 text-xs text-red-600">{batch.executeError}</p>
        {/if}
        {#if job.running && job.kind === "import-execute"}
          <div class="mt-3">
            <Progress current={job.current} total={job.total} />
            <LogPanel lines={job.log} />
          </div>
        {/if}
      </div>
    {/if}
  {/if}

  {#if batch.executeResult}
    <div class="mt-4 rounded-lg bg-emerald-50 p-4 text-sm text-emerald-800">
      ✓ 写入 {batch.executeResult.placed}
        个文件{batch.executeResult.failed ? `，${batch.executeResult.failed} 个失败（见日志）` : ""}。到
        rekordbox 里导入/刷新曲库根目录即可看到新歌。
      <button
        class="ml-2 rounded-md border border-emerald-300 bg-white px-2 py-0.5 text-xs text-emerald-700 hover:bg-emerald-100"
        onclick={resetBatch}
        disabled={batch.clearing}
      >
        清理暂存文件，处理下一批
      </button>
    </div>
  {/if}
{/if}
