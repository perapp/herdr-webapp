# Native program artwork

Copied without modification from `herdr-app/defaults/icons` at
`17eac390c346a731ba1ac14816ad91dc7cd2209b`. The companion
`../../lib/programs.catalog.json` is the matching unmodified
`herdr-app/defaults/programs.json`.

See `LICENSES.md`, `LICENSE-LobeHub.txt` and `LICENSE-Lucide.txt` for artwork
sources and license notices. Brands remain their owners' trademarks.

`lib/programs.ts` ports native `src/programs.rs` detection and
`src/window.rs` foreground-argv selection. The web client supports only this
bundled catalog, not a machine's custom programs.json or home-folder icons.
Icons identify programs/reported agent apps; model/provider names and status
colors are independent and are deliberately not used for detection.

`ProgramIcon.svelte` renders tint=true entries as masks using the caller's
text color, and tint=false entries as original-color images, as native headers
and agent rows do. All URLs are resolved from bundled SVGs, never terminal-
supplied URLs or filesystem paths. Render adjacent status indicators separately.
