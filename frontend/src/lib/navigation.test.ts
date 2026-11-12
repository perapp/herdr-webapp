import { describe, expect, it } from 'vitest';
import { paneTitle, resolvePane, selectCreated, selectPane, visiblePanes } from './navigation';
import { blankView } from './view';
import { viewKey } from './schema';
import type { LayoutNode, SharedDocument } from './schema';

const leaf = (id: string, hidden = false): LayoutNode => ({ id, type:'block', hidden, content:{ terminal:id }, children:[] });
const root = (): LayoutNode => ({ id:'root', type:'row', children:[leaf('one'), { id:'stack', type:'stack', active:'two', children:[leaf('two'), leaf('three'), leaf('hidden',true)] }, { id:'hidden-parent', type:'column', hidden:true, children:[leaf('secret')] }] });
function fixture(on: string | null = null): SharedDocument {
  return { on, version:2, spaces:[{ id:'workspace', name:'Work', folder:'/work', tabs:[{ id:'tab', name:'Shell', root:root() }] }] };
}
describe('terminal-first navigation', () => {
  it('selects a confirmed creation on the clicked computer/workspace/tab', () => {
    const view=blankView();view.space=viewKey(null,'old');
    expect(selectCreated(view,{on:'beta',space:'remote-space',tab:'other-tab',pane:'new-pane',terminal:'beta/t9'})).toBe(true);
    expect(view.space).toBe(viewKey('beta','remote-space'));
    expect(view.tabs[view.space]).toBe('other-tab');
    expect(view.focused[viewKey('beta','remote-space','other-tab')]).toBe('new-pane');
  });
  it('does not navigate on rename responses or malformed creation replies', () => {
    const view=blankView();const before=JSON.stringify(view);
    for (const result of [null,{}, {on:null,space:'s',tab:'t',pane:'p',terminal:null},{on:1,space:'s',tab:'t',pane:'p',terminal:'t9'}]) {
      expect(selectCreated(view,result)).toBe(false);
      expect(JSON.stringify(view)).toBe(before);
    }
  });

  it('lists all stack panes, excluding hidden leaves and hidden subtrees', () => {
    expect(visiblePanes(root()).map(p => p.id)).toEqual(['one','two','three']);
  });
  it('resolves stale/removed selection to a visible pane', () => {
    expect(resolvePane(root(),'gone',{},null)?.id).toBe('one');
    expect(resolvePane(root(),'hidden',{},null)?.id).toBe('one');
    expect(resolvePane(root(),'three',{},null)?.id).toBe('three');
    expect(resolvePane({ id:'r',type:'row',children:[] },'',{},null)).toBeUndefined();
  });
  it('honors private and shared stack selection when choosing a fallback', () => {
    const stack=root().children[1];
    expect(resolvePane(stack,'',{},null)?.id).toBe('two');
    expect(resolvePane(stack,'',{[viewKey(null,'stack')]:'three'},null)?.id).toBe('three');
  });
  it('selects a single pane privately, reveals its stack path and clears stale zoom', () => {
    const document=fixture(); const before=JSON.stringify(document); const view=blankView();
    view.zoomed[viewKey(null,'workspace','tab')]='one';
    expect(selectPane(view,document,document.spaces[0],document.spaces[0].tabs[0],'three')).toBe(true);
    expect(view.focused[viewKey(null,'workspace','tab')]).toBe('three');
    expect(view.stacks[viewKey(null,'stack')]).toBe('three');
    expect(view.zoomed[viewKey(null,'workspace','tab')]).toBeUndefined();
    expect(JSON.stringify(document)).toBe(before);
  });
  it('isolates identical workspace/tab/pane IDs on different computers', () => {
    const view=blankView();const local=fixture();const remote=fixture('beta');
    selectPane(view,local,local.spaces[0],local.spaces[0].tabs[0],'one');
    selectPane(view,remote,remote.spaces[0],remote.spaces[0].tabs[0],'three');
    expect(view.focused[viewKey(null,'workspace','tab')]).toBe('one');
    expect(view.focused[viewKey('beta','workspace','tab')]).toBe('three');
    const before=JSON.stringify(view);
    expect(selectPane(view,remote,remote.spaces[0],remote.spaces[0].tabs[0],'hidden')).toBe(false);
    expect(JSON.stringify(view)).toBe(before);
  });
  it('keeps unsupported leaves navigable and uses qualified terminal names safely', () => {
    const unsupported:LayoutNode={id:'mod',type:'block',content:{mod:'custom'},children:[]};
    expect(visiblePanes(unsupported)).toEqual([unsupported]);
    expect(paneTitle(unsupported,null,[],0)).toBe('Unavailable pane 1');
    expect(paneTitle(leaf('one'),'beta',[{address:'one',name:'Local'},{address:'beta/one',name:'Remote'}])).toBe('Remote');
  });
});
