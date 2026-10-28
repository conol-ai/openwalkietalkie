"""Backend-independent contract checks for the downloadable companion example."""
import importlib.util
import json
from pathlib import Path
import socket
import sys
import threading
import unittest
from collections import deque

SPEC = importlib.util.spec_from_file_location('native_companion',
    Path(__file__).resolve().parents[2] / 'host/examples/native_companion.py')
native = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = native
SPEC.loader.exec_module(native)


class Backend:
    def __init__(self):
        self.calls = []
        self.ids = {}
        self.card = 0

    def call(self, method, params=None):
        params = params or {}
        self.calls.append((method, params))
        if method == 'agent.register':
            key = params['key']
            self.ids.setdefault(key, f'ag_{len(self.ids) + 1}')
            return {'agent': self.ids[key]}
        if method == 'agent.card.set':
            assert len(params['choices']) <= 4
            self.card += 1
            return {'card': {'id': self.card}}
        return {'rev': len(self.calls)}


class CompanionTests(unittest.TestCase):
    def setUp(self):
        self.backend = Backend()
        self.app = native.Companion()
        self.app.connect(self.backend)
        self.rev = 0

    def send(self, kind, chat, **data):
        self.rev += 1
        event = {'type': kind, 'rev': self.rev, 'data': {'agent': chat.agent, **data}}
        self.app.event(event)
        return event

    def new_chat(self, text='hello'):
        self.send('agent.order', self.app.chats[0], text=text)
        return self.app.chats[-1]

    def answer(self, chat, **data):
        self.send('agent.answer', chat, card=chat.card_id, **data)

    def test_new_chat_is_published_before_focus_and_duplicate_order_is_ignored(self):
        event = self.send('agent.order', self.app.chats[0], text='hello')
        chat = self.app.chats[-1]
        calls = self.backend.calls
        focus = next(i for i, (m, p) in enumerate(calls) if m == 'agent.focus')
        for method in ['agent.register', 'agent.companion.set', 'agent.session.set', 'agent.reply']:
            self.assertTrue(any(m == method for m, p in calls[:focus]))
        self.assertEqual(calls[focus][1]['agent'], chat.agent)
        self.app.event(event)
        self.assertEqual(len(self.app.chats), 2)
        self.send('agent.order', chat, text='follow up')
        self.assertEqual([m['text'] for m in chat.messages if m['message']['role'] == 'user'], ['hello', 'follow up'])
        self.assertTrue(all(p.get('kind') == 'other' for m, p in calls if m == 'agent.register'))
        self.assertTrue(any(p['profile']['role'] == 'launcher' for m, p in calls if m == 'agent.companion.set'))

    def test_form_pages_toggles_text_review_and_exact_card_guard(self):
        chat = self.new_chat('form')
        old_card = chat.card_id
        self.answer(chat, choice=2)  # More: TypeScript / Go
        self.assertNotEqual(old_card, chat.card_id)
        self.send('agent.answer', chat, card=old_card, choice=0)
        self.assertEqual(chat.form.step, 0)
        self.answer(chat, choice=1)  # Go
        self.answer(chat, choice=0)  # Tests selected
        self.answer(chat, choice=2)  # More: UI / CLI
        self.answer(chat, choice=1)  # CLI selected
        self.answer(chat, choice=3)  # Next
        self.answer(chat, text='Build a useful tool')
        self.assertEqual(chat.form.step, 3)
        self.assertEqual(chat.form.tools, {'Tests', 'CLI'})
        self.assertFalse(any(m == 'agent.card.clear' and p.get('delivered') for m, p in self.backend.calls))
        self.answer(chat, choice=0)  # Submit
        self.assertIsNone(chat.form)
        self.assertIn('Submitted: Go; CLI, Tests; Build a useful tool', chat.messages[-1]['text'])
        self.assertEqual(sum(m == 'agent.card.clear' and p.get('delivered', False) for m, p in self.backend.calls), 1)

    def test_cancel_does_not_submit_and_reconnect_restores_draft(self):
        chat = self.new_chat('form')
        self.answer(chat, choice=0)
        self.answer(chat, choice=0)
        draft = chat.form
        self.app.connect(self.backend)
        self.assertIs(chat.form, draft)
        self.assertEqual(chat.form.tools, {'Tests'})
        self.answer(chat, choice=3)
        self.answer(chat, choice=1)  # Cancel free-text step
        self.assertIsNone(chat.form)
        self.assertIn('nothing submitted', chat.messages[-1]['text'])
        self.assertFalse(any(m == 'agent.card.clear' and p.get('delivered') for m, p in self.backend.calls))

    def test_settings_confirm_exact_revision_and_actual_supported_values(self):
        chat = self.new_chat()
        self.send('session.settings_requested', chat, model='brief', effort='', request_rev=22)
        method, params = self.backend.calls[-1]
        self.assertEqual(method, 'agent.session.set')
        self.assertEqual(params['request_rev'], 22)
        self.assertEqual(params['controls']['model'], 'brief')
        self.send('session.settings_requested', chat, model='missing', effort='high', request_rev=23)
        params = self.backend.calls[-1][1]
        self.assertEqual(params['controls']['model'], 'brief')
        self.assertIn('retained', params['controls']['note'])

    def test_reconnect_after_daemon_restart_replaces_old_card_identity(self):
        chat = self.new_chat('form')
        self.answer(chat, choice=0)
        replacement = Backend()
        replacement.ids['unrelated'] = 'ag_reserved'
        self.app.connect(replacement)
        self.assertFalse(any(m == 'agent.card.clear' for m, p in replacement.calls))
        self.assertIsNotNone(chat.card_id)
        self.assertEqual(chat.form.step, 1)


class RpcTests(unittest.TestCase):
    def test_interleaved_events_do_not_steal_the_response(self):
        client, server = socket.socketpair()
        rpc = native.Rpc.__new__(native.Rpc)
        rpc.sock, rpc.buffer, rpc.events, rpc.seq = client, b'', deque(), 0
        def respond():
            request = json.loads(server.recv(65536))
            event = {'jsonrpc': '2.0', 'method': 'event',
                     'params': {'type': 'focus.changed', 'rev': 1, 'data': {}}}
            response = {'jsonrpc': '2.0', 'id': request['id'], 'result': {'agent': 'ag_demo'}}
            server.sendall((json.dumps(event) + '\n' + json.dumps(response) + '\n').encode())
        thread = threading.Thread(target=respond)
        thread.start()
        try:
            self.assertEqual(rpc.call('agent.register', {}), {'agent': 'ag_demo'})
            self.assertEqual(rpc.events.popleft()['type'], 'focus.changed')
        finally:
            thread.join(timeout=5)
            client.close()
            server.close()


if __name__ == '__main__':
    unittest.main()
