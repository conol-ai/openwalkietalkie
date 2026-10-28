#!/usr/bin/env python3
"""Native companion example for OpenWalkieTalkie 0.11.0+ (Python 3.10+).

Run while OpenWalkieTalkie is open: python3 native_companion.py
Select Example / + New chat on the deck and dictate. Say 'form' in a chat
for a paged choice, multiple-selection and free-text wizard. Settings offers
two SCRIPTED models; this example makes no model API calls. Ctrl-C releases
its registrations. Replace Companion.order and apply_settings for your agent.
"""
from __future__ import annotations

import argparse
from collections import deque
from dataclasses import dataclass, field
import json
import os
from pathlib import Path
import select
import socket
import sys
import time
import uuid


EVENTS = ['agent.order', 'agent.answer', 'focus.changed', 'session.settings_requested']
MODELS = [
    {'id': 'echo', 'label': 'Demo echo', 'efforts': ['low', 'high'], 'default_effort': 'low'},
    {'id': 'brief', 'label': 'Demo brief', 'efforts': [], 'default_effort': ''},
]


def socket_path():
    if os.environ.get('DECKD_DATA_DIR'):
        return Path(os.environ['DECKD_DATA_DIR']) / 'deckd.sock'
    if sys.platform == 'darwin':
        return Path.home() / 'Library/Application Support/OpenWalkieTalkie/deckd.sock'
    return Path(os.environ.get('XDG_RUNTIME_DIR', Path.home() / '.local/state')) / 'openwalkietalkie/deckd.sock'


class RpcError(Exception):
    def __init__(self, error):
        self.code = error['code']
        super().__init__(error['message'])


class Rpc:
    """One ordered connection; queue events while waiting for an RPC response."""
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            self.sock.settimeout(5)
            self.sock.connect(str(path))
        except BaseException:
            self.sock.close()
            raise
        self.buffer = b''
        self.events = deque()
        self.seq = 0

    def receive(self, timeout):
        if b'\n' not in self.buffer:
            if not select.select([self.sock], [], [], max(0, timeout))[0]:
                return None
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError('daemon disconnected')
            self.buffer += chunk
            if len(self.buffer) > 2 * 1024 * 1024:
                raise ConnectionError('oversized daemon frame')
            if b'\n' not in self.buffer:
                return None
        line, self.buffer = self.buffer.split(b'\n', 1)
        return json.loads(line)

    def call(self, method, params=None):
        self.seq += 1
        request_id = self.seq
        self.sock.sendall((json.dumps({'jsonrpc': '2.0', 'id': request_id,
            'method': method, 'params': params or {}}) + '\n').encode())
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            message = self.receive(deadline - time.monotonic())
            if message is None:
                continue
            if message.get('method') == 'event':
                self.queue_event(message['params'])
            elif message.get('id') == request_id:
                if 'error' in message:
                    raise RpcError(message['error'])
                return message['result']
        # The caller must treat a timed-out mutation as uncertain, not retry it.
        raise TimeoutError(method)

    def queue_event(self, event):
        if len(self.events) >= 1024:
            raise ConnectionError('event backlog exceeded')
        self.events.append(event)

    def close(self):
        self.sock.close()


@dataclass
class Form:
    step: int = 0
    page: int = 0
    language: str = ''
    tools: set = field(default_factory=set)
    goal: str = ''

    def card(self):
        if self.step == 0:
            options = ['Python', 'Rust', 'TypeScript', 'Go', 'Other language']
            labels = options[self.page * 2:self.page * 2 + 2] + ['More', 'Cancel']
            return '1/3 Choose a language', labels, False
        if self.step == 1:
            options = ['Tests', 'Docs', 'UI', 'CLI'][self.page * 2:self.page * 2 + 2]
            labels = [('✓ ' if v in self.tools else '○ ') + v for v in options] + ['More', 'Next']
            return '2/3 Select any tools, then Next', labels, False
        if self.step == 2:
            return '3/3 Hold PTT and describe the goal', ['Back', 'Cancel'], True
        return f'Review: {self.language}; {", ".join(sorted(self.tools)) or "no tools"}; {self.goal}', ['Submit', 'Edit', 'Cancel'], False

    def answer(self, choice, text):
        _, labels, free_text = self.card()
        if text is not None:
            if not free_text or not text.strip():
                return 'ignore'
            self.goal = text.strip()[:500]
            self.step = 3
            return 'show'
        if not isinstance(choice, int) or not 0 <= choice < len(labels):
            return 'ignore'
        label = labels[choice]
        if label == 'Cancel':
            return 'cancel'
        if self.step == 0:
            if label == 'More':
                self.page = (self.page + 1) % 3
            else:
                self.language = label
                self.step, self.page = 1, 0
        elif self.step == 1:
            if label == 'More':
                self.page = (self.page + 1) % 2
            elif label == 'Next':
                self.step = 2
            else:
                value = label[2:]
                if value in self.tools:
                    self.tools.remove(value)
                else:
                    self.tools.add(value)
        elif self.step == 2:
            self.step, self.page = 1, 0  # Back
        elif label == 'Submit':
            return 'submit'
        else:
            self.step, self.page = 0, 0  # Edit keeps draft answers.
        return 'show'


@dataclass
class Chat:
    key: str
    name: str
    launcher: bool = False
    agent: str = ''
    model: str = 'echo'
    effort: str = 'low'
    messages: list = field(default_factory=list)
    form: Form | None = None
    card_id: int | None = None

    def controls(self):
        return {'model': self.model, 'effort': self.effort, 'models': MODELS,
            'editable': not self.launcher, 'note': 'Scripted demo; no model API calls.'}


class Companion:
    def __init__(self):
        # Unique process identity avoids conflicts with another running demo.
        # Real apps should persist account + conversation keys across restarts.
        self.instance = uuid.uuid4().hex
        self.chats = [Chat(f'example:{self.instance}:new', '+ New chat', launcher=True)]
        self.rpc = None
        self.seen = set()

    def connect(self, rpc):
        self.rpc = rpc
        self.seen.clear()
        rpc.call('session.hello', {'client': {'name': 'Native companion example', 'version': '1'},
            'scopes': ['agent', 'observe'], 'subscribe': EVENTS, 'snapshot': False})
        for chat in self.chats:
            self.register(chat)
            if chat.form:
                self.show_form(chat)

    def register(self, chat):
        params = {'kind': 'other', 'name': chat.name, 'key': chat.key,
            'caps': {'answer': 'push', 'order': 'push', 'focus': True}, 'lease_ttl_s': 30}
        try:
            result = self.rpc.call('agent.register', params)
        except RpcError as error:
            if error.code != -32004:
                raise
            self.rpc.call('agent.claim', {'key': chat.key})
            result = self.rpc.call('agent.register', params)
        if chat.agent != result['agent']:
            chat.card_id = None
        chat.agent = result['agent']
        self.rpc.call('agent.companion.set', {'agent': chat.agent, 'profile': {
            'app_id': 'org.example.companion', 'group_id': self.instance, 'group_label': 'Example',
            'role': 'launcher' if chat.launcher else 'session'}})
        self.rpc.call('agent.session.set', {'agent': chat.agent, 'controls': chat.controls()})
        self.rpc.call('agent.update', {'agent': chat.agent, 'state': 'ready'})
        for message in chat.messages:
            self.rpc.call('agent.reply', {'agent': chat.agent, **message})

    def reply(self, chat, role, text):
        message = {'text': text[:2000], 'speak': False,
            'message': {'id': uuid.uuid4().hex, 'role': role, 'streaming': False}}
        chat.messages.append(message)
        chat.messages = chat.messages[-16:]
        self.rpc.call('agent.reply', {'agent': chat.agent, **message})

    def order(self, chat, text):
        if chat.launcher:
            # In production: create in YOUR backend first, then publish and focus.
            if len(self.chats) >= 8:
                old = next((c for c in self.chats if not c.launcher and not c.form), None)
                if old is None:
                    self.reply(chat, 'assistant', 'Finish an open form before starting another demo chat.')
                    return
                self.rpc.call('agent.release', {'agent': old.agent})
                self.chats.remove(old)
            chat = Chat(f'example:{self.instance}:{uuid.uuid4().hex}', text[:32] or 'New chat')
            self.chats.append(chat)
            self.register(chat)
            self.reply(chat, 'user', text)
            self.rpc.call('agent.focus', {'agent': chat.agent})
        else:
            self.reply(chat, 'user', text)
        if chat.form:
            # Don't let normal dictation bypass the active form's exact card id.
            return
        if 'form' in text.lower():
            chat.form = Form()
            self.show_form(chat)
        else:
            self.reply(chat, 'assistant', f'Demo ({chat.model}, {chat.effort or "default"}): '
                + (text if chat.model == 'echo' else 'Message received.'))

    def show_form(self, chat):
        if chat.card_id is not None:
            self.rpc.call('agent.card.clear', {'agent': chat.agent, 'card': chat.card_id, 'delivered': False})
        text, labels, free_text = chat.form.card()
        result = self.rpc.call('agent.card.set', {'agent': chat.agent, 'type': 'q', 'text': text,
            'choices': [{'label': label} for label in labels], 'free_text': free_text,
            'deliver': 'push', 'origin': {'kind': 'custom'}})
        chat.card_id = result['card']['id']

    def answer(self, chat, data):
        if not chat.form or data.get('card') != chat.card_id:
            return
        outcome = chat.form.answer(data.get('choice'), data.get('text'))
        if outcome == 'ignore':
            return
        if outcome == 'show':
            self.show_form(chat)
            return
        form = chat.form
        # Only this explicit Submit branch should call your pending AskUser tool.
        text = (f'Submitted: {form.language}; {", ".join(sorted(form.tools))}; {form.goal}'
                if outcome == 'submit' else 'Form cancelled; nothing submitted.')
        self.rpc.call('agent.card.clear', {'agent': chat.agent, 'card': chat.card_id,
            'delivered': outcome == 'submit'})
        chat.form, chat.card_id = None, None
        self.reply(chat, 'assistant', text)

    def apply_settings(self, chat, data):
        # Real integrations must apply this to their backend before confirming.
        model = next((m for m in MODELS if m['id'] == data.get('model')), None)
        if model is None or (data.get('effort') and data['effort'] not in model['efforts']):
            controls = {**chat.controls(), 'note': 'Unsupported setting; current settings retained.'}
        else:
            chat.model, chat.effort = data['model'], data.get('effort', '')
            controls = chat.controls()
        self.rpc.call('agent.session.set', {'agent': chat.agent,
            'controls': controls, 'request_rev': data['request_rev']})

    def event(self, event):
        key = (event['rev'], event['type'])
        if key in self.seen:
            return
        self.seen.add(key)
        if len(self.seen) > 4096:
            self.seen = {key}
        data = event.get('data', {})
        chat = next((c for c in self.chats if c.agent == data.get('agent')), None)
        if chat is None:
            return
        kind = event['type']
        if kind == 'agent.order' and data.get('text', '').strip():
            self.order(chat, data['text'])
        elif kind == 'agent.answer':
            self.answer(chat, data)
        elif kind == 'session.settings_requested':
            self.apply_settings(chat, data)
        elif kind == 'focus.changed' and data.get('source') == 'device':
            # Your app should select its matching conversation / blank composer.
            print('Selected:', chat.name, flush=True)

    def run(self, path):
        try:
            while True:
                try:
                    rpc = Rpc(path)
                    self.rpc = rpc
                    self.connect(rpc)
                    print('Connected. Select Example / + New chat on the deck.', flush=True)
                    heartbeat = time.monotonic()
                    while True:
                        now = time.monotonic()
                        if now >= heartbeat:
                            rpc.call('agent.heartbeat', {'agents': [c.agent for c in self.chats]})
                            heartbeat = now + 10
                        if rpc.events:
                            self.event(rpc.events.popleft())
                        else:
                            message = rpc.receive(min(1, max(0, heartbeat - time.monotonic())))
                            if message and message.get('method') == 'event':
                                rpc.queue_event(message['params'])
                except (OSError, ConnectionError, TimeoutError, RpcError) as error:
                    print(f'Connection interrupted ({error}); reconnecting in 5s.', file=sys.stderr)
                    if isinstance(error, RpcError) and error.code == -32601:
                        print('Requires OpenWalkieTalkie 0.11.0 or newer.', file=sys.stderr)
                        return
                    if self.rpc:
                        self.rpc.close()
                        self.rpc = None
                    time.sleep(5)
        finally:
            if self.rpc:
                for chat in self.chats:
                    try:
                        self.rpc.call('agent.release', {'agent': chat.agent})
                    except (OSError, ConnectionError, TimeoutError, RpcError):
                        break
                self.rpc.close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--socket', type=Path, default=socket_path())
    args = parser.parse_args()
    try:
        Companion().run(args.socket)
    except KeyboardInterrupt:
        pass
