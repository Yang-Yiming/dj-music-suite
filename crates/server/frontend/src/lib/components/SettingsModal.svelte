<script lang="ts">
  import {
    formatBytes,
    getStorage,
    startStorageClear,
    type StorageClearResult,
    type StorageInventory,
  } from "../api";
  import { job } from "../job.svelte";
  import { watchJob } from "../job.svelte";

  let {
    open,
    libraryRoot,
    onSave,
    onClose,
    onDataCleared,
  }: {
    open: boolean;
    libraryRoot: string;
    onSave: (root: string) => Promise<void>;
    onClose: () => void;
    onDataCleared: (configCleared: boolean) => void;
  } = $props();

  let value = $state("");
  let saving = $state(false);
  let error = $state("");

  let storage = $state<StorageInventory | null>(null);
  let storageLoading = $state(false);
  let storageError = $state("");
  let confirmingClear = $state(false);
  let clearConfig = $state(false);
  let clearing = $state(false);
  let clearResult = $state<StorageClearResult | null>(null);

  $effect(() => {
    if (open) {
      value = libraryRoot;
      error = "";
      confirmingClear = false;
      clearResult = null;
      void loadStorage();
    }
  });

  async function loadStorage() {
    storageLoading = true;
    storageError = "";
    try {
      storage = await getStorage();
    } catch (e) {
      storageError = e instanceof Error ? e.message : String(e);
    } finally {
      storageLoading = false;
    }
  }

  async function save() {
    saving = true;
    error = "";
    try {
      await onSave(value.trim());
      onClose();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      saving = false;
    }
  }

  async function clearAllLocalData() {
    clearing = true;
    storageError = "";
    clearResult = null;
    try {
      await startStorageClear({
        staging: true,
        cache: true,
        meta: true,
        config: clearConfig,
      });
      const end = await watchJob("storage-clear");
      if (end.type === "done") {
        clearResult = end.result as StorageClearResult;
        confirmingClear = false;
        if (clearConfig) value = "";
        onDataCleared(clearConfig);
        await loadStorage();
      } else {
        storageError = end.type === "error" ? end.message : "没有收到清理任务";
      }
    } catch (e) {
      storageError = e instanceof Error ? e.message : String(e);
    } finally {
      clearing = false;
    }
  }
</script>

{#if open}
  <div
    class="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/40 p-4"
    role="presentation"
    onclick={(e) => e.target === e.currentTarget && onClose()}
  >
    <div class="max-h-[90vh] w-full max-w-2xl overflow-y-auto rounded-2xl bg-white p-6 shadow-xl">
      <h2 class="text-base font-semibold">设置</h2>
      <p class="mt-1 text-xs text-slate-500">
        曲库位置保存在本机配置里，CLI 也会读取同一个值。
      </p>
      <input
        class="mt-4 w-full rounded-lg border border-slate-300 px-3 py-2 font-mono text-[13px] focus:border-blue-500 focus:ring-2 focus:ring-blue-200 focus:outline-none"
        type="text"
        placeholder="/Users/you/Music/DJ"
        bind:value={value}
        onkeydown={(e) => e.key === "Enter" && save()}
      />
      {#if error}
        <p class="mt-2 text-xs text-red-600">{error}</p>
      {/if}
      <div class="mt-4 flex justify-end gap-2">
        <button
          class="rounded-lg border border-slate-300 px-4 py-2 text-sm hover:bg-slate-50"
          onclick={onClose}
        >
          关闭
        </button>
        <button
          class="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
          disabled={saving || !value.trim()}
          onclick={save}
        >
          保存曲库位置
        </button>
      </div>

      <div class="my-6 border-t border-slate-200"></div>

      <div class="flex items-start justify-between gap-4">
        <div>
          <h3 class="text-sm font-semibold">本地数据</h3>
          <p class="mt-1 text-xs text-slate-500">
            包括 Web 暂存、封面搜索/图片缓存，以及 Web 创建或登记过的 meta 目录。不会删除曲库音频或用户指定输出目录。
          </p>
        </div>
        <button
          class="shrink-0 text-xs text-slate-500 underline hover:text-slate-700"
          disabled={storageLoading || clearing || job.running}
          onclick={loadStorage}
        >
          {storageLoading ? "统计中…" : "重新统计"}
        </button>
      </div>

      {#if storage}
        <div class="mt-3 rounded-xl border border-slate-200 bg-slate-50 p-4">
          <div class="flex items-baseline justify-between gap-3">
            <span class="text-sm font-medium text-slate-800">可清理总量</span>
            <span class="text-lg font-semibold text-slate-900">{formatBytes(storage.total_bytes)}</span>
          </div>
          <div class="mt-3 grid gap-2 text-xs text-slate-600 sm:grid-cols-2">
            <div class="rounded-lg bg-white px-3 py-2">
              <span class="font-medium text-slate-700">Web 暂存</span>
              <span class="float-right">{formatBytes(storage.staging.bytes)}</span>
              <span class="mt-1 block break-all font-mono text-[11px] text-slate-400">{storage.staging.path}</span>
            </div>
            <div class="rounded-lg bg-white px-3 py-2">
              <span class="font-medium text-slate-700">封面缓存</span>
              <span class="float-right">{formatBytes(storage.cache.bytes)}</span>
              <span class="mt-1 block break-all font-mono text-[11px] text-slate-400">{storage.cache.path}</span>
            </div>
            <div class="rounded-lg bg-white px-3 py-2">
              <span class="font-medium text-slate-700">meta 目录</span>
              <span class="float-right">{formatBytes(storage.meta.reduce((sum, item) => sum + item.bytes, 0))}</span>
              <span class="mt-1 block text-[11px] text-slate-400">{storage.meta.length} 个目录</span>
            </div>
            <div class="rounded-lg bg-white px-3 py-2">
              <span class="font-medium text-slate-700">配置文件</span>
              <span class="float-right">{formatBytes(storage.config_bytes)}</span>
              <span class="mt-1 block break-all font-mono text-[11px] text-slate-400">{storage.config_file}</span>
            </div>
          </div>

          {#if storage.meta.length}
            <details class="mt-3 text-xs text-slate-600">
              <summary class="cursor-pointer text-slate-500">查看 meta 路径</summary>
              <ul class="mt-2 max-h-32 space-y-1 overflow-y-auto font-mono text-[11px] text-slate-500">
                {#each storage.meta as item (item.path)}
                  <li class="break-all">
                    {item.path}
                    {#if !item.managed}<span class="ml-1 rounded bg-amber-100 px-1 text-amber-700">历史/未标记</span>{/if}
                  </li>
                {/each}
              </ul>
            </details>
          {/if}
        </div>
      {:else if storageLoading}
        <p class="mt-3 text-xs text-slate-500">正在统计本地数据…</p>
      {/if}

      {#if storageError}
        <p class="mt-2 text-xs text-red-600">{storageError}</p>
      {/if}

      {#if clearResult}
        <div class="mt-3 rounded-lg border border-emerald-200 bg-emerald-50 p-3 text-xs text-emerald-800">
          已清理 {formatBytes(clearResult.freed_bytes)}：
          {formatBytes(clearResult.staging_bytes)} 暂存、
          {formatBytes(clearResult.cache_bytes)} 缓存、
          {clearResult.meta_dirs} 个 meta 目录{clearResult.config_removed ? "，并清除了曲库设置" : ""}。
        </div>
      {/if}

      {#if !confirmingClear}
        <button
          class="mt-4 rounded-lg border border-red-300 bg-white px-4 py-2 text-sm font-medium text-red-700 hover:bg-red-50 disabled:opacity-50"
          disabled={job.running || clearing}
          onclick={() => (confirmingClear = true)}
        >
          清理全部 Web 本地数据
        </button>
      {:else}
        <div class="mt-4 rounded-lg border border-red-200 bg-red-50 p-4 text-sm text-red-800">
          <p class="font-medium">确认清理所有 Web 暂存和缓存？</p>
          <p class="mt-1 text-xs">
            会删除所有未导入的暂存文件、封面缓存，以及上方列出的 meta 目录。已经写入曲库或指定输出目录的文件不会被删除。
          </p>
          <label class="mt-3 flex items-center gap-2 text-xs">
            <input class="h-4 w-4 accent-red-600" type="checkbox" bind:checked={clearConfig} />
            同时删除配置文件并清除曲库位置设置
          </label>
          <div class="mt-4 flex justify-end gap-2">
            <button
              class="rounded-lg border border-red-200 bg-white px-3 py-1.5 text-xs hover:bg-red-100"
              disabled={clearing}
              onclick={() => (confirmingClear = false)}
            >
              取消
            </button>
            <button
              class="rounded-lg bg-red-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-red-700 disabled:opacity-50"
              disabled={clearing || job.running}
              onclick={clearAllLocalData}
            >
              {clearing ? "正在清理…" : "确认清理"}
            </button>
          </div>
        </div>
      {/if}
    </div>
  </div>
{/if}
