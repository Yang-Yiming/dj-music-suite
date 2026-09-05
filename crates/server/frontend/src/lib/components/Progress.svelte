<script lang="ts">
  let { current = 0, total = 0 }: { current?: number; total?: number } = $props();

  const pct = $derived(total > 0 ? Math.min(100, (current / total) * 100) : 0);
</script>

<div class="flex items-center gap-3">
  <div class="h-2 flex-1 overflow-hidden rounded-full bg-slate-200">
    <div
      class="h-full rounded-full bg-blue-600 transition-all duration-200"
      style={`width:${pct}%`}
    ></div>
  </div>
  <span class="whitespace-nowrap text-xs text-slate-500 tabular-nums">
    {current} / {total || "?"}
  </span>
</div>
