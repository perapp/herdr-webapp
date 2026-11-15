#!/usr/bin/env python3
"""Runs only inside the disposable GUI. CDP and page contents are never logged."""
import base64
import json
from pathlib import Path
import sys
import time
import urllib.request

import websocket
from PIL import Image

ROOT = Path('/workspace')

# Passive wire observer: no mock transport, synthesized app input, or policy overrides.
# Decode only to count terminal bytes and capture real upload_result paths privately.
OBSERVER = r'''
window.__e2e={peers:[],uploads:[],receivedBytes:0,tail:'',maxFrame:0,authAttempts:0,sentFrames:0,sentBytes:0};
window.__e2e.clicks={};document.addEventListener('click',event=>{
  const selectors={rotate:'[data-testid=access-settings] > button',confirm:'[aria-label="Confirm access token rotation"] .danger',close:'[aria-label="Close settings"]',versions:'.version-option',install:'.install-option',terminal:'.terminal-host',navigation:'.navigation-toggle',settings:'.settings-button'};
  for(const [key,selector] of Object.entries(selectors))if(event.target.closest?.(selector))window.__e2e.clicks[key]=(window.__e2e.clicks[key]||0)+1;
},true);
// Count real auth attempts only; never retain identities or capabilities.
const NativeSocket=window.WebSocket;
window.WebSocket=class extends NativeSocket {
  send(data){try{const m=JSON.parse(data);if(m.type==='signal'&&JSON.parse(m.payload).kind==='auth')window.__e2e.authAttempts++;}catch{}return super.send(data);}
};
const NativePeer=window.RTCPeerConnection;
window.RTCPeerConnection=class extends NativePeer {
  constructor(...args){super(...args);window.__e2e.peers.push(this);}
  createDataChannel(...args){
    const channel=super.createDataChannel(...args), partial=new Map();
    const send=channel.send.bind(channel);
    channel.send=data=>{if(channel.label.startsWith('herdr-terminal')){window.__e2e.sentFrames++;window.__e2e.sentBytes+=data.byteLength||0;}window.__e2e.maxFrame=Math.max(window.__e2e.maxFrame,data.byteLength||0);return send(data);};
    channel.addEventListener('message',event=>{
      if(!(event.data instanceof ArrayBuffer))return;
      const b=new Uint8Array(event.data); if(b.length<17||b[0]!==72||b[1]!==68||b[2]!==82||b[3]!==1)return;
      const v=new DataView(b.buffer),id=v.getUint32(4,true),total=v.getUint32(8,true),offset=v.getUint32(12,true);
      if(total>16*1024*1024)return;
      let value=partial.get(id);if(!value){value=new Uint8Array(total);partial.set(id,value);}
      value.set(b.subarray(17),offset);if(offset+b.length-17!==total)return;partial.delete(id);
      const text=new TextDecoder().decode(value);
      if(channel.label.startsWith('herdr-terminal')){
        window.__e2e.receivedBytes+=value.length;window.__e2e.tail=(window.__e2e.tail+text).slice(-65536);
      }
      if(channel.label.startsWith('herdr-upload-v1:')){
        try{const msg=JSON.parse(text);if(msg.type==='upload_result'&&msg.status===200){
          const decoded=JSON.parse(atob(msg.body));if(typeof decoded.path==='string')window.__e2e.uploads.push(decoded.path);
        }}catch{}
      }
    });return channel;
  }
};
'''


class NavigationPending(RuntimeError):
    """A document is between execution contexts; safe to retry a read-only wait."""


class Browser:
    def __init__(self, target_id=None):
        self.seq = 0
        deadline = time.monotonic() + 30
        while True:
            try:
                with urllib.request.urlopen('http://127.0.0.1:9222/json/list', timeout=2) as response:
                    tabs = json.load(response)
                tab = next(t for t in tabs if t['type'] == 'page' and
                           (target_id is None or t['id'] == target_id))
                break
            except Exception:
                if time.monotonic() > deadline:
                    raise RuntimeError('CDP unavailable') from None
                time.sleep(.2)
        self.ws = websocket.create_connection(tab['webSocketDebuggerUrl'],
                   origin='http://gui-workstation-control.invalid', timeout=15)
        try:
            self.call('Page.enable')
            self.call('Runtime.enable')
        except BaseException:
            self.ws.close()
            raise

    def call(self, method, params=None):
        self.seq += 1
        ident = self.seq
        self.ws.send(json.dumps({'id': ident, 'method': method, 'params': params or {}}))
        while True:
            response = json.loads(self.ws.recv())
            if response.get('id') == ident:
                if 'error' in response:
                    message = response['error'].get('message', '')
                    if any(text in message for text in ('Cannot find context', 'Execution context was destroyed', 'Inspected target navigated')):
                        raise NavigationPending('Browser navigation pending')
                    raise RuntimeError('CDP operation failed')
                return response.get('result', {})

    def js(self, expression):
        result = self.call('Runtime.evaluate', {'expression': expression,
                           'returnByValue': True, 'awaitPromise': True})
        if 'exceptionDetails' in result:
            raise RuntimeError('page evaluation failed')
        return result.get('result', {}).get('value')

    def wait(self, expression, timeout=60, label='acceptance condition'):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                value = self.js(expression)
                if value:
                    return value
            except NavigationPending:
                pass  # Retry only context turnover, never arbitrary page errors.
            time.sleep(.1)
        if label in ('rotation settings responsive', 'rotation confirmation visible', 'click target visible and enabled'):
            metrics = self.js('''(()=>{const d=document.querySelector('.preferences[open]'),b=d?.querySelector('.preferences-body'),r=d?.getBoundingClientRect();return {
              viewportWidth:innerWidth,viewportHeight:innerHeight,dialogPresent:!!d,bodyPresent:!!b,
              rotationPeerPresent:!!window.__e2e.rotationPeer,
              rotateClicks:window.__e2e.clicks.rotate||0,confirmClicks:window.__e2e.clicks.confirm||0,
              closeClicks:window.__e2e.clicks.close||0,versionClicks:window.__e2e.clicks.versions||0,installClicks:window.__e2e.clicks.install||0,
              confirmDisabled:document.querySelector('[aria-label="Confirm access token rotation"] .danger')?.disabled===true,
              confirmationButtons:document.querySelectorAll('.confirmation button').length,
              visualScale:visualViewport.scale,
              confirmHit:(()=>{const e=document.querySelector('[aria-label="Confirm access token rotation"] .danger');if(!e)return false;const r=e.getBoundingClientRect(),h=document.elementFromPoint(r.x+r.width/2,r.y+r.height/2);return h===e||e.contains(h)})(),
              dialogTotal:document.querySelectorAll('.preferences').length,terminals:document.querySelectorAll('.xterm').length,
              connected:window.__e2e.peers.filter(p=>p.connectionState==='connected').length,
              navigationOpen:!!document.querySelector('#navigation-drawer[open]'),
              connectionNotice:!!document.querySelector('[data-testid=connection-notice]'),
              dialogWidth:Math.ceil(r?.width||0),dialogHeight:Math.ceil(r?.height||0),
              bodyScrollWidth:b?.scrollWidth||0,bodyClientWidth:b?.clientWidth||0,
              confirmationCount:document.querySelectorAll('[aria-label="Confirm access token rotation"]').length};})()''')
            (ROOT / 'shell-metrics.json').write_text(json.dumps(metrics))
        raise AssertionError(label)

    def live(self):
        try:
            self.wait("!!document.querySelector('.xterm-helper-textarea') && "
                      "(!!document.querySelector('.terminal-status.connected') || "
                      "(!!document.querySelector('.terminal-pane.compact') && !document.querySelector('.terminal-detail')))", label='terminal attached')
        except AssertionError:
            # Counts/booleans only: never export terminal text or private links.
            metrics = self.js('''(()=>({peers:window.__e2e?.peers.length||0,
              connected:window.__e2e?.peers.filter(p=>p.connectionState==='connected').length||0,
              terminals:document.querySelectorAll('.xterm').length,
              textareas:document.querySelectorAll('.xterm-helper-textarea').length,
              connectionNotice:!!document.querySelector('[data-testid=connection-workspace]'),
              navigationOpen:!!document.querySelector('.app-shell'),
              receivedBytes:window.__e2e?.receivedBytes||0}))()''')
            (ROOT / 'shell-metrics.json').write_text(json.dumps(metrics))
            raise
        self.wait("!!window.__e2e && window.__e2e.peers.some(p=>p.connectionState==='connected')", label='peer connected')

    def reload(self, ready="document.readyState==='complete'", timeout=60):
        """Fence the old document; a reload command can return before navigation."""
        self.js('window.__herdrReloadFence=true')
        self.call('Page.reload', {'ignoreCache': True})
        self.wait("!window.__herdrReloadFence && (" + ready + ")", timeout=timeout,
                  label='new document ready after reload')

    def disconnected(self, timeout=60):
        """Relay tears App down; local transport can retain a disconnected pane."""
        self.wait("!document.querySelector('.xterm') || !!document.querySelector('.terminal-detail')",
                  timeout=timeout, label='terminal disconnected')

    def click(self, selector):
        # Separate incognito profiles are real background tabs until activated.
        # Send trusted input to the intended owned page, not another profile.
        self.call('Page.bringToFront')
        self.wait("document.hasFocus() && document.visibilityState==='visible'",
                  label='input page focused')
        # Measure one connected, uncovered node in the same evaluation. A layout
        # change (desktop/phone) may replace the pane after the first DOM wait.
        # Retry only this read/scroll preparation, never dispatched clicks.
        rect = self.wait('(()=>{const e=document.querySelector(' + json.dumps(selector) +
                  ');if(!e?.isConnected||e.disabled)return false;'
                  'e.scrollIntoView({block:"center",inline:"nearest",behavior:"instant"});'
                  'const r=e.getBoundingClientRect(),x=r.x+r.width/2,y=r.y+r.height/2;'
                  'if(r.width<=0||r.height<=0)return false;'
                  'const hit=document.elementFromPoint(x,y);if(hit!==e&&!e.contains(hit))return false;'
                  'const v=visualViewport;return {x:(x-(v?.offsetLeft||0))*(v?.scale||1),y:(y-(v?.offsetTop||0))*(v?.scale||1)}})()',
                  label='click target visible and enabled')
        for kind in ('mousePressed', 'mouseReleased'):
            self.call('Input.dispatchMouseEvent', {'type': kind, **rect, 'button': 'left', 'clickCount': 1})

    def key(self, name, code, vk, modifiers=0):
        for kind in ('keyDown', 'keyUp'):
            self.call('Input.dispatchKeyEvent', {'type': kind, 'key': name, 'code': code,
                      'windowsVirtualKeyCode': vk, 'nativeVirtualKeyCode': vk, 'modifiers': modifiers,
                      **({'text': '\r'} if name == 'Enter' and kind == 'keyDown' else {})})

    def draft(self, text):
        self.live()
        self.click('.terminal-host')
        self.js("window.__e2e.clickFocused=document.activeElement?.classList.contains('xterm-helper-textarea')===true;window.__e2e.linkHovered=!!document.querySelector('.xterm-cursor-pointer')")
        self.call('Input.insertText', {'text': text})

    def shell(self, text, marker, label='shell sentinel'):
        self.js('window.__e2e.tail=""')
        self.draft(text)
        self.key('Enter', 'Enter', 13)
        try:
            self.wait('window.__e2e.tail.includes(' + json.dumps(marker) + ')', label=label)
        except AssertionError:
            # Fixed booleans/counts only, captured before profile teardown.
            metrics = self.js('''(()=>{const e=window.__e2e;return {
              clickFocused:e.clickFocused,linkHovered:e.linkHovered,
              activeBody:document.activeElement===document.body,
              activeInput:document.activeElement?.tagName==='INPUT',
              activeButton:document.activeElement?.tagName==='BUTTON',
              focused:document.hasFocus(),visible:document.visibilityState==='visible',
              textareaFocused:document.activeElement?.classList.contains('xterm-helper-textarea')===true,
              textareas:document.querySelectorAll('.xterm-helper-textarea').length,
              peers:e.peers.length,connected:e.peers.filter(p=>p.connectionState==='connected').length,
              receivedBytes:e.receivedBytes,tailBytes:e.tail.length,sentFrames:e.sentFrames,sentBytes:e.sentBytes,
              markerSeen:e.tail.includes(''' + json.dumps(marker) + ''')};})()''')
            (ROOT / 'shell-metrics.json').write_text(json.dumps(metrics))
            raise

    def stats(self, transport):
        value = self.js('''(async()=>{
          const p=window.__e2e.peers.filter(p=>p.connectionState==='connected').at(-1);
          if(!p)return null;
          const stats=await p.getStats();let selected=new Set();
          stats.forEach(r=>{if(r.type==='transport'&&r.selectedCandidatePairId)selected.add(r.selectedCandidatePairId)});
          let pairs=[];stats.forEach(r=>{if(r.type==='candidate-pair'&&r.state==='succeeded'&&
            (selected.has(r.id)||(!selected.size&&r.nominated))){
            const l=stats.get(r.localCandidateId),q=stats.get(r.remoteCandidateId);
            pairs.push({local:l?.candidateType,remote:q?.candidateType,protocol:l?.protocol,relayProtocol:l?.relayProtocol});
          }});
          const turnTransports=[...new Set(p.getConfiguration().iceServers.flatMap(s=>Array.isArray(s.urls)?s.urls:[s.urls])
            .filter(u=>/^turns?:/i.test(u)).map(u=>/^turns:/i.test(u)?'tls':/[?&]transport=tcp(?:&|$)/i.test(u)?'tcp':'udp'))];
          return {pairs,turnTransports,receivedBytes:window.__e2e.receivedBytes,maxFrame:window.__e2e.maxFrame};
        })()''')
        assert value and value['pairs'], 'selected ICE pair missing'
        assert value['maxFrame'] <= 16384, 'oversized SCTP frame'
        if transport != 'direct':
            assert value['turnTransports'] == [transport], 'mixed TURN transport configuration'
        for pair in value['pairs']:
            if transport == 'direct':
                assert pair['local'] != 'relay' and pair['remote'] != 'relay', 'unexpected relay route'
            else:
                assert pair['local'] == pair['remote'] == 'relay', 'both peers must select relay'
                assert pair.get('relayProtocol') == transport, 'wrong TURN transport selected'
        # Whitelist, not arbitrary browser-supplied values, for exported report.
        allowed = {'host', 'srflx', 'prflx', 'relay', 'udp', 'tcp', 'tls'}
        value['pairs'] = [{key: val for key, val in pair.items() if val in allowed}
                          for pair in value['pairs']]
        return value


class BrowserProfile:
    """Own one incognito context on the disposable workstation, never other tabs."""
    def __init__(self):
        self.control = None
        self.context = None
        self.browser = None

    def __enter__(self):
        try:
            # Browser.call normally speaks to a PAGE socket. Context ownership
            # commands require the browser endpoint, not that inherited page.
            with urllib.request.urlopen('http://127.0.0.1:9222/json/version', timeout=5) as response:
                version = json.load(response)
            control = object.__new__(Browser)
            control.seq = 0
            control.ws = websocket.create_connection(version['webSocketDebuggerUrl'],
                origin='http://gui-workstation-control.invalid', timeout=15)
            self.control = control
            self.context = self.control.call('Target.createBrowserContext')['browserContextId']
            target = self.control.call('Target.createTarget', {
                'url': 'about:blank', 'browserContextId': self.context})['targetId']
            self.browser = Browser(target)
            self.browser.call('Page.addScriptToEvaluateOnNewDocument', {'source': OBSERVER})
            return self.browser
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def __exit__(self, *_):
        try:
            if self.browser:
                self.browser.ws.close()
        finally:
            if self.control:
                try:
                    if self.context:
                        self.control.call('Target.disposeBrowserContext', {'browserContextId': self.context})
                finally:
                    self.control.ws.close()


def pair_profile(browser, job, capability):
    fragment = base64.urlsafe_b64encode(json.dumps(capability, separators=(',', ':')).encode()).decode().rstrip('=')
    query = '?forceTurn=1' if job['transport'] != 'direct' else ''
    browser.call('Page.navigate', {'url': job['origin'] + '/' + query + '#pair=' + fragment})
    browser.wait("location.hash==='' && !!window.__e2e", label='observer installed')


def native_proof(browser, job, name):
    browser.shell("printf '%s\\n' " + name + ' > ' + json.dumps(str(Path(job['proofDir']) / name)) +
                  "; printf '%s%s\\n' ROTATION_ OK", 'ROTATION_OK', label=name)


def denied_profile(browser):
    # Connector rejection can silently drop an auth request. Wait through TWO
    # actual auth attempts and their failure/deadline, not a sleep/hidden pane.
    # Zero offered peers over the entire document excludes transient success.
    browser.wait("window.__e2e.authAttempts>=2 && "
                 "document.querySelector('[data-testid=connection-workspace] h1')?.textContent==='Connection interrupted'", timeout=120,
                 label='rotation authorization rejected')
    assert browser.js("window.__e2e.peers.length===0 && !document.querySelector('.xterm')"), 'rotation authorization rejected'


def rotation_settings(browser, mobile):
    # Use separate already-authenticated pages for the two viewport captures.
    # Never remount the keeper terminal merely to emulate a different device.
    browser.call('Emulation.setDeviceMetricsOverride', {'width': 390 if mobile else 1280,
                'height': 844 if mobile else 900, 'deviceScaleFactor': 1, 'mobile': mobile})
    browser.wait("document.querySelector('.app-shell')?.classList.contains('mobile')===" + str(mobile).lower())
    browser.click('[aria-label="Toggle navigation"]')
    browser.click('.navigation-settings .settings-button[aria-haspopup="dialog"]')
    rotate = '[data-testid=access-settings] > button'
    browser.wait("document.querySelector('[data-testid=access-settings] > button')?.textContent==='Rotate access token…'")
    browser.click(rotate)
    browser.wait("(()=>{const d=document.querySelector('.preferences[open]'),b=d?.querySelector('.preferences-body');return !!b && d.getBoundingClientRect().width<=innerWidth && b.scrollWidth<=b.clientWidth+1})()",
                label='rotation settings responsive')
    # Scroll the real confirmation into view; screenshot only the page,
    # without browser chrome, URL, fragment, QR or any pairing material.
    browser.wait("(()=>{const e=document.querySelector('[aria-label=\"Confirm access token rotation\"]');if(!e?.isConnected)return false;e.scrollIntoView({block:'center',behavior:'instant'});return true})()",
                label='rotation confirmation visible')
    browser.wait("(()=>{const d=document.querySelector('.preferences[open]');if(!d)return false;const r=d.getBoundingClientRect(),buttons=[...d.querySelectorAll('.confirmation button')];return location.hash==='' && r.left>=0 && r.right<=innerWidth+1 && r.top>=0 && r.bottom<=innerHeight+1 && buttons.length===2 && buttons.every(b=>{const q=b.getBoundingClientRect();return q.height>=44 && q.left>=r.left && q.right<=r.right}) && !d.querySelector('[role=alert]')})()",
                label='rotation settings responsive')
    image = browser.call('Page.captureScreenshot', {'format': 'png', 'captureBeyondViewport': False})
    screenshot = ROOT / ('rotation-settings-phone.png' if mobile else 'rotation-settings-desktop.png')
    screenshot.write_bytes(base64.b64decode(image['data']))
    screenshot.chmod(0o600)


def rotation_acceptance(primary, job):
    primary.live()
    first = primary.js('location.pathname')
    query = '?forceTurn=1' if job['transport'] != 'direct' else ''
    # BOTH profiles use exactly the original reusable capability, not fresh
    # pairings supplied after each authentication or tabs sharing IndexedDB keys.
    with BrowserProfile() as keeper, BrowserProfile() as other:
        pair_profile(keeper, job, job['pairing'])
        keeper.live()
        native_proof(keeper, job, 'rotation-keeper-before-proof')
        pair_profile(other, job, job['pairing'])
        other.live()
        native_proof(other, job, 'rotation-other-before-proof')
        keeper.js("window.__e2e.rotationPeer=window.__e2e.peers.find(p=>p.connectionState==='connected');window.__e2e.rotationCount=window.__e2e.peers.length")
        other.js("window.__e2e.rotationPeer=window.__e2e.peers.find(p=>p.connectionState==='connected')")
        rotation_settings(keeper, False)
        keeper.js("window.__e2e.rotationPreCount=window.__e2e.peers.length;window.__e2e.rotationPreSame=window.__e2e.rotationPeer.connectionState==='connected'")
        # Exactly ONE UI confirmation. Never retry a mutation after disconnect.
        keeper.click('[aria-label="Confirm access token rotation"] .danger')
        keeper.wait("document.querySelector('[data-testid=access-settings] .success')?.textContent.startsWith('Access token rotated.')",
                    label='rotation completed')
        keeper.click('[aria-label="Close settings"]')
        keeper.click('[aria-label="Close navigation"]')
        native_proof(keeper, job, 'rotation-keeper-after-proof')
        metrics = keeper.js("({originalCount:window.__e2e.rotationCount,preCount:window.__e2e.rotationPreCount,preSame:window.__e2e.rotationPreSame,afterCount:window.__e2e.peers.length,afterSame:window.__e2e.rotationPeer.connectionState==='connected',connected:window.__e2e.peers.filter(p=>p.connectionState==='connected').length})")
        (ROOT / 'shell-metrics.json').write_text(json.dumps(metrics))
        assert metrics['originalCount'] == metrics['afterCount'] and metrics['afterSame'] and metrics['connected'] == 1, 'rotation keeper unchanged'
        other.wait("window.__e2e.rotationPeer.connectionState==='closed' && !document.querySelector('.xterm')",
                   label='rotation other revoked')
        # Resume using its persisted key after a genuine document reload.
        other.reload("!!window.__e2e")
        with BrowserProfile() as fresh:
            pair_profile(fresh, job, job['pairing'])
            denied_profile(other)
            denied_profile(fresh)
        # Existing PRIMARY profile's second-computer key must still resume.
        # It was paired by the previous phase and is not re-paired here.
        # Primary's first connection was revoked too; use the remembered list UI.
        primary.call('Page.addScriptToEvaluateOnNewDocument', {'source': OBSERVER})
        primary.call('Page.navigate', {'url': job['origin'] + '/' + query})
        primary.wait("document.querySelectorAll('[data-testid=open-computer]').length===2")
        links = primary.js("[...document.querySelectorAll('[data-testid=open-computer]')].map(e=>e.pathname)")
        second_path = next(path for path in links if path != first)
        primary.call('Page.navigate', {'url': job['origin'] + second_path + query})
        primary.wait('location.pathname===' + json.dumps(second_path) + ' && !!window.__e2e')
        primary.live()
        native_proof(primary, job, 'rotation-second-existing-proof')
        with BrowserProfile() as fresh_second:
            # Establish phone metrics before mounting App: changing devices after
            # attachment remounts panes and can close a just-opened navigation drawer.
            fresh_second.call('Emulation.setDeviceMetricsOverride', {'width': 390, 'height': 844, 'deviceScaleFactor': 1, 'mobile': True})
            pair_profile(fresh_second, job, job['secondPairing'])
            fresh_second.live()
            native_proof(fresh_second, job, 'rotation-second-pairing-proof')
            rotation_settings(fresh_second, True)
            fresh_second.click('[aria-label="Confirm access token rotation"] button:not(.danger)')
            fresh_second.click('[aria-label="Close settings"]')
            fresh_second.click('[aria-label="Close navigation"]')
        # Check the keeper again after all negative attempts and other-computer
        # traffic, so a delayed teardown cannot masquerade as successful rotation.
        native_proof(keeper, job, 'rotation-keeper-final-proof')
        assert keeper.js("window.__e2e.peers.length===window.__e2e.rotationCount && window.__e2e.rotationPeer.connectionState==='connected'"), 'rotation keeper unchanged'


def main():
    job = json.loads((ROOT / 'job.json').read_text())
    mode = sys.argv[1]
    browser = Browser()
    try:
        if mode == 'pair':
            browser.call('Page.addScriptToEvaluateOnNewDocument', {'source': OBSERVER})
            fragment = base64.urlsafe_b64encode(json.dumps(job['pairing'], separators=(',', ':')).encode()).decode().rstrip('=')
            query = '?forceTurn=1' if job['transport'] != 'direct' else ''
            browser.call('Page.navigate', {'url': job['origin'] + '/' + query + '#pair=' + fragment})
            browser.wait("location.hash === '' && !!window.__e2e", label='observer installed')
            browser.live()
            proof = str(Path(job['proofDir']) / 'shell-proof')
            browser.shell("printf '%s%s\\n' relay-shell- proof > " + json.dumps(proof) +
                          "; printf '%s%s\\n' SHELL_ OK", 'SHELL_OK')
        elif mode == 'draft-completion':
            for mobile in (False, True):
                if mobile:
                    browser.call('Emulation.setDeviceMetricsOverride', {'width': 390, 'height': 844, 'deviceScaleFactor': 1, 'mobile': True})
                else:
                    browser.call('Emulation.clearDeviceMetricsOverride')
                browser.live()
                proof = str(Path(job['proofDir']) / ('draft-phone-proof' if mobile else 'draft-desktop-proof'))
                code = '''import os,sys,termios,tty
from pathlib import Path
fd=sys.stdin.fileno();saved=termios.tcgetattr(fd);draft=bytearray();painted=False;armed=False;bad=False
try:
 tty.setraw(fd);os.write(1,b'\\r\\nDRAFT_READY\\r\\n')
 while True:
  data=os.read(fd,1)
  if data==b'\\x07' and not painted:
   painted=True
   os.write(1,b'\\x1b[?2026h\\r\\x1b[2Ktool completed\\r\\n\\x1b[6n\\x1b[c\\x1b]11;?\\x07\\x1b[?1004h\\x1b[?2026lCOMPLETION_PAINTED\\r\\n')
  elif data==b'\\x14':
   if bytes(draft)!=b'DRAFT_NOT_SENT' or bad:raise RuntimeError('draft was submitted or changed')
   armed=True;os.write(1,b'DRAFT_GUARD_OK\\r\\n')
  elif data in (b'\\r',b'\\n'):
   if painted and armed:
    Path(PROOF).write_text('draft-preserved\\n');os.write(1,b'EXPLICIT_ENTER_OK\\r\\n');break
   bad=True;Path(PROOF+'.unexpected').write_text('unexpected submission')
  elif not painted:draft.extend(data)
finally:
 os.write(1,b'\\x1b[?1004l');termios.tcsetattr(fd,termios.TCSANOW,saved)
'''.replace('PROOF', repr(proof))
                encoded = base64.b64encode(code.encode()).decode()
                browser.shell("python3 -c \"import base64;exec(base64.b64decode('" + encoded + "'))\"", 'DRAFT_READY', label='draft editor ready')
                browser.draft('DRAFT_NOT_SENT')
                browser.key('g', 'KeyG', 71, modifiers=2)
                browser.wait("window.__e2e.tail.includes('COMPLETION_PAINTED')", label='draft completion painted')
                browser.key('t', 'KeyT', 84, modifiers=2)
                browser.wait("window.__e2e.tail.includes('DRAFT_GUARD_OK')", label='draft guard retained')
                browser.key('Enter', 'Enter', 13)
                browser.wait("window.__e2e.tail.includes('EXPLICIT_ENTER_OK')", label='draft explicit enter')
            browser.call('Emulation.clearDeviceMetricsOverride')
            # A terminal link opens in the client browser, not a remote opener/dialog.
            browser.shell("printf '%s%s\\n' https://example.org/ herdr-client-link; printf '%s%s\\n' LINK_ READY", 'LINK_READY', label='client link printed')
            browser.wait("(()=>{const e=[...document.querySelectorAll('.xterm-rows span')].find(e=>e.textContent==='https://example.org/herdr-client-link');if(!e)return false;const r=e.getBoundingClientRect();window.__e2e.linkPoint={x:r.x+r.width/2,y:r.y+r.height/2};return r.width>0&&r.height>0})()")
            targets = browser.call('Target.getTargets')['targetInfos']
            original = browser.call('Target.getTargetInfo')['targetInfo']['targetId']
            previous = {t['targetId'] for t in targets}
            point = browser.js('window.__e2e.linkPoint')
            browser.call('Input.dispatchMouseEvent', {'type': 'mouseMoved', **point})
            browser.wait("!!document.querySelector('.xterm-cursor-pointer')")
            for kind in ('mousePressed', 'mouseReleased'):
                browser.call('Input.dispatchMouseEvent', {'type': kind, **point, 'button': 'left', 'clickCount': 1})
            deadline = time.monotonic() + 10
            while True:
                opened = [t for t in browser.call('Target.getTargets')['targetInfos'] if t['targetId'] not in previous and t['type'] == 'page']
                if opened: break
                assert time.monotonic() < deadline, 'acceptance condition'
                time.sleep(.05)
            assert any(t['url'].startswith('https://example.org/herdr-client-link') for t in opened), 'acceptance condition'
            for target in opened: browser.call('Target.closeTarget', {'targetId': target['targetId']})
            browser.call('Target.activateTarget', {'targetId': original})
            assert not browser.js("!!document.querySelector('dialog[open]')"), 'acceptance condition'
        elif mode == 'uploads':
            browser.live()
            Image.new('RGB', (16, 16), (140, 110, 190)).save(ROOT / 'upload.png')
            proof = str(Path(job['proofDir']) / 'unexpected-enter')
            if not browser.js("!!document.querySelector('.image-picker')"):
                browser.click('[aria-label="Terminal keyboard shortcuts"]')
            browser.wait("!!document.querySelector('.image-picker')")
            peer_count = browser.js('window.__e2e.peers.length')
            for i in range(21):
                # Receipt observation can precede Svelte's finally/busy render.
                # Respect the actual user-facing upload gate before another picker.
                browser.wait("!!document.querySelector('.attach-image:not(:disabled)')", label='upload ready')
                browser.draft('printf unexpected > ' + json.dumps(proof) + ' # ')
                doc = browser.call('DOM.getDocument')['root']['nodeId']
                node = browser.call('DOM.querySelector', {'nodeId': doc, 'selector': '.image-picker'})['nodeId']
                browser.call('DOM.setFileInputFiles', {'nodeId': node, 'files': ['/workspace/upload.png']})
                browser.wait(f'window.__e2e.uploads.length==={i + 1}')
                browser.wait("document.querySelector('.attachment-status')?.textContent.includes('Image reference inserted')")
                assert browser.js('window.__e2e.peers.length') == peer_count, 'upload reconnected peer'
                browser.live()
                # Check first draft was NOT submitted using host proof after this phase.
                # Discard pasted path with trusted Ctrl+U, never Enter.
                browser.click('.terminal-host')
                browser.key('u', 'KeyU', 85, modifiers=2)
            (ROOT / 'uploads.json').write_text(json.dumps(browser.js('window.__e2e.uploads')))
            # FIFO shell sentinel ensures host's no-Enter check is not a timing race.
            browser.shell("printf '%s%s\\n' UPLOADS_ DRAINED", 'UPLOADS_DRAINED')
        elif mode == 'reload':
            # Every action opens a new CDP session. New-document registrations belong
            # to the registering session, so install on THIS session before reload.
            # Never inject after load: that would miss the real application peer.
            browser.call('Page.addScriptToEvaluateOnNewDocument', {'source': OBSERVER})
            browser.js('window.__e2e.reloadFence=true')
            browser.call('Page.reload', {'ignoreCache': True})
            browser.wait('!!window.__e2e && !window.__e2e.reloadFence', label='reload observer installed')
            browser.live()
            assert browser.js("location.hash===''"), 'pairing fragment persisted'
            proof = str(Path(job['proofDir']) / 'reload-proof')
            browser.shell("printf '%s%s\\n' reload- proof > " + json.dumps(proof) +
                          "; printf '%s%s\\n' RELOAD_ OK", 'RELOAD_OK')
        elif mode == 'resume':
            count = browser.js('window.__e2e.peers.length')
            browser.js("window.__e2e.peers.filter(p=>p.connectionState==='connected').forEach(p=>p.close())")
            browser.wait(f'window.__e2e.peers.length>{count}', timeout=90)
            browser.live()
            proof = str(Path(job['proofDir']) / 'resume-proof')
            browser.shell("printf '%s%s\\n' resume- proof > " + json.dumps(proof) +
                          "; printf '%s%s\\n' RESUME_ OK", 'RESUME_OK')
        elif mode == 'connections':
            browser.live()
            target = browser.js('location.pathname')
            assert '/connections/' in target, 'acceptance condition'
            browser.call('Page.addScriptToEvaluateOnNewDocument', {'source': OBSERVER})
            browser.click('[aria-label="Toggle navigation"]')
            assert browser.js("document.querySelector('.drawer-brand')?.textContent==='Herdr-webapp' && ![...document.querySelectorAll('.navigation-settings button')].some(b=>['Connections','Install app'].includes(b.textContent.trim()))"), 'acceptance condition'
            assert browser.js("(()=>{const f=document.querySelector('.navigation-settings'),b=f?.querySelector('button');return !!f && f.textContent.trim()==='' && f.children.length===1 && b?.getAttribute('aria-label')==='Settings' && !!b.querySelector('svg') && b.getBoundingClientRect().width>=44 && b.getBoundingClientRect().height>=44})()"), 'acceptance condition'
            browser.click('.navigation-settings .settings-button[aria-haspopup="dialog"]')
            browser.wait("!!document.querySelector('.preferences[open] .install-option')")
            assert browser.js("!!document.querySelector('.preferences[open] a.source-link[href=\"https://github.com/perapp/herdr-webapp\"] svg')"), 'acceptance condition'
            browser.click('.version-option')
            browser.wait("!!document.querySelector('.version-dialog[open]') && !document.querySelector('.preferences[open]')")
            browser.wait("!!document.querySelector('[data-component=core]') && !!document.querySelector('[data-component=connector]')")
            assert browser.js("!!document.querySelector('.version-dialog[open] a.source-link[href=\"https://github.com/perapp/herdr-webapp\"] svg')"), 'acceptance condition'
            assert not browser.js("!!document.querySelector('.component-updates > .update-error')"), 'acceptance condition'
            browser.click('[aria-label="Close versions"]')
            browser.wait("!!document.querySelector('.preferences[open]') && document.activeElement.matches('.version-option')")
            browser.click('[aria-label="Close settings"]')
            # The branded drawer heading is the Connections action, even with one saved computer.
            browser.click('.drawer-brand')
            browser.wait("!!document.querySelector('[data-testid=remembered-computers]') && !document.querySelector('.xterm')")
            assert browser.js("!!document.querySelector('[data-testid=connections-page] a.source-link[href=\"https://github.com/perapp/herdr-webapp\"] svg')"), 'acceptance condition'
            browser.click('.connections-menu summary')
            assert browser.js("JSON.stringify([...document.querySelectorAll('.connections-menu button')].map(b=>b.textContent.trim()))") == '["Settings"]', 'acceptance condition'
            assert browser.js("document.querySelector('.connections-brand')?.textContent==='Herdr-webapp' && document.querySelector('.connections-brand').pathname==='/'"), 'acceptance condition'
            browser.click('.connections-menu button')
            browser.wait("!!document.querySelector('.settings-dialog[open] .install-option')")
            browser.click('[aria-label="Close settings"]')
            browser.call('Page.navigate', {'url': job['origin'] + '/launch' + ('?forceTurn=1' if job['transport'] != 'direct' else '')})
            browser.wait('location.pathname===' + json.dumps(target))
            browser.live()
            proof = str(Path(job['proofDir']) / 'launch-proof')
            browser.shell("printf '%s%s\\n' launch- proof > " + json.dumps(proof) +
                          "; printf '%s%s\\n' LAUNCH_ OK", 'LAUNCH_OK')
        elif mode == 'multi-computer':
            browser.live()
            first = browser.js('location.pathname')
            browser.call('Page.addScriptToEvaluateOnNewDocument', {'source': OBSERVER})
            fragment = base64.urlsafe_b64encode(json.dumps(job['secondPairing'], separators=(',', ':')).encode()).decode().rstrip('=')
            query = '?forceTurn=1' if job['transport'] != 'direct' else ''
            browser.call('Page.navigate', {'url': job['origin'] + '/' + query + '#pair=' + fragment})
            browser.wait('location.pathname!==' + json.dumps(first) + " && location.hash==='' && !!window.__e2e")
            browser.live()
            proof = str(Path(job['proofDir']) / 'second-computer-proof')
            browser.shell("printf '%s%s\\n' second-computer- proof > " + json.dumps(proof) +
                          "; printf '%s%s\\n' SECOND_COMPUTER_ OK", 'SECOND_COMPUTER_OK')
            # Computer selection remains available from Connections, not a
            # diagnostics dropdown in the icon-only drawer footer.
            browser.call('Page.navigate', {'url': job['origin'] + '/' + query})
            browser.wait("document.querySelectorAll('[data-testid=open-computer]').length===2")
            # A direct route must resume the first pinned key without a fresh pairing.
            browser.call('Page.navigate', {'url': job['origin'] + first + query})
            browser.wait('location.pathname===' + json.dumps(first) + ' && !!window.__e2e')
            browser.live()
            proof = str(Path(job['proofDir']) / 'first-computer-proof')
            browser.shell("printf '%s%s\\n' first-computer- proof > " + json.dumps(proof) +
                          "; printf '%s%s\\n' FIRST_COMPUTER_ OK", 'FIRST_COMPUTER_OK')
        elif mode == 'rotation':
            rotation_acceptance(browser, job)
        elif mode == 'mobile':
            browser.call('Emulation.setDeviceMetricsOverride', {'width': 390, 'height': 844, 'deviceScaleFactor': 1, 'mobile': True})
            browser.call('Emulation.setTouchEmulationEnabled', {'enabled': True})
            browser.wait("!!document.querySelector('.terminal-pane.compact')")
            browser.live()
            browser.click('[aria-label="Toggle navigation"]')
            assert browser.js("(()=>{const f=document.querySelector('.navigation-settings'),b=f?.querySelector('button');return !!f && f.textContent.trim()==='' && f.children.length===1 && b?.getAttribute('aria-label')==='Settings' && !!b.querySelector('svg') && b.getBoundingClientRect().width>=44 && b.getBoundingClientRect().height>=44})()"), 'acceptance condition'
            image = browser.call('Page.captureScreenshot', {'format': 'png', 'captureBeyondViewport': False})
            screenshot = ROOT / 'drawer-settings-phone.png'
            screenshot.write_bytes(base64.b64decode(image['data']))
            screenshot.chmod(0o600)
            browser.click('[aria-label="Close navigation"]')
            proof = str(Path(job['proofDir']) / 'mobile-proof')
            browser.shell("printf '%s%s\\n' mobile- proof > " + json.dumps(proof) +
                          "; printf '%s%s\\n' MOBILE_ OK", 'MOBILE_OK')
            # Observe every DOM transition, not only the final attached terminal.
            browser.js('''window.__paneSwitch={flashes:0,peers:window.__e2e.peers.length,path:location.pathname};
              window.__paneObserver=new MutationObserver(()=>{
                if(document.querySelector('[data-testid="connections-page"],.login-shell'))window.__paneSwitch.flashes++;
              });window.__paneObserver.observe(document.body,{childList:true,subtree:true});''')
            browser.wait("document.querySelectorAll('.pane-link').length===2")
            for index in [1, 0, 1, 0]:
                browser.click('[aria-label="Toggle navigation"]')
                browser.wait("document.querySelectorAll('.pane-link').length===2")
                browser.click(f'.pane-link:nth-child({index + 1})')
                browser.wait("!document.querySelector('#navigation-drawer[open]')")
                browser.live()
                assert browser.js("window.__e2e.peers.length===window.__paneSwitch.peers && location.pathname===window.__paneSwitch.path && window.__paneSwitch.flashes===0"), 'acceptance condition'
            proof = str(Path(job['proofDir']) / 'pane-switch-proof')
            browser.shell("printf '%s%s\\n' pane-switch- proof > " + json.dumps(proof) +
                          "; printf '%s%s\\n' PANE_SWITCH_ OK", 'PANE_SWITCH_OK')
            count = browser.js('window.__e2e.peers.length')
            browser.js("window.__e2e.peers.filter(p=>p.connectionState==='connected').forEach(p=>p.close())")
            browser.wait(f'window.__e2e.peers.length>{count}', timeout=90)
            browser.live()
            assert browser.js('window.__paneSwitch.flashes===0'), 'acceptance condition'
            browser.js('window.__paneObserver.disconnect()')
            proof = str(Path(job['proofDir']) / 'mobile-resume-proof')
            browser.shell("printf '%s%s\\n' mobile-resume- proof > " + json.dumps(proof) +
                          "; printf '%s%s\\n' MOBILE_RESUME_ OK", 'MOBILE_RESUME_OK')
        elif mode == 'large':
            browser.live()
            before = browser.js('window.__e2e.receivedBytes')
            proof = str(Path(job['proofDir']) / 'large-proof')
            # Bash builtin: does not assume Python is available on the native host.
            browser.shell("printf '%0400000d\\n' 0; printf '%s%s\\n' large- proof > " +
                          json.dumps(proof) + "; printf '%s%s\\n' CREDIT_ DRAINED", 'CREDIT_DRAINED')
            browser.wait(f'window.__e2e.receivedBytes-{before}>=400000')
        else:
            raise RuntimeError('unknown phase')
        stats = browser.stats(job['transport'])
        (ROOT / 'result.json').write_text(json.dumps({'ok': True, **stats}))
    finally:
        browser.ws.close()


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        # Only literal harness assertion labels may cross the private boundary.
        allowed = {'input page focused', 'click target visible and enabled', 'rotation confirmation visible', 'rotation-keeper-before-proof', 'rotation-other-before-proof', 'rotation-keeper-after-proof', 'rotation-second-existing-proof', 'rotation-second-pairing-proof', 'rotation-keeper-final-proof', 'draft editor ready', 'draft completion painted', 'draft guard retained', 'draft explicit enter', 'client link printed', 'observer installed', 'reload observer installed', 'terminal attached',
                   'peer connected', 'shell sentinel', 'acceptance condition',
                   'selected ICE pair missing', 'oversized SCTP frame', 'unexpected relay route',
                   'both peers must select relay', 'wrong TURN transport selected',
                   'upload reconnected peer', 'pairing fragment persisted', 'upload ready',
                   'mixed TURN transport configuration', 'rotation authorization rejected',
                   'rotation settings responsive', 'rotation completed', 'rotation keeper unchanged', 'rotation other revoked'}
        label = str(error) if isinstance(error, AssertionError) and str(error) in allowed else 'browser operation failed'
        trace = error.__traceback__
        lines = []
        while trace is not None:
            lines.append(trace.tb_lineno)
            trace = trace.tb_next
        trace = error.__traceback__
        caller_line = trace.tb_lineno
        while trace.tb_next is not None:
            caller_line = trace.tb_lineno
            trace = trace.tb_next
        (ROOT / 'result.json').write_text(json.dumps({'ok': False, 'assertion': label,
            'errorType': type(error).__name__, 'errorLine': trace.tb_lineno, 'callerLine': caller_line, 'stackLines': lines}))
        sys.exit(1)
