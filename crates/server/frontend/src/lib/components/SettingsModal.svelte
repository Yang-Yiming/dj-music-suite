<script lang="ts">
  let {
    open,
    libraryRoot,
    onSave,
    onClose,
  }: {
    open: boolean;
    libraryRoot: string;
    onSave: (root: string) => Promise<void>;
    onClose: () => void;
  } = $props();

  let value = $state("");
  let saving = $state(false);
  let error = $state("");

  $effect(() => {
    if (open) {
      value = libraryRoot;
      error = "";
    }
  });

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
</script>

{#if open}
  <div
    class="fixed inset-0 z-50 flex items-center justify-center bg-slate-900/40 p-4"
    role="presentation"
    onclick={(e) => e.target === e.currentTarget && onClose()}
  >
    <div class="w-full max-w-lg rounded-2xl bg-white p-6 shadow-xl">
      <h2 class="text-base font-semibold">曲库位置</h2>
      <p class="mt-1 text-xs text-slate-500">
        存放你所有音乐的文件夹（rekordbox 使用的那个）。保存在本机配置里。
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
      <div class="mt-5 flex justify-end gap-2">
        <button
          class="rounded-lg border border-slate-300 px-4 py-2 text-sm hover:bg-slate-50"
          onclick={onClose}
        >
          取消
        </button>
        <button
          class="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
          disabled={saving || !value.trim()}
          onclick={save}
        >
          保存
        </button>
      </div>
    </div>
  </div>
{/if}
