"""Tests for omarchy_sessions.py: python3 -B host/omarchy/test_omarchy_sessions.py"""
import json
import os
import queue
import socket
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).parent))
import omarchy_sessions as s  # noqa: E402

DEAD_PID = 2 ** 22 + 12345  # beyond pid_max on Linux and macOS


def fake_proc(tree):
    """tree: {pid: (comm, ppid, argv)} -> a /proc directory."""
    root = tempfile.mkdtemp()
    for pid, (comm, ppid, argv) in tree.items():
        d = Path(root) / str(pid)
        d.mkdir()
        (d / 'stat').write_text(f'{pid} ({comm}) S {ppid} 1 1 0 -1')
        (d / 'cmdline').write_bytes(b'\0'.join(a.encode() for a in argv) + b'\0')
    return root


TREE = {
    900: ('kitty', 1, ['kitty']),
    901: ('zsh', 900, ['zsh']),
    902: ('node', 901, ['node', '/usr/lib/claude']),
    903: ('sh', 902, ['sh', '-c', 'hook']),
    904: ('python3', 903, ['python3', 'omarchy_hook.py']),
}


class Processes(unittest.TestCase):
    def setUp(self):
        self.proc = fake_proc(TREE)

    def test_the_hook_finds_claude_through_its_shell_even_when_it_is_a_node_program(self):
        self.assertEqual(s.cli_process(904, 'claude', self.proc), 902)

    def test_an_unrelated_cli_is_not_found(self):
        self.assertIsNone(s.cli_process(904, 'codex', self.proc))

    def test_the_window_is_the_client_that_owns_an_ancestor(self):
        clients = [{'address': '0xa', 'pid': 111}, {'address': '0xb', 'pid': 900}]
        self.assertEqual(s.window_for(902, clients, '', self.proc)['address'], '0xb')

    def test_a_shared_terminal_process_picks_the_title_naming_the_project(self):
        clients = [{'address': '0x1', 'pid': 900, 'title': 'other', 'focusHistoryID': 0},
                   {'address': '0x2', 'pid': 900, 'title': 'claude · myproj', 'focusHistoryID': 3}]
        self.assertEqual(s.window_for(902, clients, '/home/me/myproj', self.proc)['address'], '0x2')

    def test_no_owning_window_is_none(self):
        self.assertIsNone(s.window_for(902, [{'address': '0xa', 'pid': 5}], '', self.proc))


class States(unittest.TestCase):
    def test_events_map_to_states(self):
        self.assertEqual(s.state_for('UserPromptSubmit', None), 'working')
        self.assertEqual(s.state_for('Stop', None), 'ready')
        self.assertEqual(s.state_for('Notification', 'permission_prompt'), 'question')
        self.assertEqual(s.state_for('Notification', 'idle_prompt'), 'ready')
        self.assertIsNone(s.state_for('Notification', 'auth_success'))
        self.assertIsNone(s.state_for('SessionEnd', None))

    def test_hook_validation_rejects_anything_odd(self):
        good = {'cli': 'claude', 'event': 'Stop', 'session_id': 'abc', 'cwd': '/x', 'pid': 42}
        self.assertTrue(s.valid_hook(good))
        for bad in ({**good, 'cli': 'rm'}, {**good, 'pid': 0}, {**good, 'session_id': ''},
                    {**good, 'pid': '42'}, [], None):
            self.assertFalse(s.valid_hook(bad))

    def test_an_order_is_one_printable_line(self):
        self.assertEqual(s.sanitize_order('hi\nthere\x1b[31m  you'), 'hi there [31m you')
        self.assertEqual(s.sanitize_order('请继续\r\n  好'), '请继续 好')
        self.assertEqual(len(s.sanitize_order('x' * 5000)), s.MAX_ORDER)


class FakeDeck:
    def __init__(self):
        self.calls, self.fail = [], None

    def __call__(self, method, params=None):
        self.calls.append((method, params))
        if self.fail:
            raise self.fail
        return {'agent': 'ag_1'} if method == 'agent.register' else {}


class FakeHypr:
    def __init__(self, address='0xb', follows=True):
        self.address, self.follows, self.focused = address, follows, []
        self.active = None

    def clients(self):
        return [{'address': self.address, 'pid': 900, 'title': 't'}]

    def active_address(self):
        return self.active

    def focus(self, address):
        self.focused.append(address)
        if self.follows:
            self.active = address


def hook(event='Stop', **extra):
    return {'cli': 'claude', 'event': event, 'session_id': 'sid1', 'cwd': '/home/me/proj',
            'pid': os.getpid(), **extra}


def make_bridge(deck=None, hypr=None, run=None, proc=None):
    run = run or (lambda cmd, **kw: SimpleNamespace(returncode=0))
    return s.Bridge(deck or FakeDeck(), hypr or FakeHypr(), host='box', run=run,
                    sleep=lambda _: None, proc=proc or '/nonexistent')


class BridgeSessions(unittest.TestCase):
    def test_a_first_event_registers_a_virtual_agent_without_a_pid_and_updates_its_state(self):
        deck = FakeDeck()
        bridge = make_bridge(deck)
        bridge.on_hook(hook('UserPromptSubmit'))
        register = dict(deck.calls)['agent.register']
        self.assertEqual(register['key'], 'omarchy:box:claude:sid1')
        self.assertEqual(register['name'], 'proj')
        self.assertEqual(register['caps'], {'answer': 'none', 'order': 'push', 'focus': True})
        self.assertNotIn('pid', register)
        self.assertNotIn('terminal', register)
        self.assertEqual(dict(deck.calls)['agent.update'], {'agent': 'ag_1', 'state': 'working', 'title': ''})

    def test_repeating_the_same_state_sends_nothing(self):
        deck = FakeDeck()
        bridge = make_bridge(deck)
        bridge.on_hook(hook('Stop'))
        bridge.on_hook(hook('Stop'))
        self.assertEqual([m for m, _ in deck.calls].count('agent.update'), 1)

    def test_a_question_carries_the_notification_line(self):
        deck = FakeDeck()
        bridge = make_bridge(deck)
        bridge.on_hook(hook('Notification', ntype='permission_prompt', message='Claude needs your permission to use Bash'))
        update = [p for m, p in deck.calls if m == 'agent.update'][-1]
        self.assertEqual(update['state'], 'question')
        self.assertIn('permission', update['title'])

    def test_session_end_releases_and_forgets(self):
        deck = FakeDeck()
        bridge = make_bridge(deck)
        bridge.on_hook(hook('Stop'))
        bridge.on_hook(hook('SessionEnd'))
        self.assertEqual(deck.calls[-1], ('agent.release', {'agent': 'ag_1', 'reason': 'session_end'}))
        self.assertEqual(bridge.sessions, {})

    def test_an_away_daemon_keeps_the_session_for_later(self):
        deck = FakeDeck()
        deck.fail = ConnectionError('away')
        bridge = make_bridge(deck)
        bridge.on_hook(hook('Stop'))
        self.assertEqual(len(bridge.dirty), 1)
        deck.fail = None
        bridge.flush()
        self.assertEqual(bridge.dirty, set())
        self.assertEqual(next(iter(bridge.sessions.values())).agent, 'ag_1')

    def test_a_dead_process_is_released_by_the_sweep(self):
        deck = FakeDeck()
        bridge = make_bridge(deck)
        bridge.on_hook({**hook('Stop'), 'pid': DEAD_PID})
        bridge.tick(1000.0)
        self.assertEqual(bridge.sessions, {})
        self.assertEqual(deck.calls[-1][0], 'agent.release')

    def test_a_reconnect_registers_again(self):
        deck = FakeDeck()
        bridge = make_bridge(deck)
        bridge.on_hook(hook('Stop'))
        before = [m for m, _ in deck.calls].count('agent.register')
        bridge.reregister()
        self.assertEqual([m for m, _ in deck.calls].count('agent.register'), before + 1)

    def test_a_taken_key_is_claimed_then_registered(self):
        class Taken(FakeDeck):
            def __call__(self, method, params=None):
                if method == 'agent.register' and not any(m == 'agent.claim' for m, _ in self.calls):
                    self.calls.append((method, params))
                    raise SimpleNamespace_error()
                return super().__call__(method, params)

        class SimpleNamespace_error(Exception):
            code = -32004

        deck = Taken()
        bridge = make_bridge(deck)
        bridge.on_hook(hook('Stop'))
        self.assertIn('agent.claim', [m for m, _ in deck.calls])
        self.assertEqual(next(iter(bridge.sessions.values())).agent, 'ag_1')


class BridgeOrders(unittest.TestCase):
    def setUp(self):
        self.proc = fake_proc({os.getpid(): ('claude', 900, ['claude']), 900: ('kitty', 1, ['kitty'])})
        self.typed = []

        def run(cmd, **kw):
            self.typed.append(cmd)
            return SimpleNamespace(returncode=0)

        self.run = run

    def bridge(self, hypr=None):
        bridge = make_bridge(hypr=hypr or FakeHypr(), run=self.run, proc=self.proc)
        bridge.on_hook(hook('Stop'))
        return bridge, next(iter(bridge.sessions.values()))

    def test_an_order_focuses_the_window_then_types_the_line_and_presses_return(self):
        hypr = FakeHypr()
        bridge, session = self.bridge(hypr)
        self.assertIsNone(bridge.deliver_order(session, '请继续\n谢谢'))
        self.assertEqual(hypr.focused, ['0xb'])
        self.assertEqual(self.typed, [['wtype', '--', '请继续 谢谢'], ['wtype', '-k', 'Return']])

    def test_an_open_question_dialog_is_never_typed_into(self):
        bridge, session = self.bridge()
        session.state = 'question'
        self.assertEqual(bridge.deliver_order(session, 'yes'), 'dialog_open')
        self.assertEqual(self.typed, [])

    def test_nothing_is_typed_when_the_window_never_gets_focus(self):
        bridge, session = self.bridge(FakeHypr(follows=False))
        self.assertEqual(bridge.deliver_order(session, 'hello'), 'focus_failed')
        self.assertEqual(self.typed, [])

    def test_a_missing_window_or_process_or_empty_text_is_refused(self):
        bridge, session = self.bridge()
        self.assertEqual(bridge.deliver_order(session, '  \n '), 'empty')
        session.pid = DEAD_PID
        self.assertEqual(bridge.deliver_order(session, 'hi'), 'session_gone')
        bridge2 = make_bridge(hypr=FakeHypr(), run=self.run, proc=fake_proc({os.getpid(): ('claude', 1, ['claude'])}))
        bridge2.on_hook(hook('Stop'))
        self.assertEqual(bridge2.deliver_order(next(iter(bridge2.sessions.values())), 'hi'), 'no_window')

    def test_a_failed_wtype_reports_it(self):
        bridge, session = self.bridge()
        bridge.run = lambda cmd, **kw: SimpleNamespace(returncode=1)
        self.assertEqual(bridge.deliver_order(session, 'hi'), 'wtype_failed')

    def test_selecting_a_row_on_the_deck_raises_its_window(self):
        hypr = FakeHypr()
        bridge, session = self.bridge(hypr)
        bridge.on_event({'type': 'focus.changed', 'data': {'agent': 'ag_1', 'source': 'device'}})
        bridge.on_event({'type': 'focus.changed', 'data': {'agent': 'ag_1', 'source': 'desktop'}})
        self.assertEqual(hypr.focused, ['0xb'])

    def test_orders_for_other_agents_are_ignored(self):
        bridge, _ = self.bridge()
        bridge.on_event({'type': 'agent.order', 'data': {'agent': 'ag_other', 'text': 'x'}})
        self.assertEqual(self.typed, [])


class Control(unittest.TestCase):
    def test_a_hook_line_reaches_the_queue_through_a_real_owner_only_socket(self):
        directory = tempfile.mkdtemp()
        events = queue.Queue()
        server = s.ControlServer(Path(directory) / 'c.sock', events)
        server.bind()
        server.start()
        self.assertEqual(os.stat(server.path).st_mode & 0o777, 0o600)
        with socket.socket(socket.AF_UNIX) as sock:
            sock.connect(str(server.path))
            sock.sendall(json.dumps(hook('Stop')).encode() + b'\n')
        self.assertEqual(events.get(timeout=2)['event'], 'Stop')
        server.listener.close()


if __name__ == '__main__':
    unittest.main()
