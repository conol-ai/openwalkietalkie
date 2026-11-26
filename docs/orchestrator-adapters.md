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

**Verified against herdr 0.9.5 (protocol 22)** on Linux x86_64, installed with
the documented `curl -fsSL https://herdr.dev/install.sh | sh`. The spec is
`herdr api schema --json` (bundled, works with no server); the fixture
`tests/orchestrators/herdr.json` is a real `agent list` captured from an
isolated `herdr --session` server (throwaway `HOME`/`XDG_CONFIG_HOME`) whose
shell panes self-reported their state with `herdr pane report-agent` and were
named with `herdr agent rename`. `herdr-empty.json` is the same server before
any workspace existed.

- `herdr agent list` → `{"id":…,"result":{"type":"agent_list","agents":[…]}}`.
  JSON is the only output; there is no `--json` (herdr exits 2 on it). With no
  server it prints `{"id":…,"error":{"code":"server_not_running",…}}` and
  exits 1.
- Per agent (`AgentInfo`): `name` (the live name, `[a-z][a-z0-9_-]{0,31}`;
  **absent** until someone names the agent — the schema says nullable, 0.9.5
  omits the key), `pane_id` (`w1:p1`), `agent_status` ∈
  `idle|working|blocked|done|unknown`, `agent` (the **kind**: `claude`,
  `codex`), `workspace_id` (`w1`), plus `cwd`, `title`, `terminal_title`,
  `terminal_id`, `tab_id`, `state_labels`, `tokens`, `completion_seq`.
- The task key and the prompt target are `name`, else `pane_id`. **Never
  `agent`**: herdr refuses a bare kind as a target, and many panes share one.
- `done` is herdr's "idle and not yet seen" (a `working → idle` transition sets
  `completion_seq`); `unknown` maps to `Idle`, never `Done`.
- `workspace_id` is an id, not a name, so titles carry no workspace. A real
  label exists only in `herdr workspace list` (`label`, defaulting to the cwd's
  basename, e.g. `~`); joining it would cost a second call per poll.
- Prompt one agent: `herdr agent prompt <target> <text> --wait --timeout <ms>`
  (also `--until <status>`, repeatable). A prompt to an already `blocked` agent
  is refused by herdr with `agent_blocked` before any input is sent.
- `pane report-agent` is herdr's own reporting protocol for agents; the adapter
  never calls it. Do **not** scrape the screen — status from the CLI only.
- `blocked` means herdr "recognized an approval or question UI"; `AgentInfo`
  does not say which, nor what is asked (`--message` given to `report-agent`
  does not appear in `agent list`). The card says "waiting for approval" and
  never a proposed command.

**The first version was wrong in five places.** It was written from the docs'
prose before a binary was available, and every one of these was invisible to
its own tests because the fixture was written to match the code:

| # | First version | herdr 0.9.5 | Now |
| --- | --- | --- | --- |
| 1 | `agent list --json` | no `--json`; exit 2 | `agent list` — verified |
| 2 | top-level array or `{"panes":[…]}` | `result.agents` | verified (captured) |
| 3 | key and prompt target from `agent` | `agent` is the kind; target is `name` or `pane_id` | verified (captured; `name` absent when unnamed) |
| 4 | status from `state` | `agent_status` | verified, all five words captured |
| 5 | `workspace` as a plain-word name | `workspace_id`, an id | suffix dropped — verified the field; the label join is not done |

Still inferred: that a *real* agent (not a self-reported shell pane) lists the
same way, and how herdr answers `agent prompt` end to end — no prompt was sent
to anything.

### orca (branch `feat/orchestrator-orca`)

**Doc-derived, not verified against a running instance.** Rewritten
2026-11-26. The product is Orca by Stably AI (`stablyai/orca`); sources are
[orca.dev/docs/cli/reference](https://orca.dev/docs/cli/reference),
[orca.dev/docs/cli/orchestration](https://orca.dev/docs/cli/orchestration), and,
because neither page shows any output, the CLI source at `83014a0b56e1`
(`task-agent-views.ts`, `task-handlers.ts`, `agent-view.ts`). Every encoded
name is quoted in `orca.rs`'s header.

- Needs a running runtime: "The orca CLI talks to a running Orca runtime"
  (desktop app or `orca serve`), and orchestration is "Experimental" ("Enable
  orchestration under Settings → Experimental").
- Status: `orca orchestration task-list --json` → `{"ok":…,"result":{"tasks":[
  {id, status, title, deps, parentId, dispatchId, assignee}], "runId"}}`.
- Statuses: "pending, ready, dispatched, completed, failed, or blocked".
  `dispatched` → `Working`, `completed` → `Done`, `failed` → `Blocked(Failed)`,
  everything else → `Idle`. `blocked` is `Idle`: it is a decision gate (answered
  with `gate-resolve --id <gateId>`, not in the row) or a manual mark, and the
  deck can answer neither.
- Send: `orca orchestration send --to dispatch:<dispatchId> --subject … --body
  <text> --json`, the dispatch read fresh from `task-list`; no dispatch → the
  order is refused (`no_worker`), never sent to `assignee`.

**The first version was wrong in every verb:** `orca worker list --json`,
`worker-send --worker`, `agent-context`, a `workers`/`needs_you` envelope, the
"Needs you" approval queue, `running`/`done` state words and a banner before
the JSON — none of them appears in Orca's docs or CLI source.

### multica (branch `feat/orchestrator-multica`)

**Doc-derived, not verified against a running instance.** Rewritten
2026-11-26 from the product's own CLI spec,
[`CLI_AND_DAEMON.md`](https://github.com/multica-ai/multica/blob/main/CLI_AND_DAEMON.md)
at `0c5ee1f07d87`, read in the sections the adapter touches (install, auth,
workspaces, issues through execution history, output formats, errors). Every
encoded name is quoted in `multica.rs`'s header. Listing needs `multica login`
(cloud or self-hosted): the doc's 401 message "tells you to run `multica login`".

- List: `multica issue list --limit 100 --output json` → `{total, limit,
  offset, has_more, issues[]}`. Issue keys read: `identifier`, `id`, `title`,
  `status`. Not read: `assignee_type`/`assignee_id` (who, not which issue),
  `properties` (definition ids → option ids), `priority`.
- Statuses (built-in): `backlog|todo|in_progress|in_review|done|blocked|cancelled`,
  plus per-workspace custom ones. `in_review` → `Blocked(Approval)`,
  `in_progress` → `Working`, `done` → `Done`, everything else → `Idle`.
  `blocked` is `Idle`: the doc never says it means failed.
- Send: `multica issue comment add <issue-id> --content <text>`, addressed to
  `identifier` (the doc: "copy that key into follow-up commands"), else `id`.
- Exit codes (doc table): 2 network, 3 auth, 4 not found, 5 validation → the
  order result's reason word, no retry.

**The first version was wrong in six places:** a bare-array/`issues` envelope
with no paging, `key` instead of `identifier`, `review` instead of `in_review`,
`labels` and `run.status`/`run.stage` on the list (the doc names none of them),
`multica run list` (there is none; runs are `multica issue runs <issue-id>`),
and `issue comment --issue … --body …` instead of `issue comment add <issue-id>
--content …`.

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

## What the merge changed (integration, `feat/orchestrator`)

The five branches each touched only their own module, so they merged without a
conflict. Three things had to be settled centrally, in `mod.rs` and `cli.rs`:

- **Availability is per product, not per build.** `herdr`, `multica` and `t3`
  are always registered: an absent binary is a `status` failure with a reason
  word, which is better than an adapter that silently is not there. `orca` is
  registered when `orca` is on `PATH`; `cindy` when a discovery file exists.
  `deckd orchestrators list` prints the difference (`not installed (<branch>)`).
- **`--from-file` no longer needs the product.** It used to ask the registry for
  an adapter, which is `None` exactly when the product is absent — the one case
  a capture exists for. `MODULES` now carries a second, offline constructor and
  `status --from-file` builds the adapter through it.
- **The registry test** `the_registry_lists_five_and_they_all_parse_a_capture`
  replaces the "four are stubs" assertion: it checks the three always-present
  adapters are there and that all five can parse their own fixture offline.

### Divergences found while merging (adapter ↔ contract)

| Where | What the adapter had to do | Status |
| --- | --- | --- |
| t3 | "Done" means **no pending children**, not "no children": under the contract's wording a thread that ever had a child could never be done. | `mod.rs` wording kept, adapter follows the stricter rule |
| t3 | #25585 (multi-stage stop reported as done) is only covered when the stages appear as child runs. Without a stage field there is nothing to read. | open, needs a field from t3 |
| t3 | A child's approval surfaces on the parent, but the reply carries the parent's thread id. Whether t3 forwards it is unverified. | open |
| cindy | The card cannot answer Cindy's native permission prompts (the API does not forward them): they are `Blocked(Failed)` with the raw reason word, never a fake question. | by design |
| cindy | `Config` has no `[orchestrators.cindy]`, so the token and discovery file come from `DECKD_CINDY_TOKEN`, `DECKD_CINDY_DISCOVERY`, `DECKD_CINDY_PROFILE`. | works, names are ours |
| multica | Exit codes 2/3/4/5 are mapped to `network`/`auth`/`not_found`/`validation` by reading them back out of `run()`'s error text. | works, `run()` returning a structured code would be cleaner |
| multica | The `blocked` status is `Idle`, not `Blocked(Failed)`: the doc names it but never says it means failed, and the list has no labels or runs to say more. | open: needs a product decision or a real capture |
| orca | No approval card: Orca's CLI has no "Needs you" queue, and a `blocked` task's gate id is not in `task-list`. | open: no documented way to read a gate; needs a real capture |

### State of verification

| Adapter | Tests | Verified against a real install |
| --- | --- | --- |
| herdr | 10 + captured fixture | **yes, herdr 0.9.5**: argv, envelope, fields and statuses against a live isolated server; `agent prompt` syntax from `--help` only (nothing sent) |
| orca | 8, doc/source-derived fixture | no — rewritten from orca.dev + CLI source 2026-11-26; needs the desktop runtime to verify |
| multica | 10, doc-derived fixture | no — rewritten from `CLI_AND_DAEMON.md` 2026-11-26; needs a logged-in account to verify |
| t3 | 13 | no |
| cindy | 17, incl. one round trip against a local fake server | no |

Except herdr's, every fixture was written from the product's documented output
shape, not captured from a live install — and herdr's first fixture, written the
same way, was wrong in five places (see the herdr section). Treat the other four
as unverified until each gets the same audit. What is verified is the code path: argv shape,
JSON parsing, card mapping, redaction, and — for all five — that
`deckd orchestrators <name> status --from-file` prints a plan on a machine where
the product is not installed (`1152` lib tests, `deckd orchestrators list` and
five `status --from-file` runs, all green).
