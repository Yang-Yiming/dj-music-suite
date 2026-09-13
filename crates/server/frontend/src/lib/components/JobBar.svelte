<script lang="ts">
  import { job } from "../job.svelte";
  import Progress from "./Progress.svelte";

  const LABELS: Record<string, string> = {
    convert: "正在转换 NCM",
    "import-analyze": "正在分析导入",
    "import-execute": "正在写入曲库",
    "reorg-analyze": "正在分析整理计划",
    "reorg-execute": "正在整理曲库",
    "dedup-analyze": "正在扫描重复文件",
    "dedup-execute": "正在移出重复文件",
    "trash-restore": "正在放回文件",
    "trash-delete": "正在删除回收站文件",
  };
</script>

{#if job.running && job.kind}
  <div
    class="fixed right-4 bottom-4 z-40 w-80 rounded-xl border border-slate-200 bg-white p-4 shadow-lg"
  >
    <p class="mb-2 flex items-center gap-2 text-sm font-medium">
      <span
        class="inline-block h-2 w-2 animate-pulse rounded-full bg-blue-600"
      ></span>
      {LABELS[job.kind] ?? job.kind}
    </p>
    <Progress current={job.current} total={job.total} />
    {#if job.log.length}
      <p class="mt-2 truncate text-xs text-slate-400" title={job.log.at(-1)?.text}>
        {job.log.at(-1)?.text}
      </p>
    {/if}
  </div>
{/if}
