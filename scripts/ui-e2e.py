#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["playwright>=1.47,<2"]
# ///
"""Phone and desktop browser checks against a disposable Core.

Covers behaviour that unit tests without a DOM cannot see: the keyboard not
resizing the terminal, frame-level scrolling, canvas text selection, image
attach surviving the keyboard closing, the key row fitting a phone, row menus
and Settings. Each of these regressed once while the rest of the gate passed.

Starts `scripts/dev.py` (Vite + an isolated Core; never the user's Core) on
free loopback ports and drives the system Chrome headlessly. Never prints the
access token or sign-in link. Synthetic touch does not prove real keyboards.
"""
import argparse
import json
import os
from pathlib import Path
import re
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time
import zlib

from playwright.sync_api import sync_playwright

PROJECT = Path(__file__).resolve().parent.parent
VIS = '.kept-pane:not([hidden])'
failures = []


def check(name, ok, detail=''):
    if not ok:
        failures.append(name)
    print(('PASS ' if ok else 'FAIL ') + name + (f'  [{detail}]' if detail and not ok else ''), flush=True)


def free_port():
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        return probe.getsockname()[1]


def png(path):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data) & 0xffffffff)
    path.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 1, 1, 8, 2, 0, 0, 0))
                     + chunk(b'IDAT', zlib.compress(b'\x00\xff\x00\x00')) + chunk(b'IEND', b''))


class Phone:
    """A Pixel-sized page. `gpu=False` keeps terminal text in the DOM so it can be read."""

    def __init__(self, playwright, browser, url, gpu):
        self.context = browser.new_context(**playwright.devices['Pixel 7'])
        if not gpu:
            self.context.add_init_script("try{localStorage.setItem('herdr.terminal-prefs','{\"gpu\":false}')}catch{}")
        self.page = self.context.new_page()
        self.cdp = self.context.new_cdp_session(self.page)
        self.errors = []
        self.resizes = []
        self.uploads = []
        self.page.on('pageerror', lambda error: self.errors.append(str(error)[:200]))
        self.page.on('websocket', self.socket)
        self.page.on('response', lambda r: self.uploads.append(r.status) if '/api/attachments' in r.url else None)
        self.page.goto(url)
        self.page.wait_for_selector('.terminal-host .xterm', timeout=30000)
        self.page.wait_for_function("document.querySelector('.terminal-detail')===null", timeout=20000)
        self.width, self.height = self.page.viewport_size['width'], self.page.viewport_size['height']
        self.page.click(f'{VIS} .terminal-host')
        time.sleep(0.4)

    def socket(self, ws):
        if '/api/terminal' in ws.url:
            ws.on('framesent', lambda frame: self.resizes.append(frame) if isinstance(frame, str) and '"resize"' in frame else None)

    def close(self):
        self.context.close()

    def keyboard(self, up):
        self.page.set_viewport_size({'width': self.width, 'height': self.height - (330 if up else 0)})
        time.sleep(0.8)

    def touch(self, kind, x=0, y=0):
        self.cdp.send('Input.dispatchTouchEvent', {'type': kind, 'touchPoints': [] if kind == 'touchEnd' else [{'x': x, 'y': y}]})

    def rows(self):
        return self.page.evaluate(f"[...document.querySelectorAll('{VIS} .xterm-rows > div')].map(d=>d.textContent).join('\\n')")

    def run(self, command, expect, timeout=10):
        self.page.keyboard.type(command)
        self.page.keyboard.press('Enter')
        deadline = time.time() + timeout
        while time.time() < deadline:
            if re.search(expect, self.rows()):
                return True
            time.sleep(0.1)
        return False

    def size(self):
        self.run('echo SZ=$(stty size | tr " " x)=', r'SZ=\d+x\d+=')
        return re.findall(r'SZ=(\d+x\d+)=', self.rows())[-1]

    def js(self, expression):
        return self.page.evaluate(expression)

    def center(self):
        box = self.page.locator(f'{VIS} .terminal-host').bounding_box()
        return box['x'] + box['width'] / 2, box['y'] + box['height'] / 2


def keyboard_and_scrolling(playwright, browser, url):
    phone = Phone(playwright, browser, url, gpu=False)
    page = phone.page
    full = phone.size()
    sent = len(phone.resizes)
    phone.run('seq 1 300', r'(^|\n)300')
    phone.keyboard(True)
    page.wait_for_selector(f'{VIS} .terminal-keys')
    check('keyboard opening sends no resize', len(phone.resizes) == sent, len(phone.resizes) - sent)
    check('terminal size unchanged while the keyboard is up', phone.size() == full)
    visible = phone.js(f"""(() => {{ const h = document.querySelector('{VIS} .terminal-host').getBoundingClientRect(),
      c = document.querySelector('{VIS} .xterm-cursor, {VIS} .xterm-helper-textarea').getBoundingClientRect();
      return c.top >= h.top - 1 && c.bottom <= h.bottom + 1 }})()""")
    check('cursor row stays visible above the keyboard', visible)
    keys = phone.js(f"""(() => {{ const nav = document.querySelector('{VIS} .terminal-keys');
      return {{ count: nav.children.length, overflow: nav.scrollWidth > nav.clientWidth + 1,
        inside: [...nav.children].every(b => b.getBoundingClientRect().right <= innerWidth + 1 && b.getBoundingClientRect().height >= 43.5) }} }})()""")
    check('every key in the first row fits the screen', keys['count'] >= 10 and keys['inside'] and not keys['overflow'], keys)
    phone.keyboard(False)
    check('keyboard closing sends no resize', len(phone.resizes) == sent and phone.js(f"document.querySelector('{VIS} .xterm').style.transform") == '')
    # Landscape is wide enough for the desktop layout, which attaches a fresh
    # pane: the new size arrives with that attach, so ask the shell.
    page.set_viewport_size({'width': phone.height, 'height': phone.width})
    page.wait_for_selector('.terminal-host .xterm')
    time.sleep(1.0)
    page.click('.terminal-host')
    page.keyboard.type('echo SZ=$(stty size | tr " " x)=')
    page.keyboard.press('Enter')
    time.sleep(0.8)
    landscape = re.findall(r'SZ=(\d+x\d+)=', page.evaluate("[...document.querySelectorAll('.xterm-rows > div')].map(d=>d.textContent).join('\\n')"))
    check('rotation still resizes the terminal', bool(landscape) and landscape[-1] != full, f'{full} -> {landscape[-1:]}')
    page.set_viewport_size({'width': phone.width, 'height': phone.height})
    page.wait_for_selector(f'{VIS} .terminal-host .xterm')
    time.sleep(1.0)
    page.click(f'{VIS} .terminal-host')

    phone.run('seq 1 3000', r'(^|\n)3000')
    x, y = phone.center()
    # What is on screen each frame: where a given line number would be drawn.
    page.evaluate(f"""() => {{ window.__s = []; const host = document.querySelector('{VIS} .terminal-host');
      const sample = () => {{ const row = [...host.querySelectorAll('.xterm-rows > div')].find(d => /^\\d+$/.test(d.textContent.trim()));
        if (row) {{ const box = row.getBoundingClientRect(); window.__s.push(box.top - Number(row.textContent.trim()) * box.height); }}
        requestAnimationFrame(sample); }}; requestAnimationFrame(sample); }}""")
    phone.touch('touchStart', x, y - 150)
    offsets = set()
    for step in range(1, 70):
        phone.touch('touchMove', x, y - 150 + step * 4)
        time.sleep(0.016)
        offsets.add(phone.js(f"document.querySelector('{VIS} .xterm-screen').style.transform"))
    time.sleep(0.3)
    phone.touch('touchEnd')
    time.sleep(0.5)
    samples = phone.js('window.__s')
    steps = [b - a for a, b in zip(samples, samples[1:])]
    check('dragging history never jumps backwards between frames', len(steps) > 40 and min(steps) > -0.6, f'{len(steps)} frames, worst {min(steps or [0]):.1f}')
    check('dragging moves by pixels between rows', len([o for o in offsets if o]) >= 4)
    check('a slow release settles exactly on a row', phone.js(f"document.querySelector('{VIS} .xterm-screen').style.transform") == '')
    first = lambda: int(phone.rows().split('\n')[0] or 0)
    before = first()
    phone.touch('touchStart', x, y - 150)
    for step in range(1, 7):
        phone.touch('touchMove', x, y - 150 + step * 50)
        time.sleep(0.012)
    phone.touch('touchEnd')
    time.sleep(0.25)
    middle = first()
    time.sleep(3.0)
    after = first()
    check('a flick keeps gliding after the finger lifts', before - middle >= 10 and middle - after >= 10, f'{before} {middle} {after}')
    check('a fling settles exactly on a row', phone.js(f"document.querySelector('{VIS} .xterm-screen').style.transform") == '')
    check('no page errors (keyboard and scrolling)', not phone.errors, phone.errors[:2])
    phone.close()


def selection_attach_and_settings(playwright, browser, url, image):
    phone = Phone(playwright, browser, url, gpu=True)
    page = phone.page
    page.wait_for_function(f"document.querySelectorAll('{VIS} .xterm canvas').length > 0", timeout=15000)
    check('terminal text is drawn on a canvas by default', phone.js(f"document.querySelectorAll('{VIS} .xterm-rows > div').length") == 0)
    page.keyboard.type('echo alpha bravo charlie delta')
    page.keyboard.press('Enter')
    time.sleep(0.8)
    box = page.locator(f'{VIS} .xterm-screen').bounding_box()
    # Row height comes from Settings' own readout, as the canvas has no rows to measure.
    page.click('[aria-label="Toggle navigation"]')
    page.locator('.navigation-settings .settings-button').last.click()
    page.wait_for_selector('.preferences[open] output')
    rows = int(re.search(r'(\d+) rows', page.locator('.preferences output').inner_text()).group(1))
    page.click('[aria-label="Close settings"]')
    time.sleep(0.3)
    if page.locator('.navigation-drawer[open]').count():
        page.keyboard.press('Escape')
        time.sleep(0.4)
    # The echoed output is on the second row; press on its text.
    x, y = box['x'] + box['width'] * 0.25, box['y'] + 1.5 * box['height'] / rows
    phone.touch('touchStart', x, y)
    time.sleep(0.75)
    phone.touch('touchEnd')
    time.sleep(0.4)
    bar = page.locator(f'{VIS} .selection-actions')
    handles = page.locator(f'{VIS} .selection-handle')
    check('a long press selects text and offers Copy', bar.count() == 1 and bar.locator('button').first.inner_text() == 'Copy', bar.count())
    check('a selection has a handle at each end', handles.count() == 2, handles.count())
    if handles.count() == 2:
        end = handles.nth(1).bounding_box()
        hx, hy = end['x'] + end['width'] / 2, end['y'] + 10
        phone.touch('touchStart', hx, hy)
        for step in range(1, 9):
            phone.touch('touchMove', hx + step * 8, hy)
            time.sleep(0.02)
        phone.touch('touchEnd')
        time.sleep(0.4)
        moved = handles.nth(1).bounding_box()
        check('dragging the end handle extends the selection', moved['x'] > end['x'] + 20, f"{end['x']:.0f} -> {moved['x']:.0f}")
        check('the Copy bar returns after dragging a handle', bar.count() == 1)
    page.locator(f'{VIS} .selection-actions [aria-label="Clear selection"]').click()
    time.sleep(0.3)
    check('clearing removes the bar and handles', bar.count() == 0 and handles.count() == 0)

    # Attach: a phone's image picker backgrounds the page and closes the keyboard.
    page.click(f'{VIS} .terminal-host')
    phone.keyboard(True)
    page.wait_for_selector(f'{VIS} .attach-image')
    check('the image picker is not shown as a control', phone.js("[...document.querySelectorAll('.image-picker')].every(e => !e.offsetParent)"))
    for attempt in (1, 2):
        with page.expect_file_chooser() as chooser:
            page.click(f'{VIS} .attach-image')
        page.evaluate("""() => { Object.defineProperty(document, 'hidden', {configurable: true, get: () => true});
          Object.defineProperty(document, 'visibilityState', {configurable: true, get: () => 'hidden'}); document.dispatchEvent(new Event('visibilitychange')); }""")
        phone.keyboard(False)
        page.evaluate("""() => { Object.defineProperty(document, 'hidden', {configurable: true, get: () => false});
          Object.defineProperty(document, 'visibilityState', {configurable: true, get: () => 'visible'}); document.dispatchEvent(new Event('visibilitychange')); }""")
        chooser.value.set_files(str(image))
        page.wait_for_function(f"[...document.querySelectorAll('{VIS} .attachment-status')].some(e => e.textContent.startsWith('Image reference inserted'))", timeout=10000)
        check(f'attach {attempt} uploads and inserts after the keyboard closed', len(phone.uploads) == attempt and phone.uploads[-1] == 200, phone.uploads)
        page.click(f'{VIS} .terminal-host')
        phone.keyboard(True)
        page.wait_for_selector(f'{VIS} .attach-image')
    phone.keyboard(False)

    # Drawer: visible row menus, then Settings.
    page.click('[aria-label="Toggle navigation"]')
    page.wait_for_selector('.navigation-drawer[open]')
    time.sleep(0.5)
    more = page.locator('.navigation-drawer .tree-more')
    check('workspace, tab and selected pane rows each show a menu button', more.count() == 3, more.count())
    page.locator('.navigation-drawer .pane-more').click()
    time.sleep(0.3)
    labels = phone.js("[...document.querySelectorAll('dialog[open] button')].map(b => b.textContent.trim())")
    check('the pane menu offers split and close', 'Split pane' in labels and 'Close pane' in labels, labels)
    page.keyboard.press('Escape')
    time.sleep(0.3)
    page.locator('.navigation-settings .settings-button').last.click()
    page.wait_for_selector('.preferences[open]')
    time.sleep(0.6)
    sheet = phone.js("""(() => { const d = document.querySelector('.preferences[open]'), b = d.querySelector('.preferences-body'), r = d.getBoundingClientRect();
      const small = [...d.querySelectorAll('button, input[type=range], .switch-row, .segment')].filter(e => e.offsetParent && e.getBoundingClientRect().height < 43.5).length;
      return { overflow: b.scrollWidth > b.clientWidth + 1, full: r.width >= innerWidth - 1 && r.height >= innerHeight - 1, small, readout: d.querySelector('output').textContent,
        version: d.querySelector('.version-option small').textContent, switches: d.querySelectorAll('.switch-row input').length } })()""")
    check('Settings is a full sheet without sideways overflow', sheet['full'] and not sheet['overflow'], sheet)
    check('every Settings control is at least 44 px tall', sheet['small'] == 0, sheet['small'])
    check('text size reports columns and rows', re.search(r'\d+ px · \d+ columns × \d+ rows', sheet['readout']) is not None, sheet['readout'])
    check('Settings names the loaded build', sheet['version'].startswith('Development server · '), sheet['version'])
    # Copy access link: fetched on the tap, confirmed, and never rendered.
    copy = page.locator('[data-testid=copy-access-link]')
    copy.scroll_into_view_if_needed()
    for _ in range(2):  # a browser may refuse the clipboard on the first tap
        copy.click()
        page.wait_for_selector('[data-testid=access-settings] .success, [data-testid=access-settings] [role=alert]', timeout=5000)
        if page.locator('[data-testid=access-settings] .success').count():
            break
    access = phone.js("document.querySelector('[data-testid=access-settings]').innerText")
    check('Copy access link confirms the copy', 'Access link copied' in access, access[-160:] if 'token=' not in access else 'link was rendered')
    check('the access link is never rendered', 'token=' not in phone.js('document.documentElement.outerHTML'))
    page.click('[data-testid=show-access-qr]')
    page.wait_for_selector('.access-qr svg path', timeout=5000)
    code = phone.js("""(() => { const s = document.querySelector('.access-qr svg'), r = s.getBoundingClientRect();
      return { square: Math.abs(r.width - r.height) < 1, w: r.width, modules: s.querySelector('path').getAttribute('d').split('M').length - 1,
        fits: r.right <= innerWidth + 1, text: document.documentElement.outerHTML.includes('token=') } })()""")
    check('Show QR code draws a square code that fits the sheet', code['square'] and code['fits'] and code['w'] >= 200 and code['modules'] > 200, code)
    page.click('[data-testid=show-access-qr]')
    check('Hide QR code removes it', page.locator('.access-qr').count() == 0)
    columns = lambda: int(re.search(r'(\d+) columns', page.locator('.preferences output').inner_text()).group(1))
    wide = columns()
    page.click('[aria-label="Increase font size"]')
    page.click('[aria-label="Increase font size"]')
    time.sleep(0.8)
    check('larger text reports fewer columns', columns() < wide, f'{wide} -> {columns()}')
    page.locator('.preferences .size-readout .quiet').click()
    switches = page.locator('.preferences .switch-row input')
    check('phones get three terminal switches, all on', sheet['switches'] == 3 and all(switches.nth(i).is_checked() for i in range(3)))
    switches.nth(2).click()
    page.wait_for_function("document.querySelectorAll('.xterm canvas').length === 0", timeout=5000)
    check('turning smooth drawing off removes the canvas at once', phone.js("document.querySelectorAll('.xterm-rows > div').length") > 0)
    switches.nth(2).click()
    page.wait_for_function("document.querySelectorAll('.xterm canvas').length > 0", timeout=10000)
    check('turning it back on restores the canvas', True)
    switches.nth(0).click()
    switches.nth(1).click()
    page.click('[aria-label="Close settings"]')
    time.sleep(0.3)
    if page.locator('.navigation-drawer[open]').count():
        page.keyboard.press('Escape')
        time.sleep(0.4)
    page.click(f'{VIS} .terminal-host')
    sent = len(phone.resizes)
    phone.keyboard(True)
    check('with both keyboard switches off, keys stay hidden and the keyboard resizes', page.locator(f'{VIS} .terminal-keys').count() == 0 and len(phone.resizes) > sent)
    phone.keyboard(False)
    check('no page errors (selection, attach and settings)', not phone.errors, phone.errors[:2])
    phone.close()


def desktop(browser, url):
    context = browser.new_context(viewport={'width': 1280, 'height': 800})
    page = context.new_page()
    errors = []
    page.on('pageerror', lambda error: errors.append(str(error)[:200]))
    page.goto(url)
    page.wait_for_selector('.terminal-host .xterm', timeout=30000)
    time.sleep(1.0)
    settings = page.locator('.navigation-settings .settings-button')
    if not settings.first.is_visible():
        page.click('[aria-label="Toggle navigation"]')
    settings.first.click()
    page.wait_for_selector('.preferences[open]')
    box = page.evaluate("(() => { const r = document.querySelector('.preferences[open]').getBoundingClientRect(); return { w: r.width, inside: r.left > 0 && r.right < innerWidth && r.top >= 0 && r.bottom <= innerHeight } })()")
    check('desktop Settings is a centred dialog', box['inside'] and box['w'] <= 560, box)
    check('desktop shows only the drawing switch', page.locator('.preferences .switch-row input').count() == 1)
    check('no page errors (desktop)', not errors, errors[:2])
    context.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=PROJECT / 'target/release/herdr-webapp')
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix='herdr-ui-e2e-') as temporary:
        root = Path(temporary)
        os.chmod(root, 0o700)
        state = root / 'state.json'
        image = root / 'dot.png'
        png(image)
        port, backend = free_port(), free_port()
        log = open(root / 'dev.log', 'wb')
        dev = subprocess.Popen([sys.executable, '-B', str(PROJECT / 'scripts/dev.py'), '--no-build', '--binary', str(args.binary.resolve()),
                                '--port', str(port), '--backend-port', str(backend), '--origin', f'http://127.0.0.1:{port}',
                                '--state-file', str(state)], stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
        try:
            deadline = time.time() + 90
            while not state.exists():
                if dev.poll() is not None or time.time() > deadline:
                    raise RuntimeError('The disposable dev session did not start; run `make dev` to see why.')
                time.sleep(0.2)
            url = json.loads(state.read_text())['privateSignInUrl']
            with sync_playwright() as playwright:
                browser = playwright.chromium.launch(channel='chrome', headless=True)
                try:
                    keyboard_and_scrolling(playwright, browser, url)
                    selection_attach_and_settings(playwright, browser, url, image)
                    desktop(browser, url)
                finally:
                    browser.close()
        finally:
            if dev.poll() is None:
                os.killpg(dev.pid, signal.SIGINT)
                try:
                    dev.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    os.killpg(dev.pid, signal.SIGKILL)
            log.close()
    if failures:
        print(f'UI E2E FAILED: {len(failures)} check(s): ' + '; '.join(failures))
        return 1
    print('ALL UI E2E CHECKS PASSED (isolated Core, headless Chrome, synthetic touch)')
    return 0


if __name__ == '__main__':
    sys.exit(main())
