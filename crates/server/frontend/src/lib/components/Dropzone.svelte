<script lang="ts">
  let { onFiles }: { onFiles: (files: File[]) => void } = $props();

  let drag = $state(false);
  let input: HTMLInputElement | undefined = $state();

  function picked() {
    if (!input?.files?.length) return;
    onFiles(Array.from(input.files));
    input.value = "";
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
  ondrop={(e) => {
    e.preventDefault();
    drag = false;
    onFiles(Array.from(e.dataTransfer!.files));
  }}
>
  <p class="text-slate-500">
    拖拽文件到这里，或<span class="mx-1 font-medium text-blue-600">点击选择</span
    >，可多选
  </p>
  <p class="mt-1 text-xs text-slate-400">
    支持网易云 .ncm，以及 mp3 / flac / m4a / wav 等音频
  </p>
  <input
    bind:this={input}
    type="file"
    multiple
    class="hidden"
    accept=".ncm,.mp3,.flac,.m4a,.aac,.wav,.aiff,.aif"
    onchange={picked}
  />
</div>
