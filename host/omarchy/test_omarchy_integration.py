"""End to end against a fake deckd: python3 -B host/omarchy/test_omarchy_integration.py"""
import json
import os
import socket
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).parent))
import omarchy_deck as o  # noqa: E402
from test_omarchy_sessions import FakeHypr, fake_proc  # noqa: E402


class FakeDeckd(threading.Thread):
    """Answers JSON-RPC lines; pushes one agent.order after the first agent.update."""

    def __init__(self, path):
        super().__init__(daemon=True)
        self.calls, self.sent_order = [], False
        self.server = socket.socket(socket.AF_UNIX)
        self.server.bind(str(path))
        self.server.listen(1)

    def run(self):
        conn, _ = self.server.accept()
        buffer = b''
        with conn:
            while True:
                try:
                    chunk = conn.recv(65536)
                except OSError:
                    return
                if not chunk:
                    return
                buffer += chunk
                while b'\n' in buffer:
                    line, buffer = buffer.split(b'\n', 1)
                    request = json.loads(line)
                    self.calls.append((request['method'], request['params']))
                    result = {'agent': 'ag_1'} if request['method'] == 'agent.register' else {}
                    conn.sendall(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}).encode() + b'\n')
                    if request['method'] == 'agent.update' and not self.sent_order:
                        self.sent_order = True
                        event = {'jsonrpc': '2.0', 'method': 'event', 'params': {
                            'type': 'agent.order', 'data': {'agent': 'ag_1', 'text': '请继续'}}}
                        conn.sendall(json.dumps(event).encode() + b'\n')


class Integration(unittest.TestCase):
    def test_hook_to_roster_and_order_to_keystrokes(self):
        directory = Path(tempfile.mkdtemp())
        deckd = FakeDeckd(directory / 'deckd.sock')
        deckd.start()
        typed = []
        remote = o.Remote()
        remote.snapshot = lambda: ({'active': 1, 'count': 1, 'sel': 0, 'windows': []}, [])
        remote.bridge.hypr = FakeHypr()
        remote.bridge.proc = fake_proc({os.getpid(): ('claude', 900, ['claude']), 900: ('kitty', 1, ['kitty'])})
        remote.bridge.sleep = lambda _: None
        remote.bridge.run = lambda cmd, **kw: typed.append(cmd) or SimpleNamespace(returncode=0)
        remote.hooks.put({'cli': 'claude', 'event': 'Stop', 'session_id': 'sid1',
                          'cwd': '/home/me/proj', 'pid': os.getpid()})
        errors = []

        def serve():
            try:
                remote.serve(directory / 'deckd.sock')
            except (ConnectionError, OSError) as error:
                errors.append(error)

        thread = threading.Thread(target=serve, daemon=True)
        thread.start()
        deadline = time.time() + 5
        while len(typed) < 2 and time.time() < deadline:
            time.sleep(0.05)
        methods = [m for m, _ in deckd.calls]
        hello = dict(deckd.calls)['session.hello']
        self.assertIn('agent', hello['scopes'])
        self.assertIn('agent.order', hello['subscribe'])
        self.assertIn('device.ws.set', methods)
        self.assertLess(methods.index('agent.register'), methods.index('agent.update'))
        self.assertEqual(typed, [['wtype', '--', '请继续'], ['wtype', '-k', 'Return']])


if __name__ == '__main__':
    unittest.main()
