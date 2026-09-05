<script lang="ts">
  let {
    onFiles,
    folder = false,
  }: {
    onFiles: (files: { file: File; path: string }[]) => void;
    /** also offer a folder picker (and accept dropped folders) */
    folder?: boolean;
  } = $props();

  let drag = $state(false);
  let input: HTMLInputElement | undefined = $state();
  let folderInput: HTMLInputElement | undefined = $state();

  async function handleFiles(list: FileList | File[]) {
    const arr = Array.from(list);
    if (!arr.length) return;
    onFiles(arr.map((file) => ({ file, path: file.name })));
  }

  async function handlePicked() {
    if (!input?.files?.length) return;
    await handleFiles(input.files);
    input.value = "";
  }

  async function handleFolderPicked() {
    if (!folderInput?.files?.length) return;
    const items = Array.from(folderInput.files).map((file) => {
      const rel = (file as File & { webkitRelativePath?: string }).webkitRelativePath;
      return { file, path: rel || file.name };
    });
    onFiles(items);
    folderInput.value = "";
  }

  // Dropped directories arrive as entries, not files: walk the tree.
  async function walkEntry(entry: any, prefix: string, out: { file: File; path: string }[]) {
    if (entry.isFile) {
      const file: File = await new Promise((res, rej) => entry.file(res, rej));
      out.push({ file, path: prefix + entry.name });
    } else if (entry.isDirectory) {
      const reader = entry.createReader();
      let kids: any[] = [];
      do {
        kids = await new Promise((res, rej) => reader.readEntries(res, rej));
        for (const kid of kids) await walkEntry(kid, `${prefix}${entry.name}/`, out);
      } while (kids.length);
    }
  }

  async function handleDrop(e: DragEvent) {
    e.preventDefault();
    drag = false;
    const out: { file: File; path: string }[] = [];
    const entries = Array.from(e.dataTransfer?.items ?? [])
      .map((item) => (item as any).webkitGetAsEntry?.())
      .filter(Boolean);
    if (entries.length) {
      for (const entry of entries) await walkEntry(entry, "", out);
      if (out.length) onFiles(out);
      return;
    }
    await handleFiles(e.dataTransfer?.files ?? []);
  }
</script>

<div
  role="button"
  tabindex="0"
  class={`cursor-pointer rounded-xl border-2 border-dashed p-8 text-center transition-colors ${
    drag
      ? "border-blue-500 bg-blue-50"
      : "border-slate-300 hover:border-slate-400"
  }`}
  onclick={() => input?.click()}
  onkeydown={(e) => e.key === "Enter" && input?.click()}
  ondragover={(e) => {
    e.preventDefault();
    drag = true;
  }}
  ondragleave={() => (drag = false)}
  ondrop={handleDrop}
>
  <p class="text-slate-500">
    拖拽{folder ? "文件或整个文件夹" : "文件"}到这里，或<span
      class="mx-1 font-medium text-blue-600">点击选择</span
    >，可多选
  </p>
  <p class="mt-1 text-xs text-slate-400">
    支持网易云 .ncm，以及 mp3 / flac / m4a / wav 等音频{folder
      ? "；文件夹会保留结构（.lrc 歌词、meta 封面随行）"
      : ""}
  </p>
  {#if folder}
    <button
      class="mt-3 rounded-lg border border-slate-300 px-3 py-1.5 text-xs text-slate-600 hover:bg-slate-50"
      onclick={(e) => {
        e.stopPropagation();
        folderInput?.click();
      }}
    >
      选择文件夹
    </button>
  {/if}
  <input
    bind:this={input}
    type="file"
    multiple
    class="hidden"
    accept=".ncm,.mp3,.flac,.m4a,.aac,.wav,.aiff,.aif,.lrc"
    onchange={handlePicked}
  />
  <input
    bind:this={folderInput}
    type="file"
    class="hidden"
    webkitdirectory
    directory=""
    multiple
    onchange={handleFolderPicked}
  />
</div>
