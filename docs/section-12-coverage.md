# §12 failure-mode coverage audit (week 8)

Scope: every v1 row of `docs/host-service.md` §12, lines 1181–1215 — **35 rows**. No row is
marked v1.1/v2 in its entirety; row 1197 is split and only its v1 half (banner + `lsof` listener
+ change port → `integrations.repair`) is audited here.

Method: located candidate tests across `host/crates/*/tests/`, every `#[cfg(test)]` module under
`host/crates/*/src/**` (993 `#[test]`/`#[tokio::test]` functions) and `tools/host-test.sh`; read
every cited body; then mutated production code for 18 clauses and recorded red/green. Every
mutation was reverted; `git status` is clean.

Read-only pass. Nothing in the repo was changed.

---

## Row-by-row

| # | Row (§12) | Verdict | Test(s) | Missing piece |
|---|---|---|---|---|
| 1181 | Daemon crash | **partial** | KeepAlive: `tools/host-test.sh:248` (packaging step) + `tools/assert-bundle-identity.sh:106`. New sid → resync: `host/crates/deckd/src/hid.rs:1229` `a_link_up_hellos_a_zero_sid_writes_the_barrier_and_synced_goes_live`, `host/crates/deckd/src/device/session.rs:206` `a_foreign_or_zero_sid_starts_a_sync_and_synced_returns_to_live`, `tools/host-test.sh:1487` (`--hardware` restart row). Orphan grace 300 s: `host/crates/deck-model/src/roster.rs:1164` `a_lease_expires_into_the_orphan_grace_and_then_removal`. Reseed: `host/crates/deckd/src/liveness.rs:739` `the_roster_corrects_state_and_name_and_leaves_everything_else_alone`, `:778` `seeding_is_opt_in_and_only_for_unknown_interactive_sessions` | No test kills a daemon and inspects recovery. "restart ≤10 s" unmeasured; "device shows `offline` only if the gap exceeds 15 s" has no test at all; "held hooks end with the HTTP connection" is only tested for an orderly `Shutdown` (`deck-model/src/store.rs:1698`), never for a crash |
| 1182 | Held hook's client goes away → `HoldGone` | **covered** (proved) | `host/crates/deckd/tests/hooks_door.rs:74` `a_hook_client_that_goes_away_releases_its_hold_at_once`; `host/crates/deckd/tests/holds_guard.rs:34` `hold_guard_drop_sends_hold_gone`; `host/crates/deckd/tests/adapters_claude.rs:548` `a_released_hold_degrades_to_focus_terminal_under_the_same_id`; `host/crates/deck-model/src/cards.rs:1723` `a_hold_is_written_then_cleared_and_hold_gone_degrades_at_once`; `host/crates/deckd/src/core/handle.rs:616` `a_dropped_reply_is_an_internal_error_and_the_hold_guard_reports_on_drop`; Retry refusal: `host/crates/deck-model/src/cards.rs:2155` `a_failed_hold_card_refuses_retry_and_a_timeout_card_can_be_released` | — (`hooks_door.rs:74` asserts `hold_s >= 60` up front so the pass cannot come from expiry: the best-designed test in the table) |
| 1183 | Core → hid channel overflow | **partial** (behaviour proved) | `host/crates/deckd/src/core/mod.rs:3309` `an_overflow_replays_the_removals_it_refused_before_re_projecting` (drives a real `hid::Link`, asserts replay order, the wire barrier and `status.overflows == 1`); `:3214` `delivery_effects_resolve_and_the_overflow_reprojects_the_store`; `host/crates/deck-model/src/store.rs:1307` `overflow_reprojects_and_asks_for_a_barrier` | (a) "when the channel **drains below half**" is not pinned — mutating the gate to `<= OPS_CAPACITY` leaves all 436 deckd lib tests green (see M2). (b) The row names `metrics.get.outbox_overflows`; **no such field exists** — `metrics.get` publishes `overflows` (`core/dispatch.rs:231`), and the only `metrics.get` assertion is the zero case (`core/dispatch.rs:1383`) |
| 1184 | Hid → core: slow core | **uncovered** | Only an incidental proxy: `host/crates/deckd/src/core/mod.rs:3048` `metrics_get_waits_for_a_fresh_report` happens to show a device line handled while the call is parked | Nothing asserts `run_core`'s `try_recv` drain runs **before** the `select!` (`core/mod.rs:2408`), nothing asserts the hid→core channel is unbounded/never blocks, and the "≥4 ms apart" wire limit has no test (the only 4 ms in the tree is the host→device pacer's input, which is clamped to an 8 ms floor) |
| 1185 | Device link drop / sleep / out of range | **partial** (proved in part) | `failed{link}`: `host/crates/deck-model/src/cards.rs:2624` `link_down_fails_answers_in_flight`. Abort PTT: `host/crates/deck-model/src/voice.rs:2609` `abort_up_link_drop_cap_and_codex_are_handled`. `Absent`: `host/crates/deck-model/src/store.rs:1077` `link_events_drive_the_device_state_and_rev`. Resync on return: `host/crates/deckd/src/hid.rs:1096` `a_line_cut_by_a_host_side_drop_is_resent_when_the_device_keeps_our_sid`, `:1229`. 800 ms: `host/crates/deckd/src/hid.rs:1469` tail (`sleep_for == ENUMERATE_EVERY`) | Detection column untested and **different from the doc**: the code has no hidapi removal callback and no `Ok(0)` streak — it polls `present(api)` every 2 s plus read errors (`hid.rs:886`, `:906`). "lanes held 30 s then flushed" is **not implemented** (no such timer; the only `hold_lanes` is the outbox write gate, no timer). "release keys" is not implemented (v1 synthesises none) |
| 1186 | Hello replies missing | **partial** (SM proved) | `host/crates/deckd/src/device/session.rs:239` `misses_degrade_at_three_and_reopen_at_six`; `host/crates/deckd/src/hid.rs:1411` `misses_degrade_reopen_and_a_foreign_sid_resyncs` (asserts `is_linked()` — "handle kept while degraded" — and the reopen at 6); `host/crates/deckd/src/device/session.rs:304` `await_hello_retries_every_two_seconds` | "GUI banner": `present::deck_line`'s `Link::Degraded` branch (`openwalkietalkie-app/src/present.rs:1479`) has **zero** coverage — `welcome_flow.rs:207` drives only `syncing`/`live`, and `present::degraded_banner` is the *service* banner, not the device session |
| 1187 | Firmware queue overflow | **partial** (signature proved) | `host/crates/deckd/src/hid.rs:1469` `two_drops_after_bursts_back_the_pace_off` (×2 signature, `PaceBackoff{pace_ms:10}`, `pace_backoffs`/`backoff` metrics); `error` event: `host/crates/deck-model/src/store.rs:1077`; pacer: `host/crates/deckd/src/device/outbox.rs:978` `the_pacer_spaces_reports_by_at_least_eight_ms` | "pace 10 ms for **10 min**" is not pinned: the assertion is `pace_at(t0 + BACKOFF_FOR)`, written in terms of the constant — `BACKOFF_FOR` 600 s → 60 s leaves all 436 tests green (M-BACKOFF). "sync order unchanged" untested. **`deckd device flood 2000` is not in the hardware gate** — `host-test.sh:98` runs `--flood 25` only inside the offline `deck-sim.py` parity diff |
| 1188 | `err range` after `card`/`reply` | **uncovered** | Only counters: `host/crates/deckd/src/hid.rs:1324` (`err_range == 1`), `host/crates/deck-model/src/store.rs:1241` | The promised behaviour **does not exist**: no correlation window, no "re-send `agent` then the line once", no `failed{device}` (`fail_card` is only ever called with `link`/the delivery reasons). Grep for `correlation` across `host/` returns nothing |
| 1189 | `err size/parse/unknown` | **partial** | Counters: `host/crates/deckd/src/hid.rs:1324` `device_edits_never_echo_and_the_face_gates_ws`, `host/crates/deck-model/src/store.rs:1241` `faces_face_err_and_refresh_are_mirrored`, `host/crates/deckd/src/metrics.rs:203` `the_summary_names_every_counter_and_status_round_trips` | "log the shadow line": the `tracing::warn!(recent = outbox.recent(3))` call at `hid.rs:575` is never asserted from an `err` line (`recent()` itself is asserted once, in an unrelated outbox test). "`unknown` for an enum → feature-flag that field off for this `fw`" is **not implemented** |
| 1190 | Answer delivery > 4 s or error | **partial** (budget proved) | `host/crates/deck-model/src/cards.rs:1474` `no_delivery_within_four_seconds_fails_the_card_without_a_clear`; `:1539` `a_failed_delivery_result_fails_the_card`; `host/crates/deckd/tests/core_delivery.rs:122` `forced_delivery_failure_then_late_poll_pickup` (incl. "dropped if the operator answers again first"); `host/crates/deckd/src/core/mod.rs:3759` `op_answer_answers_after_the_delivery_or_the_budget`; `tools/host-test.sh:1333` (`--hardware`) | TTS "not delivered to `<name>`" on the `Question` lane is **not implemented** (no `AudioCmd::Say` on any card-failure path). "GUI Retry (push/poll **with a live owner session**)": `present::retry_offered` (`openwalkietalkie-app/src/present.rs:419`) is `Failed && (Push|Poll)` and never consults the owner — the daemon refuses with `owner_gone` (`cards.rs:2644`) but the GUI half is untested |
| 1191 | Hook fires while the daemon is down | **partial** | `host/crates/deckd/tests/hook_helper.rs:223` `a_dead_daemon_prints_empty_and_exits_zero` (real binary, all three CLIs, exit 0 + exactly `{}\n`); `host/crates/deckd/src/hook/mod.rs:416` `a_missing_socket_still_answers_empty`; `:432`; `host/crates/deckd/tests/hook_helper.rs:253` | "Claude `http` hook non-2xx → proceeds normally" is **not testable here** without a live `claude` binary driving its own hook runtime; today it exists only as a doc comment (`tests/hooks_hostile.rs:366`). Also the helper tests use a *missing* socket, never a refused connection on an existing path |
| 1192 | Held hook exceeds the operator's patience | **covered** | `host/crates/deck-model/src/cards.rs:1857` `snooze_releases_the_hold_and_repushes_the_same_id_degraded`; `:2041` `hold_expiry_degrades_the_card_and_a_poll_snooze_keeps_it_answerable`; `:2107` `the_gui_releases_a_failed_hold_card_to_the_terminal`; `host/crates/deckd/tests/adapters_claude.rs:548` part 2 (live `hold_s=1` over the real door) | — |
| 1193 | Hook for an unknown session | **partial** | `host/crates/deckd/src/adapters/claude.rs:2021` `an_http_event_writes_no_terminal` (a bare `UserPromptSubmit` on an unseen session plans an `AgentRegister`); `host/crates/deckd/tests/adapters_claude.rs:1246` `every_fixture_replays_to_its_golden_ingest` (fresh adapter); "fills the rest": `host/crates/deckd/src/liveness.rs:739`, `:778` | No test asserts the created agent's `key`/`cwd`/`name` actually come from the payload (`session_id`, `cwd`) — the register is only asserted to exist, with the assertions aimed at `terminal`/`pid_kind` |
| 1194 | Duplicate/late hooks | **partial** | Claude 5 s window: `host/crates/deckd/src/adapters/claude.rs:1372` `identical_permission_requests_inside_the_window_raise_one_card`. Codex `turn_id`: `host/crates/deckd/src/adapters/codex.rs:722` `a_turn_is_reported_once_however_many_doors_report_it`, `:770`; end-to-end `host/crates/deckd/tests/adapters_codex.rs:393` `the_notify_payload_is_deduped_against_the_stop_hook`. `rev` unchanged: `host/crates/deck-model/src/roster.rs:1028` | The Claude dedupe key is `session_id \0 tool \0 blake3(input)` but only `tool_input` and time are varied — `session_id` and `tool_name` are never varied, so two sessions colliding on one key would not be caught. No end-to-end duplicate **Claude** post. "`-32004` logged once" — no once-only logging exists anywhere. "Integrations shows **duplicate hook config**" — that string exists only in the doc; no status word, no code path, no test |
| 1195 | Owner disappears with a card pending | **partial** (proved in part) | `host/crates/deck-model/src/cards.rs:2644` `an_orphaned_owner_downgrades_its_cards` (`push` → `focus` with a terminal, → `none` without); `host/crates/deckd/tests/core_delivery.rs:367` `a_closed_owners_poll_answer_is_claimed_by_the_next_session`; `:470` `orphaned_mail_expires_after_the_mailbox_ttl`; `host/crates/deck-model/src/roster.rs:1164` (card removed with the agent after grace) | "GUI **agent gone**" — no such label or state exists in the GUI; the only signal is a gray dot derived from the `(disconnected)` title (`present.rs:2041`). Device "not delivered" on answer is device-side |
| 1196 | Port 47201 taken, nothing answering | **covered** | `host/crates/deckd/tests/service_degrade.rs:252` `a_squatted_port_degrades_the_tcp_door_and_names_the_squatter_by_pid`; `:288` `a_degraded_daemon_keeps_the_uds_door_and_the_deck_channel_alive`; `:324` `the_service_degraded_event_carries_the_reason_the_port_and_the_listener`; `:358` `change_the_port_and_repair_the_hooks_is_the_way_out_of_a_squatted_port`; `host/crates/deckd/src/api/http.rs:2140`; helper fail-open `tests/hook_helper.rs:223` | Only the Claude-side "connection refused → Claude proceeds" is outside this machine (same clause as row 1191) |
| 1197 | Port taken by a foreign listener (**v1 half**) | **covered** | `host/crates/deckd/src/api/http.rs:2140` `a_taken_port_degrades_and_a_foreign_v1_info_is_a_hijack_risk`; `:2241` `push_caps_lsof_output_and_probe_bodies_parse`; `host/crates/openwalkietalkie-app/tests/integrations_screen.rs:125` `hooks_hijack_risk_reaches_the_integrations_screen_off_the_mirror_with_its_listener`; `host/crates/openwalkietalkie-app/src/present.rs:2211`; `host/crates/openwalkietalkie-app/tests/service_screen.rs:74`; `host/crates/deck-gui-core/tests/reducer.rs:1178` | — (`lsof` is exercised as a parser, not as a spawned process) |
| 1198 | `inject` while a dialog is open / CLI not foreground | **partial** (dialog guard proved; the foreground half retired 2026-11-13) | `host/crates/deckd/tests/desktop_guards.rs` `an_open_permission_card_refuses_the_order_before_any_job_is_built`, `every_dialog_origin_and_status_blocks_an_order_and_a_custom_card_does_not`, `a_permission_card_whose_answer_never_landed_still_blocks_an_order`; `host/crates/deck-model/src/orders.rs` `a_failed_hand_off_from_a_degraded_hold_shows_and_keeps_guard_two` | Nothing is typed into a terminal any more: an `inject` order goes by Claude Channels or the inbox, else `order.failed{no_route}` (`desktop_guards.rs` `an_inject_order_is_dispatched_then_fails_no_route_with_no_job`), so the foreground check (guard 3) and its `not_foreground` went with the Ghostty injector. "order kept on the GUI with Copy" is false for the `dialog_open` branch (`reducer.rs:853` asserts `text: None`, so there is nothing to copy). TTS "no route to `<name>`" is **not implemented**; the deck shows the failure instead |
| 1199 | App trashed before uninstall | **partial** | `host/crates/deckd/src/launchd.rs:454` `a_program_that_is_gone_is_dangling_and_names_the_app_that_carried_it`; `:501` `a_loaded_job_whose_plist_is_gone_is_dangling_too`; `:518` `a_bundle_program_resolves_against_the_app_the_plist_sits_in`; `:490`; `host/crates/deckd/tests/uninstall.rs:206` (the documented "uninstall before trashing" next-step) | Nothing connects a dangling detection to `SMAppService.unregister()`; the GUI's `Launchd` enum has no `dangling` variant, so the state cannot be shown |
| 1200 | Card id collision after restart | **covered** (proved) | `host/crates/deck-model/src/props.rs:222` `no_card_id_repeats_on_a_slot_across_restarts` (proptest rebuilding the store from the persisted counters each restart); `host/crates/deck-model/src/ids.rs:209` `card_ids_skip_zero_on_wrap`; `host/crates/deck-model/src/store.rs:1039` `a_new_store_carries_the_persisted_state_and_service_facts`; `host/crates/deck-model/src/roster.rs:1522` `release_frees_the_slot_and_the_device_forgets_it`; `host/crates/deck-model/src/cards.rs:1163` (persist before emit) | — |
| 1201 | Two sessions register the same key | **partial** | `host/crates/deck-model/src/roster.rs:1054` `the_same_key_from_another_live_session_conflicts_whoever_runs_it`; `host/crates/deckd/src/core/mod.rs:3131` `two_open_sessions_get_two_ids_and_the_same_key_conflicts_between_them`; `host/crates/deckd/tests/api_uds.rs:20` `two_uds_clients_same_uid_get_a_key_conflict`; `host/crates/deck-model/src/roster.rs:1243` `the_owning_session_revives_its_orphan_and_a_new_session_claims_it` | "GUI shows the owner" — no GUI/reducer test renders the owner of a refused registration |
| 1202 | Hook token mismatch / path drift | **covered** | `host/crates/deckd/src/integrations/claude.rs:1541` `status_walks_absent_installed_outdated_path_drift_and_token_mismatch`; `host/crates/deckd/src/integrations/codex.rs:2042` `status_reports_path_drift_when_the_binary_moved`; `host/crates/deckd/tests/integrations.rs:763` (drift decided before trust); `host/crates/deckd/src/integrations/markers.rs:254`; Repair wiring `host/crates/deck-gui-core/src/admin.rs:808` | Detection-column nit: `token_mismatch` is decided by re-verifying the embedded bearer, never by observing a live 401 on `/v1/hooks` |
| 1203 | Mic or speech denied | **partial** | `perm.changed`: `host/crates/deckd/tests/voice_ptt.rs:409` `arming_without_samples_reports_a_dead_mic` (first block) and `host/crates/deck-model/src/voice.rs:1217`. GUI sheet: `host/crates/deck-gui-core/tests/reducer.rs:1026` `a_not_determined_permission_raises_a_sheet_and_a_denied_one_opens_settings`; `host/crates/openwalkietalkie-app/tests/voice_screen.rs:191` | "PTT sends `transcript final:1` with **(microphone not allowed)**" is **not implemented**: `Store::on_voice_error` emits only `voice.state{aborted}` and writes no transcript; the literal exists only as a cpal error string in `audio/coreaudio.rs:201`. Note the *denied* GUI path is "open System Settings", not a sheet |
| 1204 | SCO never comes up | **covered** | `host/crates/deckd/tests/voice_ptt.rs:409` `arming_without_samples_reports_a_dead_mic` — asserts >200 `noErr` callbacks of digital zero, still arming at 2.4 s, then `voice.state{state:"aborted", error:"mic unavailable", ctx:"talk"}` and a `Transcript` control containing `(no mic)`; the warm re-run survives | Doc mismatch (not a test gap): the row says 1.5 s; `ARMING_LIVENESS` is 2 500 ms and the test *positively asserts* still-arming at 2.4 s |
| 1205 | A2DP does not resume (`ScoStuck`) | **partial** | `host/crates/deckd/tests/voice_route.rs:180` `two_failed_reopens_are_stuck_and_the_next_rate_change_recovers` (two roads; `open_at() == Some(16_000)`; `NominalRate{44100}` clears it); `host/crates/deck-model/src/voice.rs:1559` (wire strings, `stuck:true` is news) | GUI "audio route stuck — reconnect the deck" **does not exist** — the shipped text is different (`present.rs:1095`) and `voice_screen.rs:39` only asserts the help string is longer than 40 chars, never its content. `voice_mixer.rs:318` is named/commented for this row but never enters `ScoStuck` |
| 1206 | Codex app owns the mic | **partial** | `host/crates/deckd/tests/voice_route.rs:141` `an_external_capture_takes_the_route_and_gives_it_back` (`ScoExternal`, `CloseOutput`, **no** `OpenOutput`); (the deck's former `ctx:codex` PTT, which produced only `AudioCmd::Stop`, is gone with the Codex Micro emulation) | "hold lanes": `Engine::may_speak` refuses over `ScoOurs|ScoExternal|Reopening`, but `engine.rs:1386` drives only `ScoOurs` and `Reopening`; `RouteEvent::ExternalCapture` never reaches `Lanes` in any test |
| 1207 | Deck audio absent (HID only) | **partial** | `host/crates/deckd/tests/voice_route.rs:267` `a_fault_is_a_stream_that_has_already_stopped` (last block: `fallback_output` `none` → `Route::None`, `system_default` → `Route::SystemDefault`); config plumbing `host/crates/deckd/tests/voice_api.rs:709`; UID lookup `host/crates/deckd/src/audio/coreaudio.rs:699`, `:727`, `host/crates/deckd/tests/voice_hardware.rs:55` (`#[ignore]`) | The input side is tested since decision 11 removed `fallback_input`: `host/crates/deckd/src/audio/coreaudio.rs` `a_press_opens_the_deck_by_uid_or_nothing_and_never_the_macs_input` (a deck press with no UID is `no_device`, only `voice.listen{input:"mac"}` reaches the Mac's input), `audio/ble.rs` `the_capture_delivers_the_deck_at_16_khz_and_never_falls_back_to_the_mac`, `audio/engine.rs` `an_unset_device_uid_keeps_the_deck_found_at_start`. "deck Settings › Host still says connected" is firmware — **not testable here** |
| 1208 | Ghostty Automation denied / terminal ambiguous | **retired** (2026-11-13) | — | The Ghostty tab path is gone, and with it the Automation grant and the resolver's `ambiguous`. Its successor, a hand-off that cannot happen (no app known, not running, refused), is shown on the deck: `host/crates/deckd/src/core/mod.rs` `a_session_with_no_app_is_not_raised_and_the_deck_says_so`, `a_hand_off_that_did_not_happen_is_shown_on_the_deck` |
| 1209 | Config invalid on reload | **partial** (proved) | `host/crates/deckd/tests/config_reload.rs:73` `hot_reload_keeps_last_good_on_invalid` — real watcher, previous config kept, `config.toml.invalid` byte-identical to the rejected text, `error{code:"config_invalid"}` with `position.line`, and nothing reaches the core | "GUI banner": `host/crates/deck-gui-core/tests/reducer.rs:1237` explicitly asserts `EventKind::Error` produces **no** GUI effect. `EventKind::Error` appears nowhere else in the GUI crates |
| 1210 | `state.json` corrupt | **covered** (proved) | `host/crates/deckd/src/state.rs:193` `a_corrupt_file_is_moved_aside` (rename + `State::default()`); faces re-adopted: `host/crates/deck-model/src/store.rs:1116` `hello_adopts_faces_on_boot_zero_and_keeps_them_on_boot_one`, and from an empty store on `boot:1` too: `host/crates/deck-model/src/profile.rs` `a_deck_with_no_stored_strip_is_remembered_with_its_own_on_boot_1`, and `host/crates/deckd/src/hid.rs:1319` on the wire | The two halves are tested separately, never as one chain from a corrupt file |
| 1211 | Two daemons | **partial** (flock proved) | `host/crates/deckd/src/service.rs:319` `the_lock_is_exclusive_and_released_on_drop`; `:337` `a_fork_that_copied_the_lock_fd_cannot_keep_it_after_the_guard_drops` | "second exits 0 `already running`" — `EXIT_ALREADY_RUNNING` has **no test hit anywhere**; no test and no gate step starts a second daemon on one data dir. "`deckd status` reports the owner pid" — `cli.rs:2088` only reaches the *not running* branch (the test process holds no lock), so `status_text`'s `running (pid …)` line is never rendered |
| 1212 | Sparkle update mid-PTT | **partial** | `host/crates/deckd/tests/service_exit.rs:162` `service_restart_answers_its_caller_and_then_the_process_exits_seventy_five`; `host/crates/deckd/src/core/dispatch.rs:1847` `service_restart_and_stop_answer_before_the_supervisor_may_see_the_code`; `host/crates/deckd/src/service.rs:407` | The row's own promise — "**utterance finalised first**" — is **not implemented**: nothing ties `service.restart`/`service.stop` to a live PTT or to `Finalizing`. "waits ≤3 s" is also wrong: `Exit::GRACE` is 250 ms and is a reply-writing window, not a PTT wait |
| 1213 | GUI disconnected | **partial** | `host/crates/openwalkietalkie-app/tests/deck_screen.rs:389` `a_disconnected_window_keeps_the_last_roster_and_the_greyed_dots_say_so`; `host/crates/deck-gui-core/tests/reducer.rs:351` `a_disconnect_keeps_the_last_mirror_and_asks_for_a_reconnect`; `host/crates/deck-gui-core/tests/api_client.rs:267`; `host/crates/openwalkietalkie-app/tests/service_screen.rs:56`; `host/crates/openwalkietalkie-app/tests/tray_menu.rs:249` | The bar's **Start (SMAppService)** button is a gpui render path (`app/shell.rs:661-726`, `app.rs:191`) that no test drives — the SMAppService tests that exist are about the *login item*, a different object. The launchd-log link on the disconnected bar is likewise undriven |
| 1214 | Slot pressure (>8 agents) | **partial** (weak test proved) | `host/crates/deck-model/src/roster.rs:1460` `a_full_deck_evicts_the_longest_idle_agent_never_one_with_a_card`; `:1507` `an_unslotted_agent_that_wakes_up_takes_a_slot_from_an_idle_one`; GUI "not on deck": `host/crates/openwalkietalkie-app/src/present.rs:1762`, `:2063`, `host/crates/deck-gui-core/tests/reducer.rs:620` | **"never one with a pending card" is not exercised.** Deleting the `!a.card…on_device()` clause from `eviction_candidate` leaves all 149 deck-model tests green (M7). The test's card-holder is in `Question` state, so the earlier `state == AgentState::Idle` arm already excludes it. The real unguarded case — an **Idle** agent holding an on-device card, e.g. a `ready` card, which does not flip state (`cards.rs:245`) — is untested |
| 1215 | Device clock wrong after DST | **uncovered** (proved) | `host/crates/deckd/src/device/session.rs:336` `local_hm_is_a_clock` (format only); `host/crates/deckd/src/device/outbox.rs:1298` (a hello on the wire carries `hm`); `:239` (every 5 s) | Deleting the live-clock branch from the hello builder (`outbox.rs:807-811`) leaves all 436 deckd lib tests green (M17): **every `hm` in every test comes from a fixed `hm_override`**, so nothing exercises "hello every 5 s carries `hm`" as a *live* clock, and no test changes a clock or asserts two hellos differ. Every `hid.rs` test runs `clock_local: false`, so the production default (`[device] clock = "local"`) is untested. The device's own convergence needs the deck |

---

## Count by verdict

| Verdict | Rows | Which |
|---|---|---|
| **covered** | **8** | 1182, 1192, 1196, 1197, 1200, 1202, 1204, 1210 |
| **partial** | **24** | 1181, 1183, 1185, 1186, 1187, 1189, 1190, 1191, 1193, 1194, 1195, 1198, 1199, 1201, 1203, 1205, 1206, 1207, 1208, 1209, 1211, 1212, 1213, 1214 |
| **uncovered** | **3** | 1184, 1188, 1215 |
| **not testable here** | **0** whole rows | but four *clauses* are: Claude's own non-2xx hook behaviour (1191, 1196), the device's revert/"not delivered" toast (1190), deck Settings › Host (1207), and the device's clock convergence (1215) |

Of the 24 partials, **eleven promise behaviour that is not built at all** (not merely untested):
1185 (30 s lane hold, release keys), 1188 (the whole recovery), 1189 (fw feature-flagging),
1190 (the "not delivered to `<name>`" TTS), 1194 ("duplicate hook config"), 1198 (the "no route
to `<name>`" TTS, and `-32006` for `not_foreground`), 1199 (the `unregister` action),
1203 (the "(microphone not allowed)" transcript), 1205 (the GUI wording), 1208 (the terminal
picker), 1212 ("utterance finalised first").

---

## Proved by mutation

All mutations reverted; suites re-run green afterwards (`deck-model` 149, `deckd` lib 436).

### Went RED — the behaviour is pinned

| # | Mutation | Tests that failed | Row |
|---|---|---|---|
| M1 | `device/session.rs` `MISSES_DEGRADED: 3 → 2` | `misses_degrade_at_three_and_reopen_at_six`, `await_hello_retries_every_two_seconds` | 1186 |
| M3 | `core/mod.rs` `tick`: drop `self.step(Command::Link(LinkEvent::Overflow), now)` | `delivery_effects_resolve_and_the_overflow_reprojects_the_store`, `an_overflow_replays_the_removals_it_refused_before_re_projecting` | 1183 |
| M4 | `hid.rs` `burst_drops.len() >= 2 → >= 3` | `two_drops_after_bursts_back_the_pace_off` | 1187 |
| M5 | `hid.rs` `BACKOFF_WINDOW 300 s → 5 s` | `two_drops_after_bursts_back_the_pace_off` | 1187 |
| M6 | `store.rs` `grace_s: 300 → 120` | `a_lease_expires_into_the_orphan_grace_and_then_removal`, `tick::one_late_tick_settles_every_due_deadline` | 1181, 1195 |
| M8 | `store.rs` `DELIVERY_BUDGET 4 s → 20 s` | `no_delivery_within_four_seconds_fails_the_card_without_a_clear`, `a_push_card_waits_for_the_owner_clear_within_the_budget`, `an_orphaned_owner_downgrades_its_cards` | 1190, 1195 |
| M9 | `cards.rs` `on_hold_gone`: `degrade_card → fail_card` | `a_hold_is_written_then_cleared_and_hold_gone_degrades_at_once`, `holds_are_resolved_by_agent_and_card_not_by_card_id_alone` | 1182 |
| M10 | `agent.rs` `Card::is_open_dialog` status arm → `false` | `an_open_permission_card_refuses_the_order_before_any_job_is_built`, `every_dialog_origin_and_status_blocks_an_order_and_a_custom_card_does_not`, `a_permission_card_whose_answer_never_landed_still_blocks_an_order` | 1198 |
| M11 | `state.rs`: drop the `state.json.corrupt` rename | `a_corrupt_file_is_moved_aside` | 1210 |
| M12 | `service.rs` `LOCK_EX → LOCK_SH` | `the_lock_is_exclusive_and_released_on_drop`, `a_fork_that_copied_the_lock_fd_cannot_keep_it_after_the_guard_drops` | 1211 |
| M13 | `store.rs` `next_card_id` ignores the persisted counter | 5 `cards::` tests **and** the `props.rs` proptest (which wrote a shrink seed: `start = 0, batches = [2]`) | 1200 |
| M14 | `roster.rs` `eviction_candidate`: `min_by_key → max_by_key` | `a_full_deck_evicts_the_longest_idle_agent_never_one_with_a_card`, `an_unslotted_agent_that_wakes_up_takes_a_slot_from_an_idle_one` | 1214 (longest-idle half only) |
| M15 | `store.rs` link-down: drop `fail_answering_cards("link")` + `abort_all_ptt("link")` | `link_down_fails_answers_in_flight`, `abort_up_link_drop_cap_and_codex_are_handled`, `cancelling_a_session_that_is_still_finalizing_says_so_on_the_bus` | 1185 |
| M16 | `config.rs`: drop the `config.toml.invalid` copy | `hot_reload_keeps_last_good_on_invalid` | 1209 |

### Stayed GREEN — the clause is **not** pinned by any test

| # | Mutation | Result | Row / clause |
|---|---|---|---|
| M2 | `core/mod.rs` `needs_resync && hid.ops.len() < OPS_CAPACITY/2` → `<= OPS_CAPACITY` | 436/436 deckd lib tests pass | 1183 "when the channel drains **below half**". The `assert!(c.needs_resync, "still full")` is satisfied by the side-queue replay re-setting the flag, not by the gate |
| MB | `outbox.rs` `BACKOFF_FOR 600 s → 60 s` | 436/436 pass | 1187 "pace 10 ms for **10 min**" |
| M7 | `roster.rs` `eviction_candidate`: drop `&& !a.card…on_device()` | 149/149 deck-model tests pass | 1214 "**never one with a pending card**" |
| M17 | `outbox.rs` hello builder: drop the `clock_local.then(local_hm(now))` branch | 436/436 pass | 1215 "hello every 5 s carries `hm`" as a live clock |

---

## Looks covered, weak on reading

1. **`the_pacer_spaces_reports_by_at_least_eight_ms`** (`deckd/src/device/outbox.rs:978`) — the
   ten-minute back-off is asserted as `pace_at(t0 + BACKOFF_FOR) == 8 ms`, i.e. *in terms of the
   constant*. Exactly the "30 s cache that passed at 3600 s" pattern. Proven green at 60 s.
2. **`a_full_deck_evicts_the_longest_idle_agent_never_one_with_a_card`**
   (`deck-model/src/roster.rs:1460`) — the name promises two properties; only one is load-bearing.
   The card-holder the test builds is in `Question` state, so the earlier `state == Idle` filter
   answers first and the card clause never runs. Exactly the "guard-3 clause that could be deleted"
   pattern. Proven green with the clause removed.
3. **`delivery_effects_resolve_and_the_overflow_reprojects_the_store`**
   (`deckd/src/core/mod.rs:3214`) — reads as a test of the half-drain gate; it is not. It drains the
   receiver completely, and the intermediate "still full" assertion is held up by an unrelated code
   path. Proven green with the gate widened to always-true.
4. **`an_unsent_card_and_its_clear_cancel_and_focus_keeps_the_highest_ack`**
   (`deckd/src/device/outbox.rs:1298`) — the only place in the workspace where a hello on the wire
   carries `hm`, and it does so via `set_clock(false, Some("19:42"))`, an override. The DST row's
   whole mechanism is therefore invisible to the suite. Proven green.
5. **`a_failed_hold_card_refuses_retry_and_a_timeout_card_can_be_released`**
   (`deck-model/src/cards.rs:2155`) — the second half releases through the *adapter's* `CardClear`,
   not the operator's button. Already known: the sibling `the_gui_releases_a_failed_hold_card_to_the_terminal`
   (`:2107`) was added for the real path and says so in its doc comment. Listed for completeness.
6. **`voice_mixer::the_release_plays_the_announcement_at_pitch_into_the_sco_tail`** — its doc comment
   claims it covers `ScoStuck`; the body only ever reaches `Reopening`. The resampling it measures
   is real, but it is not row 1205's state.
7. **`launchd::a_crash_is_anything_else_and_a_signal_leaves_no_code_at_all`** — reads like row 1181's
   detection column; it is a parser test over synthetic `launchctl print` text. No daemon crashes
   anywhere in the suite.
8. **`deck_screen::a_failed_answer_offers_release_and_only_offers_retry_off_a_hold`** — asserts the
   hold/poll split of Retry, which reads as the whole of row 1190's Retry clause. The clause the row
   actually promises, "Retry … with a **live owner session**", cannot be caught here because
   `present::retry_offered` never consults the owner.
9. **`bus_replay::lagged_subscriber_gets_resync_and_events_replay_with_gap`** and the
   `reducer.rs` `…resync…` family — these match "resync" by name but are the event-bus / GUI rev-gap
   resync, not the core→hid overflow of row 1183. Do not count them toward it.

---

## Smallest set of new tests that would close the gaps, in priority order

Ordered by risk-per-line: 1–4 are the ones a regression could reach silently today.

1. **Hello carries a live local clock** (row 1215, one test, `outbox.rs`). Set `clock_local: true`,
   no override; queue two `Control::Hello`s an hour of simulated wall-clock apart (inject the clock
   through `set_clock`/a `now` parameter rather than `SystemTime::now()`); assert both lines carry an
   `hm` and that they differ. Kills M17. Requires making the clock injectable — the one production
   change this list implies, and it is the change that makes the row testable at all.
2. **An idle agent with a card is never unslotted** (row 1214, ~15 lines, `roster.rs`). Eight agents;
   give the longest-idle one a `CardKind::Ready` card (which leaves it `Idle`); register a ninth and
   assert the card-holder keeps its slot. Kills M7.
3. **The overflow re-projection waits for the half-drain** (row 1183, ~10 lines, `core/mod.rs`). Fill
   the channel, `send_op` a **`Put`** only (so the side queue stays empty), drain to
   `OPS_CAPACITY/2 + 1`, tick, assert `needs_resync` is still set; drain one more, tick, assert it
   clears. Kills M2. While there, assert `metrics.get`'s `overflows` is non-zero — and either fix the
   doc's `outbox_overflows` or rename the field.
4. **The back-off lasts ten minutes** (row 1187, 3 lines, `outbox.rs`). Replace
   `pace_at(t0 + BACKOFF_FOR)` with literals: `pace_at(t0 + 9 min) == 10 ms` and
   `pace_at(t0 + 11 min) == 8 ms`. Kills M-BACKOFF.
5. **A second daemon exits 0 and `status` names the owner** (row 1211, ~20 lines, integration).
   Start a `--no-device` daemon, run a second `deckd run` on the same data dir, assert exit status 0
   and the log line; then run `deckd status` and assert it prints `running (pid <first>)`.
6. **The core drains device lines before API commands** (row 1184, ~20 lines). Queue N `FromHid::Line`s
   and one `CoreMsg` on a stopped core, run one loop iteration, assert every device line was folded
   before the message. A second assertion that `hid.lines` is unbounded (push > `OPS_CAPACITY` without
   blocking) covers the rest of the row.
7. **A daemon that dies mid-hold** (row 1181, ~30 lines, integration). Park a hook over UDS, `SIGKILL`
   the daemon, assert the helper's connection ends (so Claude falls back to its own dialog); restart
   and assert the new `sid` produces a barrier. This also gives the "≤10 s" and "15 s offline" numbers
   somewhere to live.
8. **The GUI surfaces a device `Degraded` session and a `config_invalid` error** (rows 1186, 1209,
   two small reducer/present tests). `present::deck_line(Link::Degraded)` has zero coverage today, and
   `reducer.rs:1237` currently *asserts* that `EventKind::Error` is ignored — so decide whether §12's
   "GUI banner" is a promise or a doc leftover, and make the test say which.
9. **The Claude dedupe key really is per session and per tool** (row 1194, ~10 lines,
   `adapters/claude.rs`). Same `tool_input`, different `session_id` → two cards; same input,
   different `tool_name` → two cards.
10. ~~**`fallback_input` when the deck's UID is absent** (row 1207, ~10 lines, `coreaudio.rs`).~~
    Done with decision 11, which removed the key: `CpalCapture::choose` is tested without CoreAudio
    (deck + UID → by UID, deck + none → `no_device`, `input:"mac"` → the system input).
11. **`deckd device flood 2000` in the hardware gate** (row 1187). The row names the command by
    number and the gate never runs it; add it to the `--hardware` block beside the existing soak.

Seven doc rows should be **corrected rather than tested**, because the behaviour they promise was
never built: 1185 (30 s lane hold), 1188 (the whole `err range` recovery), 1189 (fw feature-flagging),
1190/1198 (the two TTS lines), 1194 ("duplicate hook config"), 1198 (`-32006` for `not_foreground`),
1208 (the terminal picker), 1212 ("utterance finalised first" and "≤3 s"). Three more carry stale
numbers or wording: 1204 (1.5 s vs 2 500 ms), 1205 (the GUI sentence), 1183 (`outbox_overflows`).
