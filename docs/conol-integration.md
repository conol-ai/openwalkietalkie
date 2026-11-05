# Conol companion

Run Conol desktop and OpenWalkieTalkie on the same computer. Conol discovers
`deckd.sock` automatically (including `DECKD_DATA_DIR`), registers its signed-in
accounts' chats with `kind: "conol"`, and keeps them leased while the account
window is alive. No CLI hook, API key, or terminal injection is involved.

The desktop roster and device group native chats under **Conol**, with the
robot mosaic. Terminal projects retain their directory grouping. The bridge
publishes a **+ New chat** launcher and up to seven chats per account, prioritizing
the selected chat, questions, running chats, and pinned chats. The launcher also
exists for empty accounts. The daemon still owns its shared eight-slot policy.

Select a chat to read its recent public messages. PTT sends directly to that
chat's authenticated messages endpoint. **+ New chat** accepts a first dictated
message, creates the chat and switches to it. Opening the launcher shows a blank
Conol composer. An overflow chat can replace an unpinned busy Conol chat owned
by the same account and connection; the launcher and other accounts are preserved.

AskUser forms and workflow approvals run one field at a time: paged single-choice
options, multi-select toggles, Other answers and dictated text, followed by a
review screen with edit, submit and cancel. Every step gets a fresh card id. Only
submission sends the complete answer envelope to the exact AskUser call; drafts
stay local and survive socket reconnect. Unsupported/oversized forms open in
Conol. Failed submissions retain answers and explicit retries reuse their message
id. New-chat creation never automatically retries an uncertain result.
Account windows share registrations; navigation, sign-out and expiry release
chats. Socket loss drops ownership and Conol reconnects in five seconds.

The native interface is public in OpenWalkieTalkie 0.11.0. Conol registers its
optional branded kind, then uses `agent.companion.set` with app/account identity
and launcher/session role. Actual model and effort display uses
`agent.session.set` with readonly controls. There are no `conol_settings`
metadata fields. Other agents use the same methods with their own app identity;
see the [public native companion guide](../site/public/docs/native-companions/index.html)
and [runnable example](../host/examples/native_companion.py).

PTT speech addressed to a Conol chat or the launcher is transcribed by Conol's
cloud speech model, the same one behind its web composer's microphone. The
bridge registers `caps.transcribe: "push"`. On release, deckd offers the press
as 16 kHz WAV (`agent.transcribe`). The desktop pages it out
(`agent.transcribe.audio`) and hands it to the account's page, which posts it
to `/api/transcribe` with the window's own session. The text comes back through
`agent.transcribe.result`. The local recogniser (Apple or Whisper) still runs
during the press and supplies the final transcript if Conol fails, is
unsupported by an older page, or takes more than 20 s. Its words are not shown
on the deck, because they are not what Conol hears. The press shows a RECORDING
pill and a waveform that follows the deck microphone's level. deckd sends
transcript `rec:1` and no partials; the transcript appears after release. Older
deckd builds ignore the cap. `[voice.stt] companion = false` keeps all
recognition on the Mac.

Both desktop apps and firmware must include 0.11.0 native companion support.
Conol's normal browser version does not probe local ports. Transcript refresh
is two seconds for the selected chat and pending questions; roster refresh is
five seconds. Only public user/assistant text is mirrored, up to sixteen recent
messages, eight thousand characters each. The companion also sends the current
public activity label and preview, using Conol's timeline formatter (for example,
“Running a command” with a short command preview). This appears in the roster
while running and as an activity row in the conversation/workflow. Stable call
IDs update in place; completion, cancellation and replacement end the running
indicator. Activity is never spoken. Full tool inputs, output, images and private
reasoning stay in Conol. This requires updated Conol web/desktop components and
deckd with `message.summary` support; the existing 0.11.0 firmware can render it.
