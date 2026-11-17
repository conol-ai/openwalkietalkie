"""Tests for omarchy_dictate.py: python3 -m unittest host/omarchy/test_omarchy_dictate.py"""
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).parent))
import omarchy_dictate as d  # noqa: E402

FACES = [{'f': 1, 't': 'agents'}, {'f': 2, 't': 'omarchy'}, {'f': 3, 't': 'macropad'}]


class FakeDeckd:
    """Answers the calls Dictation makes, with the snapshot `state.get` returns."""

    def __init__(self, face=1, focus='ag_session'):
        self.face, self.focus = face, focus
        self.calls = []
        self.agents = {'ag_session': {'id': 'ag_session'}}

    def __call__(self, method, params=None):
        self.calls.append((method, params))
        if method == 'agent.register':
            self.agents['ag_dictate'] = {'id': 'ag_dictate'}
            return {'agent': 'ag_dictate'}
        if method == 'state.get':
            return {'rev': 1, 'snapshot': {'device': {'face': self.face, 'faces': FACES},
                                           'focus': {'agent': self.focus}, 'agents': self.agents}}
        if method == 'focus.report':
            self.focus = params['agent']
        return {}

    def reports(self):
        return [p['agent'] for m, p in self.calls if m == 'focus.report']


class Held(Exception):
    code = -32004


def dictation(deckd, **kw):
    return d.Dictation(deckd, host='box', **kw)


class Dictate(unittest.TestCase):
    def test_it_registers_with_its_own_key_and_takes_transcription_when_it_can(self):
        deckd = FakeDeckd()
        dictation(deckd, transcribe=True).start()
        params = next(p for m, p in deckd.calls if m == 'agent.register')
        self.assertEqual(params['key'], 'omarchy:box:dictate')
        self.assertEqual(params['name'], 'Dictate')
        self.assertEqual(params['caps'], {'answer': 'none', 'order': 'push', 'focus': False, 'transcribe': 'push'})
        deckd = FakeDeckd()
        dictation(deckd).start()
        self.assertNotIn('transcribe', next(p for m, p in deckd.calls if m == 'agent.register')['caps'])

    def test_the_omarchy_face_takes_the_channel_and_gives_it_back(self):
        deckd = FakeDeckd(face=1, focus='ag_session')
        dictate = dictation(deckd)
        dictate.start()
        self.assertEqual(deckd.reports(), [], 'nothing on the Agents face')
        deckd.face = 2
        dictate.follow_face()
        self.assertEqual(deckd.reports(), ['ag_dictate'])
        dictate.follow_face()  # the same face again: nothing more
        self.assertEqual(deckd.reports(), ['ag_dictate'])
        deckd.face = 3
        dictate.follow_face()
        self.assertEqual(deckd.reports(), ['ag_dictate', 'ag_session'], 'the old channel is back')
        self.assertTrue(all(p['source'] == 'desktop' for m, p in deckd.calls if m == 'focus.report'),
                        'desktop: nothing is raised')

    def test_a_channel_chosen_meanwhile_is_left_alone(self):
        deckd = FakeDeckd(face=2, focus='ag_session')
        dictate = dictation(deckd)
        dictate.start()  # the bridge connects with the deck already on the Omarchy face
        self.assertEqual(deckd.reports(), ['ag_dictate'])
        deckd.focus = 'ag_other'
        deckd.face = 1
        dictate.follow_face()
        self.assertEqual(deckd.reports(), ['ag_dictate'], 'someone else took the focus: kept')

    def test_a_channel_that_went_away_is_not_given_back(self):
        deckd = FakeDeckd(face=1, focus='ag_session')
        dictate = dictation(deckd)
        dictate.start()
        deckd.face = 2
        dictate.follow_face()
        del deckd.agents['ag_session']
        deckd.face = 1
        dictate.follow_face()
        self.assertEqual(deckd.reports(), ['ag_dictate', None])

    def test_an_order_to_dictate_is_typed_at_the_cursor_without_enter(self):
        runs = []
        deckd = FakeDeckd()
        dictate = dictation(deckd, run=lambda cmd, **kw: runs.append(cmd) or SimpleNamespace(returncode=0))
        dictate.start()
        self.assertTrue(dictate.on_order({'agent': 'ag_dictate', 'text': ' hello,\n  world ', 'order': 'or_1'}))
        self.assertEqual(runs, [['wtype', '--', 'hello, world']])
        self.assertFalse(dictate.on_order({'agent': 'ag_session', 'text': 'not mine'}), 'a session order')
        self.assertEqual(len(runs), 1)

    def test_a_failed_type_is_reported_as_not_delivered(self):
        deckd = FakeDeckd()
        dictate = dictation(deckd, run=lambda cmd, **kw: SimpleNamespace(returncode=1))
        dictate.start()
        dictate.on_order({'agent': 'ag_dictate', 'text': 'hello', 'order': 'or_2'})
        self.assertIn(('agent.order.result', {'agent': 'ag_dictate', 'order': 'or_2', 'ok': False,
                                              'reason': 'wtype_failed'}), deckd.calls)

    def test_a_key_another_connection_still_holds_is_claimed(self):
        calls = []
        held = [True]

        def deckd(method, params=None):
            calls.append(method)
            if method == 'agent.register' and held[0]:
                held[0] = False
                raise Held()
            if method == 'agent.register':
                return {'agent': 'ag_dictate'}
            if method == 'state.get':
                return {'snapshot': {'device': {'face': 1, 'faces': FACES}, 'focus': {}, 'agents': {}}}
            return {}
        dictate = dictation(deckd)
        dictate.start()
        self.assertEqual(calls[:3], ['agent.register', 'agent.claim', 'agent.register'])
        self.assertEqual(dictate.agent, 'ag_dictate')

    def test_the_lease_is_kept_alive(self):
        deckd = FakeDeckd()
        dictate = dictation(deckd)
        dictate.start()
        dictate.tick(100.0)
        dictate.tick(105.0)
        dictate.tick(111.0)
        self.assertEqual([p for m, p in deckd.calls if m == 'agent.heartbeat'],
                         [{'agents': ['ag_dictate']}, {'agents': ['ag_dictate']}])
