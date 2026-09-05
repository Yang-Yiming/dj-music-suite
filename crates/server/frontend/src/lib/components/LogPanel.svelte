<script lang="ts">
  import type { LogLine } from "../job.svelte";

  let { lines }: { lines: LogLine[] } = $props();

  let el: HTMLPreElement | undefined = $state();

  $effect(() => {
    lines.length;
    if (el) el.scrollTop = el.scrollHeight;
  });
</script>

<pre
  bind:this={el}
  class="mt-3 max-h-44 overflow-y-auto rounded-lg bg-slate-900 p-3 font-mono text-xs leading-5 break-all whitespace-pre-wrap text-slate-300"
>{#each lines as line (line.text + line.warn)}{#if line.warn}<span
      class="text-amber-400">{line.text}
</span>{:else}{line.text}
{/if}{/each}</pre>
