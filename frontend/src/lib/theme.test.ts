import { describe, expect, it } from 'vitest';
import { terminalTheme } from './theme';
import { terminalUrl } from './terminal';
describe('native appearance',()=>{
  it('uses a readable light terminal matching native-style chrome',()=>{
    expect(terminalTheme(false)).toMatchObject({background:'#eff1f5',foreground:'#4c4f69',cursor:'#1e66f5'});
    expect(terminalUrl('t1',80,24,'http://host:8001/').searchParams.get('theme')).toBe('light');
  });
  it('negotiates matching dark viewer display facts',()=>{
    expect(terminalTheme(true)).toMatchObject({background:'#1e1e2e',foreground:'#cdd6f4'});
    expect(terminalUrl('t1',80,24,'http://host:8001/','dark').searchParams.get('theme')).toBe('dark');
  });
});
