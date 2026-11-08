"""Tests for omarchy_hook.py: python3 -B host/omarchy/test_omarchy_hook.py"""
import io
import json
import os
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).parent))
import omarchy_hook as h  # noqa: E402
from test_omarchy_sessions import TREE, fake_proc  # noqa: E402


class KimiInstall(unittest.TestCase):
    CMD = 'python3 /a/omarchy_hook.py hook kimi'
    ORIGINAL = '[providers.x]\nname = "x"\n\n[thinking]\neffort = "high"\n'

    def test_block_is_appended_once_and_the_rest_is_untouched(self):
        import tomllib
        out = h.kimi_merge(self.ORIGINAL, self.CMD)
        self.assertTrue(out.startswith(self.ORIGINAL.rstrip('\n')))
        parsed = tomllib.loads(out)
        self.assertEqual(parsed['thinking'], {'effort': 'high'})
        self.assertEqual([x['event'] for x in parsed['hooks']], [e for e, _ in h.EVENTS['kimi']])
        self.assertTrue(all(x['command'] == self.CMD and x['timeout'] == 5 for x in parsed['hooks']))
        again = h.kimi_merge(out, self.CMD.replace('/a/', '/b/'))
        self.assertEqual(again.count('[[hooks]]'), len(h.EVENTS['kimi']))
        self.assertIn('/b/', again)
        self.assertNotIn('/a/', again)

    def test_foreign_hooks_stay_and_uninstall_restores_the_file(self):
        import tomllib
        mine = self.ORIGINAL + '\n[[hooks]]\nevent = "Stop"\ncommand = "other.sh"\n'
        out = h.kimi_merge(mine, self.CMD)
        hooks = tomllib.loads(out)['hooks']
        self.assertEqual(hooks[0]['command'], 'other.sh')
        self.assertEqual(len(hooks), 1 + len(h.EVENTS['kimi']))
        self.assertEqual(h.kimi_strip(out), mine)

    def test_a_config_that_cannot_join_a_hooks_table_is_refused(self):
        with self.assertRaises(Exception):
            h.kimi_merge('hooks = []\n', self.CMD)

    def test_kimi_install_status_and_uninstall_on_disk(self):
        with tempfile.TemporaryDirectory() as tmp, mock.patch.dict(os.environ, {'KIMI_CODE_HOME': tmp}):
            path = Path(tmp) / 'config.toml'
            path.write_text(self.ORIGINAL)
            h.install('kimi')
            self.assertIn('omarchy_hook.py', path.read_text())
            self.assertEqual(len(list(Path(tmp).glob('config.toml.omarchy-deck.*.bak'))), 1)
            buf = io.StringIO()
            with redirect_stdout(buf):
                h.status()
            self.assertIn('kimi: installed', buf.getvalue())
            h.uninstall('kimi')
            self.assertEqual(path.read_text(), self.ORIGINAL)

    def test_the_kimi_payload_becomes_a_message_with_the_kimi_process(self):
        proc = fake_proc({500: ('kimi', 1, ['kimi']), 501: ('sh', 500, ['sh', '-c', 'x'])})
        msg = h.build_message('kimi', {'session_id': 'session_a', 'hook_event_name': 'PermissionRequest',
                                       'cwd': '/w', 'tool_input': {'secret': 1}}, 501, proc)
        self.assertEqual(msg, {'cli': 'kimi', 'event': 'PermissionRequest', 'session_id': 'session_a',
                               'cwd': '/w', 'pid': 500})


class Install(unittest.TestCase):
    def test_install_adds_one_handler_per_event_and_keeps_foreign_hooks_in_place(self):
        mine = h.hook_command('claude')
        foreign = {'matcher': 'Bash', 'hooks': [{'type': 'command', 'command': 'other.sh'}]}
        settings = {'model': 'x', 'hooks': {'Stop': [foreign]}}
        h.merge(settings, 'claude', mine)
        self.assertEqual(settings['model'], 'x')
        self.assertEqual(settings['hooks']['Stop'][0], foreign)
        self.assertEqual(len(settings['hooks']['Stop']), 2)
        self.assertEqual(sorted(settings['hooks']), sorted(e for e, _ in h.EVENTS['claude']))

    def test_install_twice_replaces_ours_instead_of_stacking(self):
        settings = h.merge({}, 'claude', 'python3 /a/omarchy_hook.py hook claude')
        h.merge(settings, 'claude', 'python3 /b/omarchy_hook.py hook claude')
        self.assertEqual(len(settings['hooks']['Stop']), 1)
        self.assertIn('/b/', settings['hooks']['Stop'][0]['hooks'][0]['command'])

    def test_uninstall_removes_only_ours_and_empties_the_hooks_key(self):
        foreign = {'hooks': [{'type': 'command', 'command': 'other.sh'}]}
        settings = h.merge({'hooks': {'Stop': [foreign]}}, 'claude', h.hook_command('claude'))
        h.strip(settings)
        self.assertEqual(settings, {'hooks': {'Stop': [foreign]}})
        self.assertEqual(h.strip(h.merge({}, 'codex', h.hook_command('codex'))), {})

    def test_a_malformed_hooks_value_is_refused_not_overwritten(self):
        with self.assertRaises(ValueError):
            h.merge({'hooks': []}, 'claude', 'x')
        with self.assertRaises(ValueError):
            h.merge({'hooks': {'Stop': {}}}, 'claude', 'x')

    def test_files_are_backed_up_and_written_atomically(self):
        home = Path(tempfile.mkdtemp())
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop('CLAUDE_CONFIG_DIR', None)
            path = h.settings_path('claude', home)
            path.parent.mkdir(parents=True)
            path.write_text('{"model": "x"}')
            with redirect_stdout(io.StringIO()):
                h.install('claude', home)
            self.assertTrue(list(path.parent.glob('settings.json.omarchy-deck.*.bak')))
            self.assertEqual(json.loads(path.read_text())['model'], 'x')
            with redirect_stdout(io.StringIO()):
                h.uninstall('claude', home)
            self.assertEqual(json.loads(path.read_text()), {'model': 'x'})


class Hook(unittest.TestCase):
    def test_the_message_names_the_session_and_never_carries_the_prompt(self):
        proc = fake_proc(TREE)
        message = h.build_message('claude', {'session_id': 's1', 'hook_event_name': 'UserPromptSubmit',
                                             'cwd': '/p', 'prompt': 'secret text'}, 904, proc)
        self.assertEqual(message, {'cli': 'claude', 'event': 'UserPromptSubmit', 'session_id': 's1',
                                   'cwd': '/p', 'pid': 902})

    def test_a_notification_carries_its_type_and_line(self):
        proc = fake_proc(TREE)
        message = h.build_message('claude', {'session_id': 's1', 'hook_event_name': 'Notification',
                                             'notification_type': 'idle_prompt', 'message': 'waiting'}, 904, proc)
        self.assertEqual((message['ntype'], message['message']), ('idle_prompt', 'waiting'))

    def test_no_cli_ancestor_or_a_bad_payload_means_no_message(self):
        proc = fake_proc(TREE)
        self.assertIsNone(h.build_message('codex', {'session_id': 's', 'hook_event_name': 'Stop'}, 904, proc))
        self.assertIsNone(h.build_message('claude', {'hook_event_name': 'Stop'}, 904, proc))

    def test_the_hook_always_prints_empty_json_and_exits_zero(self):
        for stdin in (io.BytesIO(b'not json'), io.BytesIO(b'[]'), io.BytesIO(b'')):
            out = io.StringIO()
            with mock.patch.object(sys, 'stdin', mock.Mock(buffer=stdin)), redirect_stdout(out):
                self.assertEqual(h.run_hook('claude'), 0)
            self.assertEqual(out.getvalue().strip(), '{}')

    def test_a_missing_daemon_does_not_break_the_hook(self):
        self.assertFalse(h.send({'x': 1}, Path(tempfile.mkdtemp()) / 'nobody.sock'))


if __name__ == '__main__':
    unittest.main()
