"""Tests for omarchy_deck.py: python3 -m unittest host/examples/test_omarchy_deck.py"""
import sys
import unittest
from pathlib import Path
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

    def test_left_is_the_previous_workspace_and_shift_left_the_window_to_the_left(self):
        with mock.patch.object(o, 'dispatch') as d:
            self.remote().on_key({'k': 'left', 'act': 1, 'shift': 0})
        d.assert_called_once_with('hl.dsp.focus({ workspace = "m-1" })', ['workspace', 'm-1'])
        with mock.patch.object(o, 'dispatch') as d:
            self.remote().on_key({'k': 'left', 'act': 1, 'shift': 1})
        d.assert_called_once_with('hl.dsp.focus({ direction = "l" })', ['movefocus', 'l'])

    def test_unmapped_keys_are_ignored(self):
        with mock.patch.object(o, 'dispatch') as d:
            self.remote().on_key({'k': 'cancel', 'act': 1, 'shift': 1})
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

if __name__ == '__main__':
    unittest.main()
