<script lang="ts">
  import { onMount } from "svelte";
  import { fileBasename, getConfig, setConfig } from "./lib/api";
  import { clearBatchAfterStorageCleanup, restore } from "./lib/batch.svelte";
  import { resetLibraryAfterStorageCleanup, restoreLibrary } from "./lib/library.svelte";
  import CoverPanel from "./lib/components/CoverPanel.svelte";
  import ConvertPanel from "./lib/components/ConvertPanel.svelte";
  import DedupPanel from "./lib/components/DedupPanel.svelte";
  import ImportPanel from "./lib/components/ImportPanel.svelte";
  import JobBar from "./lib/components/JobBar.svelte";
  import ReorgPanel from "./lib/components/ReorgPanel.svelte";
  import SettingsModal from "./lib/components/SettingsModal.svelte";
  import TranscodePanel from "./lib/components/TranscodePanel.svelte";

  type Tab = "convert" | "import" | "reorg" | "dedup" | "transcode" | "cover";

  const TABS: { id: Tab; label: string }[] = [
    { id: "convert", label: "转换" },
    { id: "import", label: "导入" },
    { id: "reorg", label: "整理" },
    { id: "dedup", label: "去重" },
    { id: "transcode", label: "转码" },
    { id: "cover", label: "补封面" },
  ];

  let tab = $state<Tab>("convert");
  let libraryRoot = $state<string | null>(null);
  let settingsOpen = $state(false);

  async function saveLibraryRoot(root: string) {
    const saved = await setConfig(root);
    libraryRoot = saved.library_root;
  }

  function handleDataCleared(configCleared: boolean) {
    clearBatchAfterStorageCleanup();
    resetLibraryAfterStorageCleanup();
    if (configCleared) libraryRoot = null;
  }

  onMount(async () => {
    try {
      const cfg = await getConfig();
      libraryRoot = cfg.library_root;
    } catch {
      // settings can be configured later
    }
    const focus = await restore();
    if (focus) tab = focus;
    // a reorg/dedup/transcode job or a plan analyzed earlier in this server
    // session
    const libraryFocus = await restoreLibrary();
    if (libraryFocus) tab = libraryFocus;
  });

  const tabButton = (active: boolean) =>
    `rounded-lg px-4 py-2 text-sm font-medium transition-colors ${
      active
        ? "bg-white text-slate-900 shadow-sm"
        : "text-slate-500 hover:text-slate-800"
    }`;

  const cardClass = (active: boolean) =>
    `mt-4 rounded-2xl border bg-white p-6 ${
      active ? "border-blue-300" : "border-slate-200"
    }`;
</script>

<header class="mx-auto max-w-3xl px-4 pt-8 pb-2">
  <div class="flex items-center justify-between">
    <div>
      <h1 class="text-xl font-bold tracking-tight">dj-music-suite</h1>
      <p class="text-xs text-slate-500">本地曲库工具 · 音频不会上传</p>
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
        <button
          class="rounded-full bg-red-50 px-3 py-1 text-xs text-red-600 hover:bg-red-100"
          onclick={() => (settingsOpen = true)}
        >
          未设置曲库 · 点此设置
        </button>
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

<nav class="mx-auto mt-4 max-w-3xl px-4">
  <div class="inline-flex gap-1 rounded-xl bg-slate-200/70 p-1">
    {#each TABS as t (t.id)}
      <button class={tabButton(tab === t.id)} onclick={() => (tab = t.id)}>
        {t.label}
      </button>
    {/each}
  </div>
</nav>

<main class="mx-auto max-w-3xl px-4 pb-24">
  {#if tab === "convert"}
    <section class={cardClass(true)}>
      <h2 class="font-semibold">转换 NCM</h2>
      <ConvertPanel />
    </section>
  {:else if tab === "import"}
    <section class={cardClass(true)}>
      <h2 class="font-semibold">导入曲库</h2>
      <ImportPanel goConvert={() => (tab = "convert")} />
    </section>
  {:else if tab === "reorg"}
    <section class={cardClass(true)}>
      <h2 class="font-semibold">整理曲库</h2>
      <ReorgPanel
        {libraryRoot}
        onNeedSettings={() => (settingsOpen = true)}
        goDedup={() => (tab = "dedup")}
      />
    </section>
  {:else if tab === "dedup"}
    <section class={cardClass(true)}>
      <h2 class="font-semibold">查找重复</h2>
      <DedupPanel {libraryRoot} onNeedSettings={() => (settingsOpen = true)} />
    </section>
  {:else if tab === "transcode"}
    <section class={cardClass(true)}>
      <h2 class="font-semibold">格式转码</h2>
      <TranscodePanel {libraryRoot} onNeedSettings={() => (settingsOpen = true)} />
    </section>
  {:else if tab === "cover"}
    <section class={cardClass(true)}>
      <h2 class="font-semibold">补专辑封面</h2>
      <CoverPanel {libraryRoot} onNeedSettings={() => (settingsOpen = true)} />
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
  onDataCleared={handleDataCleared}
/>
