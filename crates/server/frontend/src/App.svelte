<script lang="ts">
  import { onMount } from "svelte";
  import {
    clearStaging,
    defaultIncluded,
    fileBasename,
    getConfig,
    getJob,
    isReplacement,
    setConfig,
    startAnalyze,
    startConvert,
    startExecute,
    upload,
    type ConvertResult,
    type ExecuteResult,
    type ImportPlan,
  } from "./lib/api";
  import {
    job,
    resumeRunningJob,
    watchJob,
  } from "./lib/job.svelte";
  import Dropzone from "./lib/components/Dropzone.svelte";
  import JobBar from "./lib/components/JobBar.svelte";
  import LogPanel from "./lib/components/LogPanel.svelte";
  import PlanTable from "./lib/components/PlanTable.svelte";
  import Progress from "./lib/components/Progress.svelte";
  import SettingsModal from "./lib/components/SettingsModal.svelte";

  // ---------- config ----------
  let libraryRoot = $state<string | null>(null);
  let settingsOpen = $state(false);

  async function saveLibraryRoot(root: string) {
    const saved = await setConfig(root);
    libraryRoot = saved.library_root;
  }

  // ---------- staging / batch ----------
  let stagingId = $state<string | null>(null);
  let counts = $state({ ncm: 0, audio: 0, other: 0 });
  let uploadError = $state("");
  let uploading = $state(false);

  async function onFiles(files: File[]) {
    if (!files.length || job.running) return;
    uploading = true;
    uploadError = "";
    try {
      const result = await upload(files);
      stagingId = result.staging_id;
      counts = { ncm: 0, audio: 0, other: 0 };
      for (const f of result.files) counts[f.kind] += 1;
      convertDone = null;
      convertSkipped = false;
      plan = null;
      decisions = {};
      executeResult = null;
    } catch (e) {
      uploadError = e instanceof Error ? e.message : String(e);
    } finally {
      uploading = false;
    }
  }

  // ---------- convert ----------
  let noDownload = $state(false);
  let convertDone = $state<ConvertResult | null>(null);
  let convertSkipped = $state(false);
  let convertError = $state("");

  const needsConvert = $derived(
    stagingId !== null && counts.ncm > 0 && !convertDone && !convertSkipped,
  );
  const importReady = $derived(
    stagingId !== null &&
      !job.running &&
      (convertDone !== null || convertSkipped || counts.ncm === 0),
  );

  async function doConvert() {
    if (!stagingId) return;
    convertError = "";
    try {
      await startConvert(stagingId, noDownload);
      const end = await watchJob("convert");
      if (end.type === "done") {
        convertDone = end.result as ConvertResult;
      } else {
        convertError =
          end.type === "error" ? end.message : "没有收到转换任务";
      }
    } catch (e) {
      convertError = e instanceof Error ? e.message : String(e);
    }
  }

  // ---------- import analyze ----------
  let template = $state("{artist}/{filename}.{ext}");
  let plan = $state<ImportPlan | null>(null);
  let decisions = $state<Record<string, boolean>>({});
  let analyzeError = $state("");
  let analyzeWaiting = $state(false);

  const includedItems = $derived(
    plan
      ? plan.items.filter((i) => decisions[i.src] ?? defaultIncluded(i))
      : [],
  );
  const replacementCount = $derived(
    includedItems.filter(isReplacement).length,
  );
  const newCount = $derived(includedItems.length - replacementCount);

  function decide(src: string, include: boolean) {
    decisions[src] = include;
  }

  async function doAnalyze() {
    if (!stagingId) return;
    analyzeError = "";
    analyzeWaiting = true;
    plan = null;
    decisions = {};
    try {
      await startAnalyze(stagingId, template.trim() || "{artist}/{filename}.{ext}");
      const end = await watchJob("import-analyze");
      if (end.type === "done") {
        plan = end.result as ImportPlan;
      } else {
        analyzeError =
          end.type === "error" ? end.message : "没有收到分析任务";
      }
    } catch (e) {
      analyzeError = e instanceof Error ? e.message : String(e);
    } finally {
      analyzeWaiting = false;
    }
  }

  // ---------- import execute ----------
  let mode = $state<"copy" | "move">("copy");
  let executeResult = $state<ExecuteResult | null>(null);
  let executeError = $state("");
  let executing = $state(false);

  async function doExecute() {
    if (!stagingId || !plan) return;
    executing = true;
    executeError = "";
    try {
      await startExecute(
        stagingId,
        mode,
        replacementCount > 0,
        includedItems.map((i) => i.src),
      );
      const end = await watchJob("import-execute");
      if (end.type === "done") {
        executeResult = end.result as ExecuteResult;
      } else {
        executeError =
          end.type === "error" ? end.message : "没有收到导入任务";
      }
    } catch (e) {
      executeError = e instanceof Error ? e.message : String(e);
    } finally {
      executing = false;
    }
  }

  // ---------- batch cleanup ----------
  let clearing = $state(false);

  async function resetBatch() {
    if (!stagingId) return;
    clearing = true;
    try {
      await clearStaging(stagingId);
    } catch {
      // staging cleanup is best-effort
    }
    stagingId = null;
    counts = { ncm: 0, audio: 0, other: 0 };
    convertDone = null;
    convertSkipped = false;
    plan = null;
    decisions = {};
    executeResult = null;
    clearing = false;
  }

  // ---------- recovery after reload ----------
  onMount(async () => {
    try {
      const cfg = await getConfig();
      libraryRoot = cfg.library_root;
    } catch {
      // settings can be configured later
    }
    try {
      const snap = await getJob();
      if (!("kind" in snap)) return;
      if (snap.staging_id && !stagingId) stagingId = snap.staging_id;
      if (snap.status === "running") {
        const kind = snap.kind;
        if (kind === "import-analyze") analyzeWaiting = true;
        if (kind === "import-execute") executing = true;
        const end = await resumeRunningJob(snap);
        analyzeWaiting = false;
        executing = false;
        applyEnd(kind, end);
      } else if (snap.status === "done" && snap.staging_id) {
        applyEnd(snap.kind, { type: "done", result: snap.result });
      }
    } catch {
      // fresh start
    }
  });

  function applyEnd(kind: string, end: { type: string; result?: unknown; message?: string }) {
    if (end.type !== "done") {
      const message = end.message ?? "任务失败";
      if (kind === "convert") convertError = message;
      else if (kind === "import-analyze") analyzeError = message;
      else if (kind === "import-execute") executeError = message;
      return;
    }
    if (kind === "convert") convertDone = end.result as ConvertResult;
    else if (kind === "import-analyze") plan = end.result as ImportPlan;
    else if (kind === "import-execute")
      executeResult = end.result as ExecuteResult;
  }

  const stepNumber = $derived(
    executeResult ? 4 : plan ? 3 : importReady ? 3 : needsConvert ? 2 : 1,
  );
</script>

<header class="mx-auto max-w-3xl px-4 pt-8 pb-2">
  <div class="flex items-center justify-between">
    <div>
      <h1 class="text-xl font-bold tracking-tight">dj-music-suite</h1>
      <p class="text-xs text-slate-500">本地曲库工具 · 数据不出你的电脑</p>
    </div>
    <div class="flex items-center gap-3">
      {#if libraryRoot}
        <span
          class="hidden max-w-56 truncate rounded-full bg-emerald-50 px-3 py-1 text-xs text-emerald-700 sm:inline-block"
          title={libraryRoot}
        >
          曲库 {fileBasename(libraryRoot)}
        </span>
      {:else}
        <span class="rounded-full bg-red-50 px-3 py-1 text-xs text-red-600">
          未设置曲库
        </span>
      {/if}
      <button
        class="rounded-lg border border-slate-300 px-3 py-1.5 text-sm hover:bg-slate-50"
        onclick={() => (settingsOpen = true)}
      >
        设置
      </button>
    </div>
  </div>
</header>

<main class="mx-auto max-w-3xl px-4 pb-24">
  <!-- 1 · 上传 -->
  <section class="rounded-2xl border border-slate-200 bg-white p-6">
    <h2 class="font-semibold">
      <span class="mr-2 text-slate-400">1</span>放入文件
    </h2>
    {#if stagingId === null}
      <div class="mt-3">
        <Dropzone onFiles={onFiles} />
        {#if uploading}
          <p class="mt-2 text-xs text-slate-500">正在上传…</p>
        {/if}
        {#if uploadError}
          <p class="mt-2 text-xs text-red-600">{uploadError}</p>
        {/if}
      </div>
    {:else}
      <div class="mt-3 flex flex-wrap items-center gap-3 text-sm">
        <span class="font-medium text-emerald-700">✓ 已接收</span>
        {#if counts.ncm}<span class="rounded-full bg-blue-50 px-2.5 py-0.5 text-xs text-blue-700">{counts.ncm} 个 NCM</span>{/if}
        {#if counts.audio}<span class="rounded-full bg-blue-50 px-2.5 py-0.5 text-xs text-blue-700">{counts.audio} 个音频</span>{/if}
        {#if counts.other}<span class="rounded-full bg-slate-100 px-2.5 py-0.5 text-xs text-slate-500">{counts.other} 个其它</span>{/if}
      </div>
      <button
        class="mt-3 text-xs text-slate-500 underline hover:text-slate-700"
        onclick={resetBatch}
        disabled={clearing || job.running}
      >
        重新选择文件
      </button>
    {/if}
  </section>

  <!-- 2 · 转换 -->
  {#if stagingId !== null && (needsConvert || convertDone || convertSkipped)}
    <section
      class="mt-4 rounded-2xl border border-slate-200 bg-white p-6 {stepNumber === 2 ? 'ring-2 ring-blue-200' : ''}"
    >
      <h2 class="font-semibold">
        <span class="mr-2 text-slate-400">2</span>转换 NCM
        {#if counts.ncm === 0}
          <span class="ml-1 text-xs font-normal text-slate-400">本批没有 .ncm，跳过</span>
        {/if}
      </h2>
      {#if needsConvert}
        <p class="mt-1 text-xs text-slate-500">
          把网易云的 .ncm 解密成 mp3/flac，自动写入标题、歌手、专辑、封面和歌词。
        </p>
        <div class="mt-3 flex items-center gap-4">
          <button
            class="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
            disabled={job.running}
            onclick={doConvert}
          >
            开始转换（{counts.ncm} 个）
          </button>
          <label class="flex items-center gap-1.5 text-xs text-slate-600">
            <input type="checkbox" class="accent-blue-600" bind:checked={noDownload} />
            不联网下载封面
          </label>
        </div>
        {#if convertError}
          <p class="mt-2 text-xs text-red-600">{convertError}</p>
        {/if}
        {#if job.running && job.kind === "convert"}
          <div class="mt-3">
            <Progress current={job.current} total={job.total} />
            <LogPanel lines={job.log} />
          </div>
        {/if}
      {:else if convertDone}
        <p class="mt-2 text-sm text-emerald-700">
          ✓ 转换完成：{convertDone.total - convertDone.failed} 个成功，{convertDone.tagged}
            个写入标签{convertDone.failed ? `，${convertDone.failed} 个失败（见日志）` : ""}
        </p>
      {:else if convertSkipped}
        <p class="mt-2 text-sm text-slate-500">已跳过转换</p>
      {/if}
    </section>
  {/if}

  <!-- 3 · 导入 -->
  {#if stagingId !== null && importReady}
    <section
      class="mt-4 rounded-2xl border border-slate-200 bg-white p-6 {stepNumber === 3 && !executeResult ? 'ring-2 ring-blue-200' : ''}"
    >
      <h2 class="font-semibold">
        <span class="mr-2 text-slate-400">3</span>导入曲库
      </h2>
      <p class="mt-1 text-xs text-slate-500">
        对照曲库查重，先预览，确认后才真正写入。
      </p>

      {#if !plan}
        <div class="mt-3 flex flex-wrap items-center gap-3">
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
            disabled={job.running || analyzeWaiting}
            onclick={doAnalyze}
          >
            分析
          </button>
        </div>
        {#if analyzeError}
          <p class="mt-2 text-xs text-red-600">{analyzeError}</p>
        {/if}
        {#if job.running && job.kind === "import-analyze"}
          <div class="mt-3">
            <Progress current={job.current} total={job.total} />
          </div>
        {/if}
      {:else}
        <PlanTable {plan} {decisions} onchange={decide} />
        <p class="mt-3 text-sm">
          <span class="font-medium">{newCount} 个文件</span>将{mode === "copy" ? "复制" : "移动"}进曲库{replacementCount
            ? `，并覆盖 ${replacementCount} 个重复/冲突文件`
            : ""}
        </p>
        <div class="mt-3 flex flex-wrap items-center gap-4">
          <label class="flex items-center gap-1.5 text-xs text-slate-600">
            <input
              type="radio"
              class="accent-blue-600"
              name="mode"
              value="copy"
              bind:group={mode}
            />
            复制（保留原文件）
          </label>
          <label class="flex items-center gap-1.5 text-xs text-slate-600">
            <input
              type="radio"
              class="accent-blue-600"
              name="mode"
              value="move"
              bind:group={mode}
            />
            移动（源文件删除）
          </label>
          <button
            class="ml-auto rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
            disabled={executing || newCount + replacementCount === 0}
            onclick={doExecute}
          >
            确认导入 {newCount + replacementCount} 个文件
          </button>
        </div>
        {#if executeError}
          <p class="mt-2 text-xs text-red-600">{executeError}</p>
        {/if}
        {#if job.running && job.kind === "import-execute"}
          <div class="mt-3">
            <Progress current={job.current} total={job.total} />
            <LogPanel lines={job.log} />
          </div>
        {/if}
      {/if}
    </section>
  {/if}

  <!-- 4 · 完成 -->
  {#if executeResult}
    <section class="mt-4 rounded-2xl border border-emerald-200 bg-emerald-50 p-6">
      <h2 class="font-semibold text-emerald-800">
        <span class="mr-2 text-emerald-400">4</span>导入完成
      </h2>
      <p class="mt-2 text-sm text-emerald-800">
        ✓ 写入 {executeResult.placed}
          个文件{executeResult.failed ? `，${executeResult.failed} 个失败（见日志）` : ""}。到
          rekordbox 里导入/刷新曲库根目录即可看到新歌。
      </p>
      <button
        class="mt-3 rounded-lg border border-emerald-300 bg-white px-4 py-2 text-sm text-emerald-700 hover:bg-emerald-100"
        onclick={resetBatch}
        disabled={clearing}
      >
        清理暂存文件，处理下一批
      </button>
    </section>
  {/if}
</main>

<footer class="pb-6 text-center text-xs text-slate-400">
  dj-music-suite web · 只监听本机 127.0.0.1
</footer>

<JobBar />
<SettingsModal
  open={settingsOpen}
  {libraryRoot}
  onSave={saveLibraryRoot}
  onClose={() => (settingsOpen = false)}
/>
