<script lang="ts">
  import { fileBasename, formatBytes, formatDuration } from "../api";
  import {
    doMetadataScan,
    metadata,
    metadataDirty,
    resetMetadataDraft,
    saveMetadata,
    selectMetadataTrack,
  } from "../metadata.svelte";
  import { job } from "../job.svelte";
  import Progress from "./Progress.svelte";

  let { libraryRoot, onNeedSettings }: {
    libraryRoot: string | null;
    onNeedSettings: () => void;
  } = $props();

  const filteredTracks = $derived.by(() => {
    const query = metadata.search.trim().toLowerCase();
    if (!query) return metadata.tracks;
    return metadata.tracks.filter((track) =>
      [track.path, track.title, track.artist, track.album, track.album_artist]
        .filter(Boolean)
        .some((value) => value!.toLowerCase().includes(query)),
    );
  });

  const dirty = $derived(metadataDirty());

  const fieldLabels = {
    title: "标题",
    artist: "歌手",
    album: "专辑",
    album_artist: "专辑歌手",
    year: "年份 / 日期",
    genre: "流派",
    track_number: "音轨号",
    disc_number: "碟号",
  } as const;
</script>

<p class="mt-2 text-sm text-slate-600">
  扫描单首歌曲或整个文件夹，在列表里搜索并逐首编辑标签。文字保存不会改动封面，也不会重命名文件。
  当前支持 MP3、FLAC、M4A。
</p>

<div class="mt-4 flex flex-col gap-2 sm:flex-row">
  <input
    class="min-w-0 flex-1 rounded-lg border border-slate-300 px-2.5 py-1.5 font-mono text-xs focus:border-blue-500 focus:outline-none"
    type="text"
    placeholder={libraryRoot ?? "歌曲或文件夹的绝对路径"}
    bind:value={metadata.input}
    onkeydown={(event) => {
      if (event.key === "Enter" && !job.running) doMetadataScan(libraryRoot);
    }}
  />
  <button
    class="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
    disabled={job.running || metadata.scanning}
    onclick={() => doMetadataScan(libraryRoot)}
  >
    {metadata.scanning ? "扫描中…" : "扫描"}
  </button>
</div>
<p class="mt-1 text-xs text-slate-400">
  留空会扫描设置中的曲库。也可以直接填入一个音频文件来只编辑这一首。
  {#if !libraryRoot}
    <button class="ml-1 underline hover:text-slate-600" onclick={onNeedSettings}>去设置曲库</button>
  {/if}
</p>

{#if metadata.error}
  <p class="mt-2 text-xs text-red-600">{metadata.error}</p>
{/if}
{#if job.running && job.kind === "metadata-scan"}
  <div class="mt-3">
    <Progress current={job.current} total={job.total} />
  </div>
{/if}

{#if metadata.tracks.length}
  <div class="mt-5 grid gap-4 lg:grid-cols-[minmax(0,1.35fr)_minmax(19rem,0.9fr)]">
    <section class="min-w-0 rounded-xl border border-slate-200">
      <div class="border-b border-slate-200 p-3">
        <div class="flex items-center justify-between gap-3">
          <span class="text-xs text-slate-500">{filteredTracks.length} / {metadata.tracks.length} 首</span>
          <a class="text-xs text-slate-400 hover:text-slate-600" href="#metadata-editor">编辑区</a>
        </div>
        <input
          class="mt-2 w-full rounded-lg border border-slate-300 px-2.5 py-1.5 text-sm focus:border-blue-500 focus:outline-none"
          type="search"
          placeholder="搜索文件名、标题、歌手或专辑"
          bind:value={metadata.search}
        />
      </div>
      <div class="max-h-[32rem] overflow-y-auto">
        {#each filteredTracks as track (track.path)}
          <button
            class={`flex w-full items-start gap-3 border-b border-slate-100 px-3 py-2.5 text-left last:border-b-0 hover:bg-slate-50 ${
              metadata.selectedPath === track.path ? "bg-blue-50/80" : ""
            }`}
            onclick={() => selectMetadataTrack(track.path)}
          >
            <div class="mt-0.5 w-10 shrink-0 rounded bg-slate-100 px-1 py-0.5 text-center text-[10px] font-medium uppercase text-slate-500">
              {track.format}
            </div>
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-2">
                <span class="truncate text-sm font-medium text-slate-800">{track.title || fileBasename(track.path)}</span>
                {#if track.error}
                  <span class="shrink-0 rounded-full bg-red-100 px-1.5 py-0.5 text-[10px] text-red-700">读取失败</span>
                {:else if track.has_cover}
                  <span class="shrink-0 rounded-full bg-emerald-100 px-1.5 py-0.5 text-[10px] text-emerald-700">有封面</span>
                {/if}
              </div>
              <span class="mt-0.5 block truncate text-xs text-slate-500">
                {track.artist || "未知歌手"}{track.album ? ` · ${track.album}` : ""}
              </span>
              <span class="mt-0.5 block truncate font-mono text-[10px] text-slate-400">{fileBasename(track.path)} · {formatDuration(track.duration_secs)}</span>
            </div>
          </button>
        {:else}
          <p class="p-6 text-center text-sm text-slate-400">没有匹配的歌曲</p>
        {/each}
      </div>
    </section>

    <section id="metadata-editor" class="min-w-0 rounded-xl border border-slate-200 p-4">
      {#if metadata.original}
        <div class="flex items-start justify-between gap-3">
          <div class="min-w-0">
            <p class="truncate text-sm font-semibold" title={metadata.original.path}>
              {fileBasename(metadata.original.path)}
            </p>
            <p class="mt-0.5 truncate font-mono text-[10px] text-slate-400" title={metadata.original.path}>
              {metadata.original.path}
            </p>
          </div>
          {#if dirty}
            <span class="shrink-0 rounded-full bg-amber-100 px-2 py-0.5 text-[10px] font-medium text-amber-700">未保存</span>
          {:else}
            <span class="shrink-0 rounded-full bg-slate-100 px-2 py-0.5 text-[10px] text-slate-500">已同步</span>
          {/if}
        </div>

        {#if metadata.original.error}
          <p class="mt-3 rounded-lg bg-red-50 p-2 text-xs text-red-700">{metadata.original.error}</p>
        {/if}

        <div class="mt-3 flex flex-wrap gap-x-4 gap-y-1 text-[11px] text-slate-400">
          <span>{metadata.original.format.toUpperCase()}</span>
          <span>{formatDuration(metadata.original.duration_secs)}</span>
          <span>{formatBytes(metadata.original.size)}</span>
          <span>{metadata.original.has_cover ? "内嵌封面保持不变" : "无内嵌封面"}</span>
        </div>

        <div class="mt-4 grid gap-3 sm:grid-cols-2 lg:grid-cols-1 xl:grid-cols-2">
          <label class="flex flex-col gap-1 text-xs text-slate-600">
            {fieldLabels.title}
            <input class="rounded-lg border border-slate-300 px-2.5 py-1.5 text-sm focus:border-blue-500 focus:outline-none" bind:value={metadata.draft.title} />
          </label>
          <label class="flex flex-col gap-1 text-xs text-slate-600">
            {fieldLabels.artist}
            <input class="rounded-lg border border-slate-300 px-2.5 py-1.5 text-sm focus:border-blue-500 focus:outline-none" bind:value={metadata.draft.artist} />
          </label>
          <label class="flex flex-col gap-1 text-xs text-slate-600">
            {fieldLabels.album}
            <input class="rounded-lg border border-slate-300 px-2.5 py-1.5 text-sm focus:border-blue-500 focus:outline-none" bind:value={metadata.draft.album} />
          </label>
          <label class="flex flex-col gap-1 text-xs text-slate-600">
            {fieldLabels.album_artist}
            <input class="rounded-lg border border-slate-300 px-2.5 py-1.5 text-sm focus:border-blue-500 focus:outline-none" bind:value={metadata.draft.album_artist} />
          </label>
          <label class="flex flex-col gap-1 text-xs text-slate-600">
            {fieldLabels.year}
            <input class="rounded-lg border border-slate-300 px-2.5 py-1.5 text-sm focus:border-blue-500 focus:outline-none" placeholder="例如 2026" bind:value={metadata.draft.year} />
          </label>
          <label class="flex flex-col gap-1 text-xs text-slate-600">
            {fieldLabels.genre}
            <input class="rounded-lg border border-slate-300 px-2.5 py-1.5 text-sm focus:border-blue-500 focus:outline-none" placeholder="例如 House" bind:value={metadata.draft.genre} />
          </label>
          <label class="flex flex-col gap-1 text-xs text-slate-600">
            {fieldLabels.track_number}
            <input class="rounded-lg border border-slate-300 px-2.5 py-1.5 text-sm focus:border-blue-500 focus:outline-none" placeholder="例如 3" bind:value={metadata.draft.track_number} />
          </label>
          <label class="flex flex-col gap-1 text-xs text-slate-600">
            {fieldLabels.disc_number}
            <input class="rounded-lg border border-slate-300 px-2.5 py-1.5 text-sm focus:border-blue-500 focus:outline-none" placeholder="例如 1" bind:value={metadata.draft.disc_number} />
          </label>
        </div>

        <p class="mt-2 text-[11px] text-slate-400">把字段留空会清除该标签。保存前会重新读取文件，扫描后文件有变化则不会写入。</p>
        <div class="mt-3 flex flex-wrap items-center gap-2">
          <button
            class="rounded-lg bg-emerald-600 px-4 py-2 text-sm font-medium text-white hover:bg-emerald-700 disabled:opacity-50"
            disabled={!dirty || job.running || metadata.saving}
            onclick={saveMetadata}
          >
            {metadata.saving ? "保存中…" : "保存修改"}
          </button>
          <button
            class="rounded-lg border border-slate-300 px-3 py-2 text-sm text-slate-600 hover:bg-slate-50 disabled:opacity-50"
            disabled={!dirty || metadata.saving}
            onclick={resetMetadataDraft}
          >
            撤销修改
          </button>
        </div>
        {#if metadata.writeError}
          <p class="mt-2 text-xs text-red-600">{metadata.writeError}</p>
        {/if}
        {#if job.running && job.kind === "metadata-write"}
          <p class="mt-2 text-xs text-slate-500">正在写入并校验标签…</p>
        {/if}
        {#if metadata.savedPath === metadata.original.path}
          <p class="mt-2 rounded-lg bg-emerald-50 px-3 py-2 text-xs text-emerald-700">
            已保存到文件。到 rekordbox 里刷新该曲目即可读取新标签。
          </p>
        {/if}
      {:else}
        <p class="text-sm text-slate-400">从左侧列表选择一首歌开始编辑。</p>
      {/if}
    </section>
  </div>
{/if}
