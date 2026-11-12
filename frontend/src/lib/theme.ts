import type { ITheme } from '@xterm/xterm';

/** Keep terminal defaults in step with the native-style light/dark chrome. */
export function terminalTheme(dark: boolean): ITheme {
  return dark ? {
    background:'#1e1e2e', foreground:'#cdd6f4', cursor:'#89b4fa', selectionBackground:'#45475a',
    black:'#45475a', brightBlack:'#585b70', red:'#f38ba8', green:'#a6e3a1', yellow:'#f9e2af',
    blue:'#89b4fa', magenta:'#cba6f7', cyan:'#94e2d5', white:'#bac2de',
    brightRed:'#f38ba8', brightGreen:'#a6e3a1', brightYellow:'#f9e2af', brightBlue:'#89b4fa', brightMagenta:'#cba6f7', brightCyan:'#94e2d5', brightWhite:'#cdd6f4',
  } : {
    background:'#eff1f5', foreground:'#4c4f69', cursor:'#1e66f5', selectionBackground:'#bccbed',
    black:'#5c5f77', brightBlack:'#6c6f85', red:'#d20f39', green:'#40a02b', yellow:'#df8e1d',
    blue:'#1e66f5', magenta:'#8839ef', cyan:'#179299', white:'#acb0be',
    brightRed:'#d20f39', brightGreen:'#40a02b', brightYellow:'#df8e1d', brightBlue:'#1e66f5', brightMagenta:'#8839ef', brightCyan:'#179299', brightWhite:'#bcc0cc',
  };
}
