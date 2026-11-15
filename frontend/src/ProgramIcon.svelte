<script lang="ts">
  import { programIconUrl } from './lib/programs';
  import type { ProgramAppearance } from './lib/programs';

  let { program, size = 16, color = 'currentColor', decorative = true }: {
    program: ProgramAppearance;
    size?: number;
    /** Native headers/agent rows tint glyphs with row text, not state-dot colors. */
    color?: string;
    /** Set false only when the adjacent text does not already name the program. */
    decorative?: boolean;
  } = $props();
  const url = $derived(programIconUrl(program));
  // Vite inlines SVGs with single-quoted XML attributes. Quote the entire
  // CSS URL as a string instead of nesting those apostrophes in single quotes.
  const mask = $derived(url ? `url(${JSON.stringify(url)})` : undefined);
  const pixels = $derived(Number.isFinite(size) ? Math.max(1, Math.min(128, size)) : 16);
</script>

{#if url}
  <span class="program-icon" style:width="{pixels}px" style:height="{pixels}px"
    role={decorative ? undefined : 'img'} aria-label={decorative ? undefined : program.name} aria-hidden={decorative ? 'true' : undefined}>
    {#if program.tint}
      <span class="glyph" style:background-color={color} style:mask-image={mask} style:-webkit-mask-image={mask}></span>
    {:else}
      <img src={url} alt="" width={pixels} height={pixels} draggable="false" />
    {/if}
  </span>
{/if}

<style>
  .program-icon { display: inline-flex; flex: none; vertical-align: middle; pointer-events: none; }
  .glyph { display: block; width: 100%; height: 100%; mask-repeat: no-repeat; mask-position: center; mask-size: contain; -webkit-mask-repeat: no-repeat; -webkit-mask-position: center; -webkit-mask-size: contain; }
  img { display: block; width: 100%; height: 100%; object-fit: contain; }
</style>
