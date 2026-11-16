# Herdr Web on a phone

How the web app behaves under your fingers: scrolling, gestures, the key rows,
selection, agent commands and attaching images. Installation, hosting, security
and development are in the [usage guide](usage.md).

## Touch scrolling

Swipe vertically inside a pane to scroll terminal history. Local history moves
by pixels rather than whole rows, flings glide like a web page and settle on a row.
In Pi and other mouse-aware terminal applications, swipes send negotiated wheel
reports instead, paced at most three per animation frame. A flick coasts for a
moment with decaying speed and a bounded backlog; touching the screen again
stops it at once, as do disconnects and mode changes. These programs still
scroll by whole rows and redraw per step.
Swiping does not open the keyboard. Tap to type. The keyboard accessory strip
also offers **PgUp/PgDn** (open it with the keyboard button on phones).
Applications without mouse support require their own keys; swipes never guess
Up/Down arrow input. Pinch zoom is left to the browser, and reduced-motion
preferences disable momentum.

## Phone gestures, keys and closing

- **Accessory keys** appear with the on-screen keyboard and hide with it. The
  keyboard button overrides that until the keyboard next opens or closes.
  Keyboard detection is inferred from viewport height on touch devices and
  needs validation on physical iOS/Android devices.
- **All ten keys fit one row** on a phone (Esc, Tab, Ctrl, Alt, Ctrl C, the
  four arrows, PgUp and PgDn); nothing is hidden behind a sideways scroll.
- **Row menus.** In the drawer, each workspace and tab row and the selected
  pane row has a **⋯** button for rename, split and close. Long press on a row
  opens the same menu.
- **Settings** (drawer footer) is a full-screen sheet on phones. *Text size*
  shows a sample and what the size means for the pane in view, such as
  "13 px · 44 columns × 29 rows". *Terminal* has three switches, stored in this
  browser only and all on by default: **Keys with the keyboard**, **Steady size
  under the keyboard** and **Smooth drawing** (GPU). Turn the last one off if
  text looks wrong on a device. The two keyboard switches are hidden on
  devices without a touch screen. *About → Version* names the loaded build,
  for example "Main · 261115…-1bcc505" or "Development server · …".
- **Long press selects text.** Hold a word for about half a second to select
  it, keep dragging to extend the selection, then use the **Copy** / **×** bar
  that appears above your finger (below the handles near the top of a pane). Terminal text is drawn on a GPU canvas
  (falling back to page elements without WebGL2), so the browser's own text
  selection does not apply. After release, drag the handle at either end to
  adjust the selection. A selection cannot extend past the visible screen.
- **The keyboard does not resize the terminal.** While the on-screen keyboard
  is up, the grid keeps its full size and the pane shows the part that holds
  the cursor (the newest rows while reading history). Resizing made
  full-screen programs redraw, and agents that redraw in place (Claude Code)
  left duplicated text in history when it happened mid-answer. Rotation, font
  size and manually toggling the key row still resize. A pane first attached
  while the keyboard is already open has no full-size grid yet and resizes once.
- **Ctrl** and **Alt** latch for the next key only (Ctrl then `r` sends Ctrl+R;
  Alt then ← sends the xterm modified-arrow sequence). A latched Ctrl+M/Ctrl+J
  is not a way to submit: use Enter or **Ctrl J**. Held arrow keys repeat.
- **Copy** copies the selection, or the visible screen when nothing is selected.
  **Paste** inserts clipboard text without pressing Enter; it is offered only
  where the browser can read the clipboard (HTTPS or localhost, not the plain
  HTTP LAN URL).
- A round **↓** button appears while scrolled up in history and returns to the
  latest output.
- **Swipe the top bar** left/right for the next/previous pane of the workspace
  (tabs in order, wrapping). It never focuses input or opens the keyboard.
  **Long-press the top bar** for the current pane's actions.
- **Pane switching is instant on phones.** The six most recently shown panes
  stay mounted and attached while another is on screen, so returning needs no
  new stream or replay and keeps the scroll position. Kept panes still receive
  output (and use bandwidth); they are not laid out, never take input, and
  propose their viewport again only when shown. The least recently shown pane
  beyond six is released, as are all of them on leaving the phone layout.
- **Long-press** (or right-click / the Menu key) a workspace, tab or pane in the
  drawer for rename, split and close. The visible **+** still creates.
- **Close pane / tab / workspace** follow the desktop: the last pane closes its
  tab and the last tab its workspace, after a confirmation that states how
  many terminals end. Closing ends those programs and their sub-agents (except
  sub-agents kept in a tab of their own) and cannot be undone. Programs are
  ended before the shared layout is written; if a running desktop removes the
  same pane first, that newer layout is accepted instead of failing.

## Agent commands and favorites

Open the accessory keys and tap **/** beside the paperclip. The command palette
uses the terminal's live core-reported app identity to offer built-in shortcuts
for Pi, Claude, Codex and OpenCode. Pi's loaded extension commands (such as
`/git-mode`), prompt templates and skill commands are also listed when the native
Pi publisher is installed. It is hidden for unrecognized apps and shells;
it does not infer an agent from terminal text or pane names.

Star commands to keep them at the top, or choose **Favorites only**. Favorites
are stored per agent in this browser, not in the shared workspace. Select a
command to edit its arguments, then **Insert** to paste without Enter, or
explicitly **Run** to paste and press Enter. Use an empty terminal prompt for
Run: the existing draft is never cleared. Selection, search and starring do not
send terminal input. A changed connection or reported agent requires reopening
the palette; commands are never queued across reconnects.

The built-in shortcuts are static and version-dependent. Pi's custom command
list comes from `pi.getCommands()` inside the running session, not from scanning
extension source or reading transcripts. Typed command arguments are not saved
as favorites. Favorites for temporarily unavailable commands are retained but
aren't shown or executed until that command is available again.

Install the native Pi publisher on each computer where Pi runs:

```sh
./scripts/install-pi-extension.sh
```

Then use **`/reload` once in existing Pi sessions**, or start a new session. Open
or refresh the Commands palette afterward. The installer adds a separate
`herdr-webapp-commands.ts` symlink under Pi's global extensions directory and
never replaces the older remote client's publisher or another existing file.
`PI_CODING_AGENT_DIR` can select an alternate absolute Pi configuration directory.
Keep this checkout in place while its symlink is installed.

The publisher uses persistent native Core RPC to publish bounded, expiring
command metadata in a dedicated per-terminal versioned document. Discovery does
not type into terminals, execute commands, read session files or use SSH helpers.
Configured remote machines work through native Core document routing; inbound-only
peers cannot provide this catalog. Built-ins remain usable if discovery is
missing, disabled (`HERDR_COMMAND_DISCOVERY=0`), stale or unsupported.
If a `herdr-core/config.toml` file exists, set `HC_SOCKET` explicitly when starting
Pi; the publisher deliberately does not parse TOML configuration. Without that
file, standard Linux/macOS socket discovery works without `HC_SOCKET`.

## Attach an image to a Pi prompt

Open the keyboard accessory panel with the top-right keyboard button, then use
**Attach image** on its second row. Choose a PNG, JPEG, GIF or WebP image up to
8 MiB. The key-sized paperclip sits at the right of the second row and inserts
its path automatically after upload. Add your message and send it;
insertion never presses Enter or opens the keyboard automatically. This follows Pi's clipboard-image
pattern: stage a file and paste its path; Pi can read it with its file tool.
It is not a direct model-image attachment or a terminal image preview.

Uploads use core's native `terminal.put` on the selected terminal's machine,
including configured remote machines; unsupported peers fail explicitly without
uploading locally. There is no user-supplied destination path or filename.
Files live in core's terminal staging area and are removed when that terminal
record is removed. Privacy relies on core's private state-directory ancestor;
files are never exposed through a public download URL. SVG and HEIC are not
supported; export them to a supported image format first.

Automatic insertion is tied to the trusted file selection and its exact live
connection. If the connection changes during upload, the path is not inserted
or replayed after reconnect; the UI asks you to attach again when connected. Leaving a pane aborts
its browser upload, but an upload already processed by core may remain staged.
Failed/uncertain uploads are not retried automatically. Two uploads can run at
once; staging is capped at 64 MiB per terminal and 256 MiB per web-server run.
These are accident-prevention limits, not a durable disk quota. Core terminal
cleanup or manual removal manages longer-lived staged files.
