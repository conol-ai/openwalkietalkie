"""Omarchy's own dictation (Superwhisper) as the recogniser for presses to Omarchy sessions.

deckd offers a press addressed to an agent registered with `caps.transcribe: "push"` to the
connection that owns it (`agent.transcribe`, docs: "Transcribe with your own speech model"):
the bridge fetches the WAV (`agent.transcribe.audio`), hands it to `superwhisper transcribe`
and answers `agent.transcribe.result` with the text. deckd's own recogniser stays the
fallback: an error, an empty text, or no answer within its 20 s budget. Superwhisper decides
how the audio is recognised (its processing mode: offline, offline lite, or cloud), with its
own language, vocabulary and replacements, as for every other dictation on the machine.

`superwhisper transcribe` returns at once with a session id; the text is the service's `last`
dictation once its `session_id` is that one. Nothing is pasted (no `--insert`).
"""
from __future__ import annotations

import base64
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import tempfile
import threading
import time

# Under deckd's own 20 s, so an answer of ours is never thrown away as late.
TRANSCRIBE_TIMEOUT_S = 15.0
POLL_S = 0.15
# deckd's recordings are about a minute at most: 16 kHz, 16-bit mono.
MAX_WAV = 4 * 1024 * 1024


def superwhisper() -> str | None:
    return shutil.which('superwhisper')


def fetch_wav(call, transcription: str) -> bytes:
    """The whole recording, page by page (`agent.transcribe.audio`)."""
    data = bytearray()
    while True:
        page = call('agent.transcribe.audio', {'transcription': transcription, 'offset': len(data)})
        chunk = base64.b64decode(page.get('data') or '')
        total = int(page.get('total') or 0)
        if not chunk or total > MAX_WAV:
            break
        data += chunk
        if len(data) >= total:
            break
    return bytes(data)


def run_superwhisper(wav: bytes, binary: str, run=subprocess.run, sleep=time.sleep,
                     clock=time.monotonic) -> str:
    """The text Superwhisper makes of `wav`; raises RuntimeError with a short reason."""
    runtime = os.environ.get('XDG_RUNTIME_DIR')
    fd, path = tempfile.mkstemp(prefix='deck-press-', suffix='.wav', dir=runtime if runtime else None)
    try:
        with os.fdopen(fd, 'wb') as f:
            f.write(wav)
        started = run([binary, 'transcribe', path], capture_output=True, text=True, timeout=5)
        try:
            reply = json.loads(started.stdout or '{}')
        except ValueError:
            reply = {}
        session = reply.get('session_id')
        if started.returncode != 0 or not reply.get('ok') or not isinstance(session, int):
            raise RuntimeError('superwhisper_refused')
        deadline = clock() + TRANSCRIBE_TIMEOUT_S
        while clock() < deadline:
            sleep(POLL_S)
            status = run([binary, 'status'], capture_output=True, text=True, timeout=5)
            try:
                last = (json.loads(status.stdout or '{}').get('last') or {})
            except ValueError:
                continue
            if last.get('session_id') == session:
                text = str(last.get('text') or '').strip()
                if not text:
                    raise RuntimeError('superwhisper_empty')
                return text
        raise RuntimeError('superwhisper_timeout')
    except (OSError, subprocess.SubprocessError) as error:
        raise RuntimeError('superwhisper_failed') from error
    finally:
        Path(path).unlink(missing_ok=True)


class Transcriber:
    """Superwhisper on worker threads; `results` is drained by the bridge's own loop, which
    alone talks to deckd."""

    def __init__(self, binary: str | None = None, worker=run_superwhisper):
        self.binary = binary if binary is not None else superwhisper()
        self.worker = worker
        self.results: queue.Queue = queue.Queue()

    @property
    def available(self) -> bool:
        return bool(self.binary)

    def submit(self, transcription: str, wav: bytes) -> None:
        def work():
            try:
                self.results.put((transcription, self.worker(wav, self.binary), None))
            except RuntimeError as error:
                self.results.put((transcription, None, str(error)))
        threading.Thread(target=work, name='superwhisper', daemon=True).start()

    def answers(self):
        """`agent.transcribe.result` params for every transcription finished so far."""
        while True:
            try:
                transcription, text, error = self.results.get_nowait()
            except queue.Empty:
                return
            yield ({'transcription': transcription, 'text': text} if text is not None
                   else {'transcription': transcription, 'error': error})
