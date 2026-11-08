"""Kimi Code session watcher, against a fake ~/.kimi-code and a fake /proc."""
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import omarchy_kimi as k  # noqa: E402


def wire(*types):
    return [json.dumps({'type': t}) for t in types]


class StateFromWire(unittest.TestCase):
    def test_states(self):
        self.assertEqual(k.state_from_wire(wire('metadata')), 'idle')
        self.assertEqual(k.state_from_wire(wire('turn.prompt', 'llm.request')), 'working')
        self.assertEqual(k.state_from_wire(wire('turn.prompt', 'turn.ended')), 'ready')
        self.assertEqual(k.state_from_wire(wire('turn.prompt', 'interaction.request')), 'question')
        self.assertEqual(k.state_from_wire(wire('turn.prompt', 'interaction.request',
                                                'interaction.resolved')), 'working')
        self.assertEqual(k.state_from_wire(wire('turn.prompt', 'turn.cancel')), 'ready')

    def test_garbage_lines_are_ignored(self):
        lines = ['not json', '[]', '', json.dumps({'type': 'turn.prompt'})]
        self.assertEqual(k.state_from_wire(lines), 'working')


class LastReply(unittest.TestCase):
    def line(self, kind, text=None):
        event = {'type': kind}
        if text is not None:
            event['message'] = {'message': {'role': 'assistant', 'content': [
                {'type': 'think', 'think': 'secret'}, {'type': 'text', 'text': text}]}}
        return json.dumps(event)

    def wire(self, *lines):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        path = Path(tmp.name) / 'wire.jsonl'
        path.write_text('\n'.join(lines) + '\n')
        return path

    def test_the_closing_text_of_the_turn_without_thoughts(self):
        w = self.wire(self.line('turn.prompt'), self.line('agent.message.appended', 'first'),
                      self.line('agent.message.appended', 'Done:\n  all good'), self.line('turn.ended'))
        self.assertEqual(k.last_reply(w), 'Done: all good')

    def test_a_turn_without_text_does_not_repeat_the_one_before(self):
        w = self.wire(self.line('agent.message.appended', 'old'), self.line('turn.prompt'), self.line('turn.ended'))
        self.assertEqual(k.last_reply(w), '')
        self.assertEqual(k.last_reply(None), '')


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name) / 'kimi'
        self.proc = Path(self.tmp.name) / 'proc'
        self.proc.mkdir()
        self.cwd = Path(self.tmp.name) / 'work'
        self.cwd.mkdir()
        self.watcher = k.KimiWatcher(str(self.home), str(self.proc))

    def session(self, sid, *types, archived=False, cwd=None):
        d = self.home / 'sessions' / 'wd_x' / sid
        (d / 'agents' / 'main').mkdir(parents=True, exist_ok=True)
        (d / 'state.json').write_text(json.dumps({'id': sid, 'cwd': str(cwd or self.cwd), 'archived': archived}))
        w = d / 'agents' / 'main' / 'wire.jsonl'
        w.write_text('\n'.join(wire(*types)) + '\n')
        return w

    def process(self, pid, argv=('kimi',), cwd=None):
        d = self.proc / str(pid)
        d.mkdir(exist_ok=True)
        (d / 'cmdline').write_bytes(b'\0'.join(a.encode() for a in argv) + b'\0')
        link = d / 'cwd'
        if link.is_symlink():
            link.unlink()
        link.symlink_to(cwd or self.cwd)


class Watch(Fixture):
    def test_new_session_then_work_then_question_then_end(self):
        w = self.session('session_a', 'metadata')
        self.process(101)
        first = self.watcher.poll()
        self.assertEqual([(m['event'], m['cli'], m['pid'], m['session_id']) for m in first],
                         [('SessionStart', 'kimi', 101, 'session_a')])
        self.assertEqual(first[0]['cwd'], str(self.cwd))
        self.assertEqual(self.watcher.poll(), [])  # nothing changed
        w.write_text('\n'.join(wire('metadata', 'turn.prompt')) + '\n')
        self.assertEqual([m['event'] for m in self.watcher.poll()], ['UserPromptSubmit'])
        w.write_text('\n'.join(wire('turn.prompt', 'interaction.request')) + '\n')
        (ask,) = self.watcher.poll()
        self.assertEqual(ask['event'], 'PermissionRequest')
        self.assertEqual(ask['ntype'], 'permission_prompt')
        self.assertEqual(ask['message'], k.QUESTION_TITLE)  # a fixed line, never the tool text
        w.write_text('\n'.join(wire('turn.prompt', 'turn.ended')) + '\n')
        self.assertEqual([m['event'] for m in self.watcher.poll()], ['Stop'])
        (self.proc / '101' / 'cmdline').unlink()
        self.assertEqual([m['event'] for m in self.watcher.poll()], ['SessionEnd'])

    def test_two_processes_in_one_project_get_two_sessions(self):
        a = self.session('session_a', 'turn.ended')
        self.process(101)
        self.watcher.poll()
        os.utime(a, (1, 1))
        self.session('session_b', 'turn.prompt')
        self.process(102)
        (m,) = self.watcher.poll()
        self.assertEqual((m['session_id'], m['pid']), ('session_b', 102))

    def test_archived_other_project_and_no_session_are_skipped(self):
        self.session('session_old', 'turn.ended', archived=True)
        self.session('session_elsewhere', 'turn.ended', cwd=self.home)
        self.process(101)
        self.assertEqual(self.watcher.poll(), [])
        self.session('session_new', 'turn.ended')
        self.assertEqual([m['session_id'] for m in self.watcher.poll()], ['session_new'])

    def test_only_kimi_programs_count(self):
        self.process(101, argv=('python3', 'owt.py'))
        self.process(102, argv=('kimi', 'web'))
        self.process(103, argv=('node', '/x/bin/kimi-code'))
        self.assertEqual(k.kimi_pids(str(self.proc), me=0), [103])

    def test_a_long_log_is_read_from_its_tail(self):
        w = self.session('session_a', 'turn.prompt')
        with open(w, 'a') as f:
            f.write('{"type":"llm.request","pad":"' + 'x' * (k.TAIL_BYTES * 2) + '"}\n')
            f.write(json.dumps({'type': 'turn.ended'}) + '\n')
        self.process(101)
        (m,) = self.watcher.poll()
        self.assertEqual(m['event'], 'Stop')


if __name__ == '__main__':
    unittest.main()
