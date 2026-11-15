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
window.__e2e={peers:[],uploads:[],receivedBytes:0,tail:'',maxFrame:0};
const NativePeer=window.RTCPeerConnection;
window.RTCPeerConnection=class extends NativePeer {
  constructor(...args){super(...args);window.__e2e.peers.push(this);}
  createDataChannel(...args){
    const channel=super.createDataChannel(...args), partial=new Map();
    const send=channel.send.bind(channel);
    channel.send=data=>{window.__e2e.maxFrame=Math.max(window.__e2e.maxFrame,data.byteLength||0);return send(data);};
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
    def __init__(self):
        self.seq = 0
        deadline = time.monotonic() + 30
        while True:
            try:
                with urllib.request.urlopen('http://127.0.0.1:9222/json/list', timeout=2) as response:
                    tabs = json.load(response)
                tab = next(t for t in tabs if t['type'] == 'page')
                break
            except Exception:
                if time.monotonic() > deadline:
                    raise RuntimeError('CDP unavailable') from None
                time.sleep(.2)
        self.ws = websocket.create_connection(tab['webSocketDebuggerUrl'],
                   origin='http://gui-workstation-control.invalid', timeout=15)
        self.call('Page.enable')
        self.call('Runtime.enable')

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
                if self.js(expression):
                    return
            except NavigationPending:
                pass  # Retry only context turnover, never arbitrary page errors.
            time.sleep(.1)
        raise AssertionError(label)

    def live(self):
        self.wait("!!document.querySelector('.xterm-helper-textarea') && "
                  "(!!document.querySelector('.pane-name .dot.live') || "
                  "(!!document.querySelector('.terminal-pane.compact') && !document.querySelector('.terminal-detail')))", label='terminal attached')
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
        self.wait('(()=>{const e=document.querySelector(' + json.dumps(selector) +
                  ');if(!e)return false;const r=e.getBoundingClientRect();return r.width>0&&r.height>0&&!e.disabled})()',
                  label='click target visible and enabled')
        rect = self.js('(()=>{const r=document.querySelector(' + json.dumps(selector) +
                       ').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})()')
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
        self.call('Input.insertText', {'text': text})

    def shell(self, text, marker):
        self.js('window.__e2e.tail=""')
        self.draft(text)
        self.key('Enter', 'Enter', 13)
        self.wait('window.__e2e.tail.includes(' + json.dumps(marker) + ')', label='shell sentinel')

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
        allowed = {'observer installed', 'reload observer installed', 'terminal attached',
                   'peer connected', 'shell sentinel', 'acceptance condition',
                   'selected ICE pair missing', 'oversized SCTP frame', 'unexpected relay route',
                   'both peers must select relay', 'wrong TURN transport selected',
                   'upload reconnected peer', 'pairing fragment persisted', 'upload ready',
                   'mixed TURN transport configuration'}
        label = str(error) if isinstance(error, AssertionError) and str(error) in allowed else 'browser operation failed'
        (ROOT / 'result.json').write_text(json.dumps({'ok': False, 'assertion': label}))
        sys.exit(1)
