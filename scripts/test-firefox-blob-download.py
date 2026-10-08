"""Isolated Firefox/Marionette smoke test; never opens the user's profile.

The ChatGPT hostname resolves to our local TLS fixture only inside this profile.
Native Messaging is stubbed by default. --probe-native reads the registered host
without submitting a download or starting a desktop application.
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import socket
import ssl
import subprocess
import tempfile
import threading
import time
import zipfile


class Marionette:
    def __init__(self, port):
        deadline = time.monotonic() + 30
        while True:
            try:
                self.sock = socket.create_connection(('127.0.0.1', port), 1)
                break
            except OSError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(.1)
        self.sock.settimeout(30)
        self.reader = self.sock.makefile('rb')
        self.counter = 0
        self.read()

    def read(self):
        length = b''
        while True:
            char = self.reader.read(1)
            if not char:
                raise EOFError('Firefox closed Marionette')
            if char == b':':
                break
            length += char
        return json.loads(self.reader.read(int(length)))

    def command(self, name, **params):
        self.counter += 1
        payload = json.dumps([0, self.counter, name, params]).encode()
        self.sock.sendall(str(len(payload)).encode() + b':' + payload)
        _, number, error, result = self.read()
        assert number == self.counter
        if error:
            raise RuntimeError(f'{name}: {error}')
        return result

    def script(self, script):
        result = self.command('WebDriver:ExecuteScript', script=script,
                              args=[], newSandbox=False, sandbox=None)
        return result.get('value', result) if isinstance(result, dict) else result


class Fixture(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b'%PDF-1.7\nfictional fixture\n' if self.path == '/file' else b'''<!doctype html>
<button id="mapped" onclick="start(true)">Mapped</button>
<button id="unknown" onclick="start(false)">Unknown</button>
<script>async function start(mapped) {
  const blob = mapped ? await (await fetch('/file', {headers:{Authorization:'Bearer fictional'}})).blob()
    : new Blob(['locally generated']);
  const a = document.createElement('a'); a.href = URL.createObjectURL(blob);
  a.download = 'fixture.bin'; a.click(); URL.revokeObjectURL(a.href);
}</script>'''
        self.send_response(200)
        self.send_header('Content-Type', 'application/octet-stream' if self.path == '/file' else 'text/html')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--firefox', default=r'C:\Program Files\Mozilla Firefox\firefox.exe')
    parser.add_argument('--openssl', default='openssl')
    parser.add_argument('--probe-native', action='store_true',
                        help='Read-only probe of the registered Native host using an isolated Firefox profile')
    parser.add_argument('--reject-native', action='store_true',
                        help='Reject a blob handoff and verify fallback followed by repeated page downloads')
    args = parser.parse_args()
    if args.probe_native and args.reject_native:
        parser.error('--probe-native and --reject-native cannot be combined')
    root = Path(__file__).resolve().parent.parent
    with tempfile.TemporaryDirectory(prefix='curl-firefox-blob-') as temp:
        work = Path(temp)
        profile = work / 'profile'
        profile.mkdir()
        port = free_port()
        prefs = {'marionette.port': port, 'network.dns.localDomains': 'chatgpt.com',
                 'network.proxy.type': 0, 'network.trr.mode': 5,
                 'browser.download.useDownloadDir': True,
                 'browser.download.dir': str(work), 'browser.download.folderList': 2,
                 'browser.shell.checkDefaultBrowser': False, 'app.update.auto': False,
                 'datareporting.policy.dataSubmissionEnabled': False,
                 'toolkit.telemetry.enabled': False}
        (profile / 'user.js').write_text(''.join(
            f'user_pref({json.dumps(k)}, {json.dumps(v)});\n' for k, v in prefs.items()))
        subprocess.run([args.openssl, 'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
                        '-keyout', str(work / 'key.pem'), '-out', str(work / 'cert.pem'),
                        '-days', '1', '-subj', '/CN=chatgpt.com'], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Fixture)
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(work / 'cert.pem', work / 'key.pem')
        server.socket = tls.wrap_socket(server.socket, server_side=True)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        xpi = work / 'test.xpi'
        with zipfile.ZipFile(xpi, 'w') as archive:
            for path in (root / 'firefox-extension').rglob('*'):
                if not path.is_file() or 'tests' in path.parts:
                    continue
                name = path.relative_to(root / 'firefox-extension').as_posix()
                data = path.read_bytes()
                if name == 'native-session.js' and not args.probe_native:
                    if args.reject_native:
                        data += b"\nCurlDownloaderNativeSession = () => ({send: async m => m.type === 'enqueue' ? ({type:'enqueue_result',request_id:m.request_id,ok:false,error:{code:'invalid_request_context',message:'Fictional test rejection'}}) : ({type:'defaults',request_id:m.request_id,target_dir:''}),setKeepAlive(){},close(){}});\n"
                    else:
                        data += b"\nCurlDownloaderNativeSession = () => ({send: async m => ({type:'defaults',request_id:m.request_id,target_dir:''}),setKeepAlive(){},close(){}});\n"
                if name == 'request-context.js':
                    data += b"""\nvar blobSmokeTrace=[];
const smokeCreateTracker=CurlExtensionRequestContext.createRequestContextTracker;
CurlExtensionRequestContext.createRequestContextTracker=(opts)=>{
 const tracker=smokeCreateTracker(opts);
 for(const method of ['observeSendHeaders','claimDownload']){
  const original=tracker[method];tracker[method]=(...args)=>{
   const result=original(...args);blobSmokeTrace.push({method,args,result});return result;
  };
 }return tracker;
};\n"""
                if name == 'background.js' and args.probe_native:
                    data += b"\nbrowser.tabs.create({url:browser.runtime.getURL('settings.html?downloadId=-999999')});\n"
                archive.writestr(name, data)
        process = subprocess.Popen([args.firefox, '--headless', '--no-remote', '--marionette',
                                    '--profile', str(profile)], stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL,
                                   creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
        client = None
        try:
            client = Marionette(port)
            session = client.command('WebDriver:NewSession', acceptInsecureCerts=True)
            print('Firefox:', session.get('capabilities', {}).get('browserVersion', 'connected'), flush=True)
            client.command('Addon:Install', path=str(xpi), temporary=True)
            # Temporary installation completes before the persistent background finishes starting.
            time.sleep(1)
            if args.probe_native:
                handles = client.command('WebDriver:GetWindowHandles')
                if isinstance(handles, dict):
                    handles = handles.get('value', [])
                for handle in handles:
                    client.command('WebDriver:SwitchToWindow', handle=handle)
                    if 'settings.html?' in client.command('WebDriver:GetCurrentURL').get('value', ''):
                        break
                probe = client.command('WebDriver:ExecuteAsyncScript', args=[], sandbox=None,
                    newSandbox=False, script="""const done=arguments[arguments.length-1];
browser.runtime.getBackgroundPage().then(bg=>bg.CurlDownloaderBackground.sendNativeWithRetry(
{type:'get_defaults',allow_start:false},{attempts:1})).then(r=>done({
type:r.type,code:r.error?.code||null}),e=>done({connectionError:e.message}));""")['value']
                print('Native host read-only probe:', probe, flush=True)
                assert probe.get('type') == 'defaults' or probe.get('code') in (
                    'gui_not_running', 'manually_stopped'), probe
                return
            client.command('WebDriver:Navigate', url=f'https://chatgpt.com:{server.server_port}/')
            source = client.command('WebDriver:GetWindowHandle').get('value')
            cases = [
                ('mapped', True, 'use-firefox'), ('unknown', False, 'close'), ('unknown', False, 'cancel')]
            if args.reject_native:
                cases = [('mapped', True, 'reject-curl'), ('mapped', True, 'use-firefox'),
                         ('mapped', True, 'use-firefox')] + cases[1:]
            for button, supported, decision in cases:
                client.script(f"document.getElementById('{button}').click();")
                deadline = time.monotonic() + 10
                while True:
                    handles = client.command('WebDriver:GetWindowHandles')
                    if isinstance(handles, dict):
                        handles = handles.get('value', [])
                    others = [handle for handle in handles if handle != source]
                    if others:
                        break
                    if time.monotonic() >= deadline:
                        raise AssertionError(f'{button}: settings did not open')
                    time.sleep(.1)
                client.command('WebDriver:SwitchToWindow', handle=others[0])
                url = client.command('WebDriver:GetCurrentURL').get('value', '')
                while url == 'about:blank' and time.monotonic() < deadline:
                    time.sleep(.05)
                    url = client.command('WebDriver:GetCurrentURL').get('value', '')
                assert 'settings.html?downloadId=-' in url, url
                result = client.command('WebDriver:ExecuteAsyncScript', args=[], sandbox=None,
                    newSandbox=False, script="""const done = arguments[arguments.length-1];
browser.runtime.sendMessage({type:'get-pending',downloadId:Number(new URL(location.href).searchParams.get('downloadId'))}).then(done);""")['value']
                if result['download']['externalSupported'] is not supported:
                    trace = client.command('WebDriver:ExecuteAsyncScript', args=[], sandbox=None,
                        newSandbox=False, script="""const done=arguments[arguments.length-1];
browser.runtime.getBackgroundPage().then(bg=>done(bg.blobSmokeTrace));""")['value']
                    raise AssertionError(f'{result}; isolated fixture trace={trace}')
                print(f'{button}: settings opened; externalSupported={supported}', flush=True)
                if not supported:
                    deadline = time.monotonic() + 5
                    while not client.script("return document.getElementById('submit-external')?.disabled === true;"):
                        if time.monotonic() >= deadline:
                            raise AssertionError('Unknown blob enabled Curl submission')
                        time.sleep(.05)
                # Explicit fallback and closing the choice both preserve the Blob after site revocation.
                if decision == 'reject-curl':
                    client.script("document.getElementById('target-dir').value=" + json.dumps(str(work)) + ";")
                    client.script("document.getElementById('submit-external').click();")
                    deadline = time.monotonic() + 10
                    while True:
                        try:
                            status = client.script("return document.getElementById('status').textContent;")
                        except RuntimeError as error:
                            raise AssertionError('Rejected blob closed its retry settings') from error
                        if 'invalid_request_context' in status:
                            break
                        if time.monotonic() >= deadline:
                            raise AssertionError(f'Rejection lost native error code: {status}')
                        time.sleep(.1)
                    time.sleep(1)
                    assert others[0] in client.command('WebDriver:GetWindowHandles'), 'Diagnostic settings closed'
                    assert client.script("return document.getElementById('submit-external').disabled;"), 'Restored download can be resubmitted'
                    assert client.script("return document.getElementById('cancel').textContent;") == '關閉'
                    client.script("document.getElementById('cancel').click();")
                    deadline = time.monotonic() + 5
                    while others[0] in client.command('WebDriver:GetWindowHandles'):
                        if time.monotonic() >= deadline:
                            raise AssertionError('Diagnostic close retried an already restored blob')
                        time.sleep(.05)
                    print('Rejected blob retained its diagnostic settings; then closed cleanly', flush=True)
                elif decision == 'close':
                    client.command('WebDriver:CloseWindow')
                else:
                    client.script(f"document.getElementById('{decision}').click();")
                    deadline = time.monotonic() + 5
                    while others[0] in client.command('WebDriver:GetWindowHandles'):
                        if time.monotonic() >= deadline:
                            raise AssertionError(f'{decision}: choice did not close')
                        time.sleep(.05)
                client.command('WebDriver:SwitchToWindow', handle=source)
                if decision == 'cancel':
                    time.sleep(.3)
                    assert not any(path.stat().st_size > 0 for path in work.glob('fixture*.bin'))
                    print('unknown: cancel did not download a file', flush=True)
                    continue
                deadline = time.monotonic() + 10
                while not any(path.stat().st_size > 0 and not Path(str(path) + '.part').exists()
                              for path in work.glob('fixture*.bin')):
                    if time.monotonic() >= deadline:
                        raise AssertionError(f'{button}: Firefox fallback lost the retained Blob')
                    time.sleep(.1)
                complete = [path.read_bytes() for path in work.glob('fixture*.bin') if path.stat().st_size > 0]
                expected = b'%PDF-1.7\nfictional fixture\n' if supported else b'locally generated'
                assert expected in complete, complete
                for downloaded in work.glob('fixture*.bin'):
                    downloaded.unlink()
                print(f'{button}: Firefox fallback saved nonempty file', flush=True)
        finally:
            if client is not None:
                try:
                    client.command('Marionette:Quit', flags=['eForceQuit'])
                except (OSError, EOFError, RuntimeError):
                    pass
                client.reader.close()
                client.sock.close()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=15)
            server.shutdown()
            server.server_close()


if __name__ == '__main__':
    main()
