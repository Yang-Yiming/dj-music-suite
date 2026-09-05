<script lang="ts">
  import {
    DISPOSITION_LABELS,
    defaultIncluded,
    fileBasename,
    isReplacement,
    type Disposition,
    type ImportItem,
  } from "../api";

  let {
    plan,
    decisions,
    onchange,
  }: {
    plan: ImportPlan;
    decisions: Record<string, boolean>;
    onchange: (src: string, include: boolean) => void;
  } = $props();

  const PRIORITY: Disposition[] = [
    "duplicate",
    "conflict",
    "untagged",
    "alt-version",
    "new",
  ];

  const badgeClass: Record<Disposition, string> = {
    "new": "bg-emerald-100 text-emerald-700",
    "alt-version": "bg-blue-100 text-blue-700",
    "duplicate": "bg-amber-100 text-amber-700",
    "conflict": "bg-red-100 text-red-700",
    "untagged": "bg-slate-200 text-slate-600",
  };

  const sorted = $derived(
    [...plan.items].sort(
      (a, b) =>
        PRIORITY.indexOf(a.disposition) - PRIORITY.indexOf(b.disposition),
    ),
  );

  const dstLabel = (item: ImportItem): string => {
    const target =
      item.disposition === "duplicate" ? (item.replace ?? item.dst) : item.dst;
    return target ?? "";
  };
</script>

<div class="mt-3 max-h-80 overflow-y-auto rounded-lg border border-slate-200">
  <table class="w-full text-left text-[13px]">
    <thead class="sticky top-0 bg-slate-50 text-xs text-slate-500">
      <tr>
        <th class="px-3 py-2 font-medium">文件</th>
        <th class="px-3 py-2 font-medium">状态</th>
        <th class="px-3 py-2 font-medium">去向 / 说明</th>
        <th class="px-3 py-2 text-right font-medium">导入</th>
      </tr>
    </thead>
    <tbody>
      {#each sorted as item (item.src)}
        {@const included = decisions[item.src] ?? defaultIncluded(item)}
        <tr class="border-t border-slate-100 align-top">
          <td class="max-w-48 truncate px-3 py-2" title={item.src}>
            {fileBasename(item.src)}
          </td>
          <td class="px-3 py-2">
            <span
              class={`inline-block rounded-full px-2 py-0.5 text-xs whitespace-nowrap ${badgeClass[item.disposition]}`}
            >
              {DISPOSITION_LABELS[item.disposition]}
            </span>
          </td>
          <td class="px-3 py-2 text-slate-500">
            {#if item.disposition !== "duplicate" && dstLabel(item)}
              <span class="block max-w-72 truncate" title={dstLabel(item)}>
                {dstLabel(item).replace(plan.root, "~")}
              </span>
            {/if}
            {#if item.note}
              <span class="block max-w-72 truncate" title={item.note}>
                {item.note}
              </span>
            {/if}
          </td>
          <td class="px-3 py-2 text-right">
            <input
              type="checkbox"
              class="h-4 w-4 accent-blue-600"
              disabled={item.disposition === "untagged"}
              checked={included}
              onchange={(e) => onchange(item.src, e.currentTarget.checked)}
            />
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>
<p class="mt-2 text-xs text-slate-400">
  {#if plan.items.some(isReplacement)}
    勾选"重复/冲突"的文件表示用新文件覆盖曲库里的旧版本；不勾则保留原文件。
  {:else}
    取消勾选可以跳过单个文件。
  {/if}
</p>
