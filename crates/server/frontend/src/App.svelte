<script lang="ts">
  import { onMount } from "svelte";
  import { fileBasename, getConfig, setConfig } from "./lib/api";
  import { restore } from "./lib/batch.svelte";
  import ConvertPanel from "./lib/components/ConvertPanel.svelte";
  import ImportPanel from "./lib/components/ImportPanel.svelte";
  import JobBar from "./lib/components/JobBar.svelte";
  import SettingsModal from "./lib/components/SettingsModal.svelte";

  type Tab = "convert" | "import" | "reorg" | "dedup";

  const TABS: { id: Tab; label: string }[] = [
    { id: "convert", label: "转换" },
    { id: "import", label: "导入" },
    { id: "reorg", label: "整理" },
    { id: "dedup", label: "去重" },
  ];

  let tab = $state<Tab>("convert");
  let libraryRoot = $state<string | null>(null);
  let settingsOpen = $state(false);

  async function saveLibraryRoot(root: string) {
    const saved = await setConfig(root);
    libraryRoot = saved.library_root;
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
    <section class="mt-4 rounded-2xl border border-slate-200 bg-white p-6">
      <h2 class="font-semibold">整理曲库</h2>
      <p class="mt-2 text-sm text-slate-600">
        把曲库里的文件按标签布局重新归位（{`{artist}/{filename}.{ext}`} 之类的目录模板），
        先分析出移动计划，确认后执行，rekordbox 里一键 Relocate 即可。
      </p>
      <p class="mt-3 rounded-lg bg-slate-50 p-3 text-xs text-slate-500">
        Web 版开发中。当前可在终端使用：
        <code class="font-mono text-slate-700">dj-music-suite reorg --root &lt;曲库&gt;</code>
        （<code class="font-mono">--execute</code> 应用计划）
      </p>
    </section>
  {:else if tab === "dedup"}
    <section class="mt-4 rounded-2xl border border-slate-200 bg-white p-6">
      <h2 class="font-semibold">查找重复</h2>
      <p class="mt-2 text-sm text-slate-600">
        扫描曲库里的重复文件：内容完全相同的直接归组，同一首歌的多个版本按音质评分保留最好的一份，
        其余移入回收目录，确认无误后再删除。
      </p>
      <p class="mt-3 rounded-lg bg-slate-50 p-3 text-xs text-slate-500">
        Web 版开发中。当前可在终端使用：
        <code class="font-mono text-slate-700">dj-music-suite dedup --root &lt;曲库&gt;</code>
        （<code class="font-mono">--execute</code> 移入回收目录）
      </p>
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
