<script lang="ts">
  import PaneTree from './PaneTree.svelte';
  import TerminalPane from './TerminalPane.svelte';
  import { agentStatus } from './lib/agentSeen';
  import { terminalProgram } from './lib/programs';
  import { detectCommandAgent } from './lib/commands';
  import { qualify, readable, terminalAddress, terminalName, viewKey } from './lib/schema';
  import type { LayoutNode, TerminalInfo } from './lib/schema';
  let { node, on, terminals, focused, stacks, focus, selectStack, compact = false, showKeys = false, visible = true }: {
    node: LayoutNode; on: string | null; terminals: TerminalInfo[]; focused: string;
    stacks: Record<string, string>; focus: (id: string) => void; selectStack: (key: string, id: string) => void; compact?: boolean; showKeys?: boolean; visible?: boolean;
  } = $props();
  let children = $derived(node.children.filter(c => !c.hidden));
  let stackKey = $derived(viewKey(on, node.id));
  let selected = $derived(children.find(c => c.id === stacks[stackKey]) || children.find(c => c.id === node.active) || children[0]);
  let address = $derived(typeof node.content?.terminal === 'string' ? qualify(node.content.terminal, on) : '');
  let terminal = $derived(terminals.find(t => terminalAddress(t) === address));
  let title = $derived(readable(node.title, terminalName(terminal)));
  let sizing = $derived(node.size?.px ? `0 0 ${Math.min(node.size.px, 4096)}px` : `${Math.min(node.size?.weight || 1, 1000)} 1 0%`);
</script>

{#if !node.hidden}
  <div class="layout-node" class:column={node.type === 'column'} class:row={node.type === 'row'} style:flex={compact ? '1 1 0%' : sizing}>
    {#if node.type === 'block'}
      {#if address}
        {#key address}<TerminalPane {address} {title} {compact} {showKeys} {visible} agent={detectCommandAgent(terminal)} program={terminalProgram(terminal)} reported={agentStatus(terminal)} active={focused === node.id} onfocus={() => focus(node.id)} />{/key}
      {:else}
        <section class="unsupported" aria-label={readable(node.title, 'Unavailable pane')}>
          <h3>{readable(node.title, typeof node.content?.mod === 'string' ? readable(node.content.mod, 'Unsupported module') : 'Unsupported pane')}</h3>
          <p>This pane is not supported in the browser. No module code is executed.</p>
          <p class="muted">Conversation transcripts are unavailable.</p>
        </section>
      {/if}
    {:else if node.type === 'stack'}
      <nav class="stack-tabs" aria-label="Pane group">
        {#each children as child, i (child.id || i)}
          <button class:chosen={selected?.id === child.id} aria-pressed={selected?.id === child.id} onclick={() => selectStack(stackKey, child.id)}>{readable(child.title, `Pane ${i + 1}`)}</button>
        {/each}
      </nav>
      {#if selected}
        <PaneTree node={selected} {on} {terminals} {focused} {stacks} {focus} {selectStack} {compact} {showKeys} {visible} />
      {:else}<p class="empty">This pane group is empty.</p>{/if}
    {:else}
      {#each children as child, i (child.id || i)}
        <PaneTree node={child} {on} {terminals} {focused} {stacks} {focus} {selectStack} {compact} {showKeys} {visible} />
      {:else}<p class="empty">No panes yet. Add a tab to start a terminal.</p>{/each}
    {/if}
    {#if node.floating?.length}<p class="floating-note">Floating panels are not supported in this browser.</p>{/if}
  </div>
{/if}
