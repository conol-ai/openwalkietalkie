"""Tests for omarchy_transcribe.py: python3 -m unittest host/omarchy/test_omarchy_transcribe.py"""
import base64
import json
import sys
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

sys.path.insert(0, str(Path(__file__).parent))
import omarchy_deck as o  # noqa: E402
import omarchy_transcribe as t  # noqa: E402
from omarchy_sessions import Bridge  # noqa: E402


def fake_superwhisper(last_after=2, text='Run the tests.', start=None):
    """`run` for superwhisper: `transcribe` starts session 7, and `status` reports it as the
    last dictation from its `last_after`-th poll on."""
    calls = []

    def run(cmd, **kw):
        calls.append(cmd)
        if cmd[1] == 'transcribe':
            assert Path(cmd[2]).read_bytes() == b'RIFFwav'
            return SimpleNamespace(returncode=0, stdout=json.dumps(start or {'ok': True, 'session_id': 7}))
        polls = sum(1 for c in calls if c[1] == 'status')
        last = {'session_id': 7, 'text': text} if polls >= last_after else {'session_id': 6, 'text': 'old'}
        return SimpleNamespace(returncode=0, stdout=json.dumps({'phase': 'idle', 'last': last}))
    return run, calls


class Superwhisper(unittest.TestCase):
    def test_the_text_is_the_last_dictation_once_it_is_ours_and_the_file_goes(self):
        run, calls = fake_superwhisper()
        text = t.run_superwhisper(b'RIFFwav', 'superwhisper', run=run, sleep=lambda _: None)
        self.assertEqual(text, 'Run the tests.')
        self.assertFalse(Path(calls[0][2]).exists(), 'the recording is deleted')
        self.assertNotIn('--insert', calls[0], 'nothing is pasted')

    def test_a_refusal_an_empty_text_or_no_result_is_an_error(self):
        for run, why in ((fake_superwhisper(start={'ok': False})[0], 'superwhisper_refused'),
                         (fake_superwhisper(text='  ')[0], 'superwhisper_empty'),
                         (fake_superwhisper(last_after=10**9)[0], 'superwhisper_timeout')):
            ticks = iter(range(0, 10**6))
            with self.assertRaises(RuntimeError) as caught:
                t.run_superwhisper(b'RIFFwav', 'superwhisper', run=run, sleep=lambda _: None,
                                   clock=lambda: next(ticks))
            self.assertEqual(str(caught.exception), why)

    def test_the_recording_is_fetched_page_by_page(self):
        wav = bytes(range(256)) * 10
        def call(method, params):
            self.assertEqual(method, 'agent.transcribe.audio')
            at = params['offset']
            return {'offset': at, 'total': len(wav), 'data': base64.b64encode(wav[at:at + 1000]).decode()}
        self.assertEqual(t.fetch_wav(call, 'tr_1'), wav)


class Offers(unittest.TestCase):
    """The bridge's side: fetch on its own connection, transcribe off it, answer from its loop."""

    def remote(self, worker):
        remote = o.Remote()
        remote.transcriber = t.Transcriber(binary='superwhisper', worker=worker)
        wav = b'RIFFwav'
        calls = []
        def call(method, params=None):
            calls.append((method, params))
            if method == 'agent.transcribe.audio':
                return {'offset': 0, 'total': len(wav), 'data': base64.b64encode(wav).decode()}
            return {}
        remote.rpc = mock.Mock(call=mock.Mock(side_effect=call))
        return remote, calls

    def drain(self, remote, want=1):
        deadline = time.monotonic() + 5
        answered = []
        while len(answered) < want and time.monotonic() < deadline:
            answered += list(remote.transcriber.answers())
            time.sleep(0.01)
        for params in answered:
            remote.answer_transcription(params)

    def test_an_offer_is_answered_with_superwhispers_text(self):
        remote, calls = self.remote(lambda wav, binary: 'hello deck')
        remote.handle({'type': 'agent.transcribe', 'data': {'agent': 'ag_1', 'transcription': 'tr_3'}})
        self.drain(remote)
        self.assertIn(('agent.transcribe.result', {'transcription': 'tr_3', 'text': 'hello deck'}), calls)

    def test_a_failure_is_answered_at_once_as_an_error(self):
        def fails(wav, binary):
            raise RuntimeError('superwhisper_timeout')
        remote, calls = self.remote(fails)
        with mock.patch('sys.stderr'):
            remote.handle({'type': 'agent.transcribe', 'data': {'agent': 'ag_1', 'transcription': 'tr_4'}})
            self.drain(remote)
        self.assertIn(('agent.transcribe.result', {'transcription': 'tr_4', 'error': 'superwhisper_timeout'}),
                      calls)

    def test_without_superwhisper_nothing_is_offered_to_us(self):
        bridge = Bridge(lambda m, p=None: {'agent': 'ag_1'}, None, host='box')
        self.assertFalse(bridge.transcribe)
        calls = []
        bridge.call = lambda m, p=None: calls.append((m, p)) or {'agent': 'ag_1'}
        bridge.transcribe = True
        from omarchy_sessions import Session
        bridge.register(Session('k', 'claude', 's', '/p', 2, 'p'))
        self.assertEqual(calls[0][1]['caps']['transcribe'], 'push')
