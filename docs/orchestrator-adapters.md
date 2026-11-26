# Orchestrator adapters

The deck's grammar is fixed by `agent-command-deck.md`: **the screen is the
status board, PTT is the radio, the buttons are the decisions, and the device
shows who needs you and never the work itself.** Conol is one implementation of
that grammar (§4.6, `conol-integration.md`). This branch adds the same grammar
for *fleet* tools — programs whose job is to run many agents at once and whose
users therefore have exactly the problem the deck solves: somebody is blocked,
and nobody knows who.

The adapters do **not** render diffs, threads, boards or terminals. They map an
orchestrator's fleet state onto slots and cards, and map one dictated order back
onto that orchestrator's own "send a message to this agent" verb.

## Shape

```
deckd orchestrators list                       # which adapters are configured
deckd orchestrators <name> status [--json]     # fleet state, no daemon needed
deckd orchestrators <name> sync [--once]       # register + deliver cards, poll orders
```

`status` is pure: it shells out to the orchestrator's CLI, parses, prints. It
must work with no daemon, no deck and no network beyond the local machine, so
the mapping logic is testable in CI without either product installed.

`sync` is the only part that talks to the daemon: it calls `deck-client`, the
same door `ctl` uses (`Client::uds(...)`, `Request::AgentRegister`, `Deliver`,
`AgentWait`, `AgentOrderResult`).

## Code layout

| File | Owner |
|---|---|
| `host/crates/deckd/src/orchestrators/mod.rs` | base branch: trait, task model, card mapping, registry |
| `host/crates/deckd/src/orchestrators/<name>.rs` | **that product's branch only** |
| `host/crates/deckd/src/cli.rs` (`Cmd::Orchestrators`) | base branch |
| `host/crates/deckd/tests/orchestrators/<name>.json` | that product's branch only |

A product branch may not edit `mod.rs`, `cli.rs`, `Cargo.toml`, or another
product's file. If the trait is wrong, say so in the PR description instead of
widening it — five branches editing one trait is five conflicts.

## Task model

```rust
pub struct Task {
    pub key: String,          // stable within the orchestrator; the address for an order
    pub title: String,        // ≤ 48 chars, already redacted, no file paths unless asked for
    pub state: TaskState,
    pub activity: Option<String>, // ≤ 40 chars, what it is doing now, never a command line
}

pub enum TaskState { Working, Blocked(Blocked), Done, Idle }
pub enum Blocked { Approval, Question, Failed }
```

One task per unit the operator can act on, never per token or per tool call.
`key` is whatever the orchestrator uses to address a message: a thread id, a
pane id, an issue key, a worker id.

## Card mapping (shared, in `mod.rs`)

| State | Card | Notes |
|---|---|---|
| `Blocked(Approval)` | `question`, choices from the orchestrator | ENTER accepts, CANCEL declines; the deck's own grammar |
| `Blocked(Question)` | `question`, free text | PTT answers it |
| `Blocked(Failed)` | `failure` | must survive `agent.card.clear` (§4.7 of the design) |
| `Working` | `info`, no sound | roster row only, activity label |
| `Done` | `info`, `done` sound once | one card, cleared when acknowledged |
| `Idle` | nothing | absence is the signal |

Hard rules:

- **At most eight cards per orchestrator**, and **one orchestrator may not take
  every slot**: when more than `slots / configured` tasks are blocked, the card
  becomes a summary ("3 waiting: herdr, orca") and PTT opens the list. A fleet
  tool with 40 blocked tasks must not turn the deck into a wall.
- **Content discipline (§11).** A title is the most sensitive string in the
  daemon. No command lines, no diffs, no paths outside the repo, no tokens. When
  in doubt, the title is the task's own name and the state.
- Every adapter gets the same truncation helper; do not write local `chars().take(n)`.

## Order path

PTT speech arrives as an `agent.order` on the registered agent. The adapter
turns `(task.key, text)` into the orchestrator's own verb:

- argv only, never a shell string (`Command::new(...)`), so a dictated message
  cannot inject a command;
- the text goes in as one argument, after the verb, with the payload as JSON on
  stdin where the CLI supports it;
- the result is always reported back with `agent.order.result`, `ok: false` and
  a short reason when the call failed — an order with no trace is worse than a
  refused order (`conol-integration.md` is the precedent).

## Per-product notes

Each is what the adapter may rely on; anything else is out of scope for v1.

### herdr (branch `feat/orchestrator-herdr`)

- `herdr agent list --json` → panes with `agent`, `state` ∈
  `idle|working|blocked|done|unknown`, plus `workspace`, `title`.
- Prompt one agent: `herdr agent prompt <name> <text> --wait --timeout <ms>`.
- Keeps its own protocol versioned (`pane report-agent --seq`); do **not** scrape
  the screen — `state` from the CLI only, and `unknown` maps to `Idle`.
- `blocked` is *not* proof the operator may approve anything: the card says why
  ("waiting for approval"), never a proposed command.

### orca (branch `feat/orchestrator-orca`)

- `orca worker list --json`, `orca task-list --json`, `orca dispatch-show --json`
  (228 commands at v1.4.180, all `--json`).
- The action queue is **"Needs you"**, one entry per worker: that is a
  `Blocked(Approval)` card, not an `Idle` worker.
- Send: `orca worker-send --worker <id> <text>` (verify the exact verb against
  `orca agent-context --json` on the machine; if it moved, fail loudly).
- Known trap: some `--json` responses print a banner line before the JSON —
  parse from the first `{`, and test that case.

### multica (branch `feat/orchestrator-multica`)

- `multica issue list --output json`, `multica run list --output json`.
- The unit is the **issue**: a task in `review` is `Blocked(Approval)`, a task
  whose agent is running is `Working`, `blocked` label is `Blocked(Failed)`.
- Send: `multica issue comment --issue <KEY> --body <text>` (a review comment is
  the natural utterance) — confirm against `multica --help` before shipping.
- Its CLI has stable exit codes (3 auth, 4 not-found, 5 validation): map them to
  the order result's `reason`, do not retry.

### t3 (branch `feat/orchestrator-t3`)

- `t3 thread list --json`, `t3 thread show --json`.
- **Do not trust its completion state.** It reports Done at every intermediate
  stop of a multi-stage job and when a child finishes (upstream #25585, #31087).
  Rules: `hasPendingChildRuns` or an open child → `Working`; a thread with a
  pending approval → `Blocked(Approval)`; only a thread with no children and no
  pending approval is `Done`.
- Send: `t3 thread send --thread <id> --text <text> --json`.

### cindy (branch `feat/orchestrator-cindy`)

- Local Agent API v1 on `127.0.0.1` only: discovery at
  `<profile>/local-agent/discovery.json` (`{protocol, port, instance}`), then
  `POST /rpc` with `{"version":1,"instance":…,"id":"…","method":…,"params":{…}}`.
  It is **not** JSON-RPC and it rejects any request carrying an `Origin` header.
- A worker is a full session; `orca_workers`/`session.parent_session_id` shape
  the tree. Its native permission prompts are not forwarded by the API, so a
  worker needing approval surfaces as `RESUME_UNAVAILABLE`-class failures, not as
  a question: map those to `Blocked(Failed)` and keep the raw reason.
- Send: `send` with the session id; the client token comes from `connect/start`
  + user approval in Cindy's UI, so the adapter must fail with "not paired"
  instead of trying to pair.

## Definition of done (every branch)

1. `cargo test -p deckd orchestrators` passes, and its tests cover: a recorded
   fixture from the product's own docs, the empty fleet, a 40-task fleet, a
   malformed payload, and the banner-before-JSON case where relevant.
2. `cargo clippy -p deckd -- -D warnings` is clean for the touched files.
3. `deckd orchestrators <name> status --json` runs against the fixture via
   `--from-file <path>` (so a tester with no Herdr installed can still see it).
4. No new dependencies. No `.unwrap()` on parsed input. No secrets, paths or
   command lines in cards or in `--json` output.
5. The PR description says which real command was verified on a real
   installation, and which was inferred from docs.
