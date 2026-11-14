"""Tests for omarchy_deck.py: python3 -m unittest host/examples/test_omarchy_deck.py"""
import json
import os
import re
import socket
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

sys.path.insert(0, str(Path(__file__).parent))
import omarchy_deck as o  # noqa: E402


def client(address, ws, cls='kitty', title='', x=0, y=0, focus=5, **extra):
    return {'address': address, 'workspace': {'id': ws}, 'class': cls, 'title': title,
            'at': [x, y], 'focusHistoryID': focus, 'mapped': True, 'hidden': False, **extra}


MONITORS = [{'id': 0, 'focused': True, 'activeWorkspace': {'id': 3}}]
WORKSPACES = [{'id': 1, 'monitorID': 0, 'windows': 1}, {'id': 3, 'monitorID': 0, 'windows': 2},
              {'id': 5, 'monitorID': 0, 'windows': 1}, {'id': 12, 'monitorID': 1, 'windows': 4}]


class BuildWs(unittest.TestCase):
    def test_chips_cover_the_highest_occupied_slot_of_the_focused_monitor(self):
        line, _ = o.build_ws(MONITORS, WORKSPACES, [])
        self.assertEqual((line['active'], line['count']), (3, 5))

    def test_second_monitor_block_is_shown_as_slots_one_to_ten(self):
        monitors = [{'id': 1, 'focused': True, 'activeWorkspace': {'id': 12}}]
        line, _ = o.build_ws(monitors, WORKSPACES, [])
        self.assertEqual((line['active'], line['count']), (2, 2))

    def test_windows_are_the_active_workspaces_in_screen_order_with_the_focused_one_selected(self):
        clients = [client('0xb', 3, title='right', x=900, focus=0), client('0xa', 3, title='left', x=10),
                   client('0xc', 1, title='other workspace')]
        line, addresses = o.build_ws(MONITORS, WORKSPACES, clients)
        self.assertEqual(addresses, ['0xa', '0xb'])
        self.assertEqual(line['sel'], 1)
        self.assertEqual(line['windows'][1]['tone'], 'blue')
        self.assertTrue(line['windows'][0]['title'].startswith('kitty — left'))

    def test_at_most_six_rows_and_the_focused_window_always_shows(self):
        clients = [client(f'0x{i}', 3, x=i, focus=i + 1) for i in range(8)]
        clients[7]['focusHistoryID'] = 0
        line, addresses = o.build_ws(MONITORS, WORKSPACES, clients)
        self.assertEqual(len(line['windows']), 6)
        self.assertIn('0x7', addresses)
        self.assertEqual(addresses[line['sel']], '0x7')

    def test_the_line_is_one_the_daemon_accepts(self):
        clients = [client('0xa', 3, title='t' * 80)]
        line, _ = o.build_ws(MONITORS, WORKSPACES, clients)
        self.assertLessEqual(line['active'], line['count'])
        self.assertLessEqual(line['count'], 9)
        self.assertLess(line['sel'], max(1, len(line['windows'])))
        self.assertLessEqual(len(line['windows'][0]['title']), 48)

    def test_slot_ten_is_not_drawn_but_never_breaks_the_line(self):
        monitors = [{'id': 0, 'focused': True, 'activeWorkspace': {'id': 10}}]
        line, _ = o.build_ws(monitors, WORKSPACES, [])
        self.assertEqual(line['active'], 0)
        self.assertLessEqual(line['count'], 9)

    def test_no_focused_monitor_sends_nothing(self):
        self.assertIsNone(o.build_ws([{'id': 0, 'focused': False, 'activeWorkspace': {'id': 1}}], [], []))


class Keys(unittest.TestCase):
    def remote(self):
        remote = o.Remote()
        remote.addresses = ['0xa', '0xb']
        return remote

    def test_wheel_steps_cycle_windows_and_a_release_does_nothing(self):
        with mock.patch.object(o, 'dispatch') as d:
            self.remote().on_key({'k': 'ccw', 'act': 2, 'shift': 0})
            self.remote().on_key({'k': 'enter', 'act': 0, 'shift': 0})
        d.assert_called_once_with('hl.dsp.window.cycle_next()', ['cyclenext'])

    def test_left_and_right_step_workspace_slots_and_wrap_and_shift_moves_window_focus(self):
        monitors = lambda ws: [{'focused': True, 'activeWorkspace': {'id': ws}}]
        for active, key, want in ((7, 'left', 6), (7, 'right', 8), (1, 'left', 10), (10, 'right', 1)):
            with mock.patch.object(o, 'hyprctl_json', return_value=monitors(active)), \
                    mock.patch.object(o, 'focus_slot') as f:
                self.remote().on_key({'k': key, 'act': 1, 'shift': 0})
            f.assert_called_once_with(want)
        with mock.patch.object(o, 'dispatch') as d:
            self.remote().on_key({'k': 'left', 'act': 1, 'shift': 1})
        d.assert_called_once_with('hl.dsp.focus({ direction = "l" })', ['movefocus', 'l'])

    def test_home_cancel_toggles_the_scratchpad_and_home_enter_sends_a_window_there(self):
        with mock.patch.object(o, 'dispatch') as d:
            self.remote().on_key({'k': 'cancel', 'act': 1, 'shift': 1})
            self.remote().on_key({'k': 'enter', 'act': 1, 'shift': 1})
        self.assertEqual([c.args[1] for c in d.call_args_list],
                         [['togglespecialworkspace', 'scratchpad'], ['movetoworkspacesilent', 'special:scratchpad']])

    def test_an_open_scratchpad_lists_its_own_windows(self):
        monitors = [{'id': 0, 'focused': True, 'activeWorkspace': {'id': 7}, 'specialWorkspace': {'id': -98}}]
        clients = [client('0xa', 7, title='main'), client('0xs', -98, title='notes')]
        line, addresses = o.build_ws(monitors, [], clients)
        self.assertEqual(addresses, ['0xs'])
        self.assertTrue(line['windows'][0]['title'].endswith('notes'))

    def test_unmapped_keys_are_ignored(self):
        with mock.patch.object(o, 'dispatch') as d:
            self.remote().on_key({'k': 'cancel', 'act': 1, 'shift': 0})
        d.assert_not_called()

    def test_tapping_a_row_focuses_its_window_and_a_stale_index_is_ignored(self):
        with mock.patch.object(o, 'focus_window') as f:
            self.remote().on_ws({'win': 1})
            self.remote().on_ws({'win': 7})
        f.assert_called_once_with('0xb')

    def test_tapping_a_chip_goes_to_that_slot(self):
        with mock.patch.object(o, 'focus_slot') as f:
            self.remote().on_ws({'go': 4})
        f.assert_called_once_with(4)


class Macropad(unittest.TestCase):
    """deckd types Macropad keys only on macOS: on Linux the bridge types them with wtype."""

    LAYOUT = {'3': {'keys': [{'label': 'Shot', 'keys': 'cmd+shift+4'}, {'label': 'Note'},
                             {'label': 'Bad', 'keys': 'cmd+nope'}]}}

    def remote(self, platform='linux'):
        remote = o.Remote()
        calls = {'service.info': {'platform': platform, 'session': 'live'},
                 'state.get': {'agents': {}, 'macros': self.LAYOUT}}
        remote.rpc = mock.Mock(call=mock.Mock(side_effect=lambda method, params=None: calls[method]))
        remote.run = mock.Mock(return_value=SimpleNamespace(returncode=0))
        remote.link_at_hello(0.0)
        return remote

    def press(self, remote, f, i, act=1):
        with mock.patch.object(o, 'ensure_instance'):
            remote.handle({'type': 'device.macro', 'data': {'f': f, 'i': i, 'act': act}})

    def test_combos_become_wtype_arguments(self):
        self.assertEqual(o.wtype_args('cmd+shift+4'), ['wtype', '-M', 'logo', '-M', 'shift', '-k', '4',
                                                       '-m', 'shift', '-m', 'logo'])
        self.assertEqual(o.wtype_args('ctrl+opt+pageup'), ['wtype', '-M', 'ctrl', '-M', 'alt', '-k', 'Prior',
                                                           '-m', 'alt', '-m', 'ctrl'])
        self.assertEqual(o.wtype_args('enter'), ['wtype', '-k', 'Return'])
        for bad in ('cmd', 'a+b', 'cmd+nope', ''):
            self.assertIsNone(o.wtype_args(bad), bad)

    def test_every_deck_key_has_a_keysym(self):
        # Read from deck-proto itself, so a key deckd starts accepting cannot go untypeable here.
        types = Path(__file__).resolve().parent.parent / 'crates/deck-proto/src/types.rs'
        listed = re.search(r'KEYSTROKE_KEYS: &\[&str\] = &\[(.*?)\];', types.read_text(), re.S)
        keys = re.findall(r'"([^"]+)"', listed[1])
        self.assertIn('right', keys)
        for key in keys:
            self.assertIsNotNone(o.wtype_args(key), key)

    def test_a_press_types_its_keystroke_and_nothing_else_does(self):
        remote = self.remote()
        self.press(remote, 3, 0)
        self.assertEqual(remote.run.call_args[0][0], o.wtype_args('cmd+shift+4'))
        self.press(remote, 3, 0, act=0)  # a release
        self.press(remote, 3, 1)  # an event-only key
        self.press(remote, 3, 2)  # not typeable
        self.press(remote, 3, 7)  # no such slot
        self.press(remote, 9, 0)  # no such face
        self.assertEqual(remote.run.call_count, 1)

    def test_layout_changes_follow_the_events(self):
        remote = self.remote()
        remote.handle({'type': 'device.macros', 'data': {'f': 4, 'src': 'deck', 'keys': [{'label': 'x', 'keys': 'x'}]}})
        self.press(remote, 4, 0)
        self.assertEqual(remote.run.call_args[0][0], ['wtype', '-k', 'x'])
        remote.handle({'type': 'device.profile', 'data': {'macros': {}}})
        self.press(remote, 4, 0)
        self.assertEqual(remote.run.call_count, 1)

    def test_a_layout_event_with_a_bad_face_id_is_passed_over(self):
        remote = self.remote()
        for f in (None, 'x'):
            remote.handle({'type': 'device.macros', 'data': {'f': f, 'keys': [{'label': 'x', 'keys': 'x'}]}})
        remote.handle({'type': 'device.profile', 'data': {'macros': {'nope': {'keys': []}, '5': {'keys': []}}}})
        self.assertEqual(remote.macros, {5: []})

    def test_a_macs_deckd_types_them_on_the_mac(self):
        remote = self.remote(platform='macos')
        self.press(remote, 3, 0)
        remote.run.assert_not_called()


class Resilience(unittest.TestCase):
    def test_a_hyprctl_that_times_out_does_not_stop_the_bridge(self):
        import subprocess
        remote = o.Remote()
        remote.push = lambda force=False: (_ for _ in ()).throw(subprocess.TimeoutExpired('hyprctl', 2))
        remote.try_push(force=True)  # must not raise
        remote.push = lambda force=False: (_ for _ in ()).throw(RuntimeError('hyprctl failed'))
        remote.try_push()
        remote.push = lambda force=False: (_ for _ in ()).throw(ValueError('bad json'))
        remote.try_push()

    def test_a_key_pressed_while_hyprland_is_away_is_dropped_not_fatal(self):
        remote = o.Remote()
        with mock.patch.object(o, 'dispatch', side_effect=RuntimeError('down')):
            remote.on_key({'k': 'enter', 'act': 1, 'shift': 0})


class KimiSeed(unittest.TestCase):
    def test_with_hooks_the_log_reader_runs_once_and_without_them_always(self):
        remote = o.Remote.__new__(o.Remote)
        remote.kimi_seeded = False
        remote.kimi = mock.Mock(poll=mock.Mock(return_value=['m']))
        with mock.patch.object(o, 'hooks_installed', return_value=True):
            self.assertEqual(remote.kimi_messages(), ['m'])
            self.assertEqual(remote.kimi_messages(), [])
        with mock.patch.object(o, 'hooks_installed', return_value=False):
            self.assertEqual(remote.kimi_messages(), ['m'])
            self.assertEqual(remote.kimi_messages(), ['m'])


class Quiet(unittest.TestCase):
    """Like deckd, the bridge goes quiet after ten minutes of `device.link` saying no deck,
    and wakes the moment one links."""

    def remote(self):
        remote = o.Remote()
        remote.bridge = mock.Mock()
        remote.try_push = mock.Mock()
        return remote

    def test_ten_minutes_without_a_deck_make_it_quiet_and_a_link_wakes_it_at_once(self):
        remote = self.remote()
        remote.on_link({'link': 'offline', 'session': 'absent', 'host_online': False}, 100.0)
        self.assertFalse(remote.tick_quiet(100.0 + o.DORMANT_AFTER_S - 1), '9:59')
        # A second report of the same drop does not restart the window.
        remote.on_link({'link': 'offline', 'session': 'absent', 'host_online': False}, 650.0)
        self.assertTrue(remote.tick_quiet(100.0 + o.DORMANT_AFTER_S))
        remote.bridge.reregister.assert_not_called()
        remote.try_push.assert_not_called()
        remote.on_link({'link': 'connected', 'session': 'live', 'host_online': True}, 900.0)
        self.assertFalse(remote.tick_quiet(900.0))
        remote.bridge.reregister.assert_called_once_with()
        remote.try_push.assert_called_once_with(force=True)

    def test_a_deck_back_inside_the_window_never_makes_it_quiet(self):
        remote = self.remote()
        remote.on_link({'host_online': False}, 0.0)
        remote.on_link({'host_online': True}, 300.0)
        remote.on_link({'host_online': False}, 500.0)
        self.assertFalse(remote.tick_quiet(1_000.0), 'ten minutes since the second drop have not passed')
        self.assertTrue(remote.tick_quiet(1_100.0))

    def test_the_hello_reads_the_link_and_a_deckd_already_quiet(self):
        remote = self.remote()
        remote.rpc = mock.Mock(call=mock.Mock(return_value={'session': 'live', 'collecting': True}))
        remote.link_at_hello(50.0)
        self.assertIsNone(remote.away_since)
        remote.rpc.call.return_value = {'session': 'absent', 'collecting': False}
        remote.link_at_hello(50.0)
        self.assertTrue(remote.dormant(50.0), 'deckd is quiet already: so is the bridge')
        remote.away_since = None
        remote.rpc.call.return_value = {'session': 'absent'}  # a deckd that predates dormancy
        remote.link_at_hello(50.0)
        self.assertFalse(remote.dormant(50.0 + o.DORMANT_AFTER_S - 1))
        self.assertTrue(remote.dormant(50.0 + o.DORMANT_AFTER_S))
        remote.away_since = None
        remote.rpc.call.side_effect = o.RpcError({'code': -32002, 'message': 'forbidden'})
        remote.link_at_hello(50.0)
        self.assertEqual(remote.away_since, 50.0)
        remote.rpc.call.assert_called_with('service.info')

    def test_while_quiet_the_hooks_reach_the_bridge_marked_quiet(self):
        remote = self.remote()
        for event in ('SessionStart', 'Stop'):
            remote.hooks.put({'cli': 'claude', 'event': event, 'session_id': 's1', 'pid': 4242})
        remote.take_hooks(quiet=True)
        self.assertTrue(remote.hooks.empty())
        self.assertEqual([(c.args[0]['event'], c.kwargs) for c in remote.bridge.on_hook.call_args_list],
                         [('SessionStart', {'quiet': True}), ('Stop', {'quiet': True})])


class QuietDeckd(threading.Thread):
    """A deckd whose deck is away and which is quiet itself (or, with `info`, whatever `service.info`
    says); `link()` brings the deck back and `unlink()` takes it away again."""

    def __init__(self, path, info=None):
        super().__init__(daemon=True)
        self.calls, self.requests, self.conn, self.lock = [], [], None, threading.Lock()
        self.info = info or {'session': 'absent', 'collecting': False}
        self.agents = {}
        self.server = socket.socket(socket.AF_UNIX)
        self.server.bind(str(path))
        self.server.listen(1)

    def answer(self, method, params):
        if method == 'service.info':
            return self.info
        if method == 'agent.register':  # one agent per session key, as deckd's register is
            return {'agent': self.agents.setdefault(params['key'], f'ag_{len(self.agents) + 1}')}
        return {}

    def close(self):
        """End the connection, which ends the bridge's `serve`."""
        if self.conn is not None:
            try:
                self.conn.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass

    def send(self, message):
        with self.lock:
            self.conn.sendall(json.dumps(message).encode() + b'\n')

    def run(self):
        self.conn, _ = self.server.accept()
        buffer = b''
        with self.conn:
            while True:
                try:
                    chunk = self.conn.recv(65536)
                except OSError:
                    return
                if not chunk:
                    return
                buffer += chunk
                while b'\n' in buffer:
                    line, buffer = buffer.split(b'\n', 1)
                    request = json.loads(line)
                    self.calls.append(request['method'])
                    self.requests.append((request['method'], request.get('params') or {}))
                    result = self.answer(request['method'], request.get('params') or {})
                    self.send({'jsonrpc': '2.0', 'id': request['id'], 'result': result})

    def link(self):
        self.send({'jsonrpc': '2.0', 'method': 'event', 'params': {
            'type': 'device.link', 'rev': 9,
            'data': {'link': 'connected', 'session': 'live', 'host_online': True}}})

    def unlink(self):
        self.send({'jsonrpc': '2.0', 'method': 'event', 'params': {
            'type': 'device.link', 'rev': 10,
            'data': {'link': 'offline', 'session': 'absent', 'host_online': False}}})


class FakeHypr:
    """One window, owned by this test process, which takes focus when asked."""

    def __init__(self):
        self.active = None

    def clients(self):
        return [{'address': '0xa', 'pid': os.getpid(), 'title': 'proj'}]

    def active_address(self):
        return self.active

    def focus(self, address):
        self.active = address


def wait_for(done, what, timeout=5.0):
    deadline = time.time() + timeout
    while not done():
        if time.time() > deadline:
            raise AssertionError(f'timed out waiting for {what}')
        time.sleep(0.02)


class QuietServe(unittest.TestCase):
    def test_a_quiet_deckd_gets_no_face_and_no_session_until_the_deck_links(self):
        directory = Path(tempfile.mkdtemp())
        deckd = QuietDeckd(directory / 'deckd.sock')
        deckd.start()
        remote = o.Remote()
        remote.snapshot = lambda: ({'active': 1, 'count': 1, 'sel': 0, 'windows': []}, [])
        polls = []
        remote.kimi_messages = lambda: polls.append(1) or []
        hook = {'cli': 'claude', 'event': 'Stop', 'session_id': 'sid1', 'cwd': '/home/me/proj',
                'pid': os.getpid()}
        remote.hooks.put(hook)

        def serve():
            try:
                remote.serve(directory / 'deckd.sock')
            except (ConnectionError, OSError):
                pass

        threading.Thread(target=serve, daemon=True).start()
        time.sleep(1.0)  # several turns of the loop
        self.assertEqual(deckd.calls, ['session.hello', 'service.info'], 'no face, no roster')
        self.assertEqual(polls, [], 'no Kimi log read')
        self.assertTrue(remote.hooks.empty(), 'the hook was taken, and nothing sent for it')
        deckd.link()
        remote.hooks.put(hook)
        deadline = time.time() + 5
        while not ({'device.ws.set', 'agent.register'} <= set(deckd.calls) and polls):
            self.assertLess(time.time(), deadline, deckd.calls)
            time.sleep(0.05)


    def test_the_wake_shows_each_session_as_the_hooks_left_it_while_quiet(self):
        """Hooks that arrive while the bridge is quiet still move its own table, so the wake neither
        shows a session as it was ten minutes ago nor types into a dialog opened meanwhile."""
        directory = Path(tempfile.mkdtemp())
        deckd = QuietDeckd(directory / 'deckd.sock', info={'session': 'live', 'collecting': True})
        deckd.start()
        self.addCleanup(deckd.close)
        typed = []
        remote = o.Remote()
        remote.bridge = o.Bridge(remote.call, FakeHypr(), host='box',
                                 run=lambda argv, **_: typed.append(argv) or SimpleNamespace(returncode=0),
                                 sleep=lambda _: None, proc='/nonexistent')
        remote.snapshot = lambda: ({'active': 1, 'count': 1, 'sel': 0, 'windows': []}, [])
        remote.kimi_messages = lambda: []
        hook = {'cli': 'claude', 'cwd': '/home/me/proj', 'pid': os.getpid()}

        def serve():
            try:
                remote.serve(directory / 'deckd.sock')
            except (ConnectionError, OSError):
                pass

        with mock.patch.object(o, 'DORMANT_AFTER_S', 0.3):  # ten minutes, shortened
            threading.Thread(target=serve, daemon=True).start()
            for sid in ('s1', 's2'):
                remote.hooks.put({**hook, 'session_id': sid, 'event': 'UserPromptSubmit'})
            wait_for(lambda: deckd.calls.count('agent.update') == 2, 'both sessions working')
            deckd.unlink()
            wait_for(lambda: remote.quiet, 'the bridge going quiet')
            before = len(deckd.requests)
            remote.hooks.put({**hook, 'session_id': 's1', 'event': 'PermissionRequest', 'message': 'Run it?'})
            remote.hooks.put({**hook, 'session_id': 's2', 'event': 'SessionEnd'})
            wait_for(remote.hooks.empty, 'the quiet loop taking the hooks')
            time.sleep(0.3)  # a few more turns of the quiet loop
            self.assertEqual(deckd.requests[before:], [], 'nothing is sent while quiet')
            deckd.link()
            release = ('agent.release', {'agent': 'ag_2', 'reason': 'session_end'})
            wait_for(lambda: release in deckd.requests and 'device.ws.set' in deckd.calls[before:],
                     'the wake')
        woke = deckd.requests[before:]
        self.assertIn(('agent.update', {'agent': 'ag_1', 'state': 'question', 'title': 'Run it?'}), woke)
        self.assertNotIn(('agent.update', {'agent': 'ag_1', 'state': 'working', 'title': ''}), woke)
        self.assertNotIn('s2', ''.join(remote.bridge.sessions), 'the ended session is gone')
        session = remote.bridge.by_agent('ag_1')
        self.assertEqual(remote.bridge.deliver_order(session, 'also update the README'), 'dialog_open')
        self.assertEqual(typed, [], 'nothing typed into the open dialog')


class Instance(unittest.TestCase):
    def serve(self, root, name):
        import socket as _s
        d = Path(root) / 'hypr' / name
        d.mkdir(parents=True)
        srv = _s.socket(_s.AF_UNIX)
        srv.bind(str(d / '.socket.sock'))
        srv.listen(16)  # nobody accepts: leave room for every probe
        self.addCleanup(srv.close)
        return d

    def test_a_stale_signature_is_replaced_by_the_live_instance(self):
        import os as _os
        import tempfile as _t
        import omarchy_hypr as hy
        with _t.TemporaryDirectory(dir='/tmp') as root:
            (Path(root) / 'hypr' / 'dead_1').mkdir(parents=True)  # a directory with no socket
            self.serve(root, 'live_2')
            env = {'XDG_RUNTIME_DIR': root, 'HYPRLAND_INSTANCE_SIGNATURE': 'dead_1'}
            with mock.patch.dict(_os.environ, env):
                hy.ensure_instance()
                self.assertEqual(_os.environ['HYPRLAND_INSTANCE_SIGNATURE'], 'live_2')
            with mock.patch.dict(_os.environ, {'XDG_RUNTIME_DIR': root}, clear=True):
                hy.ensure_instance()
                self.assertEqual(_os.environ['HYPRLAND_INSTANCE_SIGNATURE'], 'live_2')

class SocketPath(unittest.TestCase):
    """Without --socket the bridge looks where deckd listens (config.rs `Paths::detect`)."""

    def path(self, env):
        import os as _os
        with mock.patch.dict(_os.environ, env, clear=True):
            return o.socket_path()

    # Paths that exist on no machine, so a real tunnel or deckd changes nothing.
    HOME = '/nonexistent/home'
    RUN = '/nonexistent/run'

    def test_the_named_socket_and_the_data_dir_come_first(self):
        env = {'HOME': self.HOME, 'XDG_RUNTIME_DIR': self.RUN, 'DECKD_DATA_DIR': '/data'}
        self.assertEqual(self.path(env), Path('/data/deckd.sock'))
        self.assertEqual(self.path({**env, 'DECKD_SOCKET': '/tmp/s.sock'}), Path('/tmp/s.sock'))

    def test_deckds_own_socket_is_under_the_runtime_dir_else_the_cache_dir(self):
        cases = [
            ({'XDG_RUNTIME_DIR': self.RUN, 'XDG_CACHE_HOME': '/c'}, f'{self.RUN}/openwalkietalkie/deckd.sock'),
            ({'XDG_CACHE_HOME': '/c'}, '/c/openwalkietalkie/deckd.sock'),
            # A relative XDG variable does not count, as in deckd.
            ({'XDG_RUNTIME_DIR': 'run', 'XDG_CACHE_HOME': 'c'}, f'{self.HOME}/.cache/openwalkietalkie/deckd.sock'),
            ({}, f'{self.HOME}/.cache/openwalkietalkie/deckd.sock'),
        ]
        for env, want in cases:
            self.assertEqual(self.path({'HOME': self.HOME, **env}), Path(want), env)

    def test_an_old_tunnels_end_is_used_only_while_deckds_own_socket_is_absent(self):
        import tempfile as _t
        with _t.TemporaryDirectory(dir='/tmp') as runtime:
            env = {'HOME': self.HOME, 'XDG_RUNTIME_DIR': runtime}
            own = Path(runtime) / 'openwalkietalkie' / 'deckd.sock'
            self.assertEqual(self.path(env), own, 'neither is there: deckd\'s own')
            tunnel = Path(runtime) / 'deckd.sock'
            tunnel.touch()
            self.assertEqual(self.path(env), tunnel, 'only the old tunnel end is there')
            own.parent.mkdir()
            own.touch()
            self.assertEqual(self.path(env), own, 'deckd\'s own wins once it is there')


if __name__ == '__main__':
    unittest.main()
