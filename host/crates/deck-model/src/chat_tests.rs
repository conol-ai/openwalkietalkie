use crate::{
    testkit::{Sim, UID},
    *,
};

fn send(sim: &mut Sim, agent: &AgentId, id: &str, role: ChatRole, streaming: bool, text: &str) {
    sim.api(
        UID,
        SessionId(1),
        Request::Reply(ReplySet {
            agent: agent.clone(),
            text: text.into(),
            message: Some(MessageMeta { summary: None, images: Vec::new(),
                activity: None,
                runs: None, plan: None,
                id: id.into(),
                role,
                streaming,
            }),
            speak: true,
            announce: None,
            lane: None,
        }),
    )
    .unwrap();
}

#[test]
fn native_activity_uses_public_labels_and_settles_without_speaking() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "Conol", None);
    let state = |value: &str| serde_json::from_value(serde_json::json!({
        "method":"agent.update", "params":{"agent":agent, "state":value}
    })).unwrap();
    sim.api(UID, SessionId(1), state("working")).unwrap();
    send(&mut sim, &agent, "u1", ChatRole::User, false, "Find the chip");
    for n in 0..5 {
        send(&mut sim, &agent, &format!("old-{n}"), ChatRole::Tool, false, &format!("**Finished tool {n}**"));
    }
    let reply = |running| serde_json::from_value(serde_json::json!({
        "method":"agent.reply", "params":{
            "agent":agent, "text":"**Command**\n\nPublic command preview", "speak":false,
            "message":{"id":"activity:call-1", "role":"tool", "streaming":running,
                "activity":if running {"running"} else {"completed"},
                "summary":{"label":if running {"Running a command"} else {"Ran a command"}, "detail":"mkdir -p ~/workspace/plaud"}}
        }
    })).unwrap();
    sim.api(UID, SessionId(1), reply(true)).unwrap();
    let a = &sim.store.snapshot().agents[&agent];
    let (_, lines) = crate::store::chat_lines(a, 0, None);
    assert!(lines.join(" ").contains("Running a command"));
    assert!(workflow::activity(a).last().unwrap().title.contains("mkdir -p ~/workspace/plaud"));
    sim.api(UID, SessionId(1), reply(false)).unwrap();
    let a = &sim.store.snapshot().agents[&agent];
    assert_eq!(a.messages.len(), 7, "stable call id updates in place");
    assert_eq!(workflow::activity(a).last().unwrap().status, workflow::ActivityStatus::Completed);
    assert!(!crate::store::chat_lines(a, 0, None).1.join(" ").contains("Working…"));
    sim.api(UID, SessionId(1), reply(true)).unwrap();
    sim.api(UID, SessionId(1), state("ready")).unwrap();
    let a = &sim.store.snapshot().agents[&agent];
    assert!(!crate::store::chat_lines(a, 0, None).1.join(" ").contains("Working…"));
    assert_eq!(workflow::activity(a).last().unwrap().status, workflow::ActivityStatus::Stopped);
    assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Audio(AudioCmd::Say { .. }))));
    assert!(redact(sim.store.snapshot(), &[Scope::Observe]).agents[&agent].messages.is_empty());
}

#[test]
fn public_activity_summaries_are_bounded_and_only_apply_to_tools() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "native", None);
    for role in ["tool", "assistant"] {
        let request = serde_json::from_value(serde_json::json!({"method":"agent.reply", "params":{
            "agent":agent, "text":"Public text", "message":{"id":role,"role":role,
                "summary":{"label":"界".repeat(1000),"detail":"preview\n".repeat(1000)}}
        }})).unwrap();
        sim.api(UID, SessionId(1), request).unwrap();
    }
    let messages = &sim.store.snapshot().agents[&agent].messages;
    let summary = messages[0].message.summary.as_ref().unwrap();
    assert!(summary.label.len() <= 160 && summary.detail.len() <= 320);
    assert!(!summary.detail.contains('\n'));
    assert!(messages[1].message.summary.is_none());
}

#[test]
fn images_are_visible_with_collapsed_tools_and_invalidate_changed_indices() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "bot", None);
    let image = ChatImage { id: "a".repeat(64), label:"Screenshot".into(), width:640, height:480 };
    let mut meta = MessageMeta { summary: None, images:vec![image], id:"tool-1".into(), role:ChatRole::Tool, streaming:false, activity:None, runs:None, plan:None };
    let mut submit = |meta: MessageMeta| { sim.api(UID, SessionId(1), Request::Reply(ReplySet { agent:agent.clone(), text:"**Read image**".into(), message:Some(meta), speak:false, announce:None, lane:None })).unwrap(); };
    submit(meta.clone());
    let before = sim.store.snapshot().agents[&agent].chat_rev;
    let (_, lines) = crate::store::chat_lines(&sim.store.snapshot().agents[&agent], 0, None);
    assert!(lines.iter().any(|line| line.starts_with("i000")));
    meta.images.clear();
    sim.api(UID, SessionId(1), Request::Reply(ReplySet { agent:agent.clone(), text:"**Read image**".into(), message:Some(meta), speak:false, announce:None, lane:None })).unwrap();
    assert_ne!(sim.store.snapshot().agents[&agent].chat_rev, before);
    let redacted = redact(sim.store.snapshot(), &[Scope::Observe]);
    assert!(redacted.agents[&agent].messages.is_empty());
}

#[test]
fn streaming_upserts_history_without_speaking_or_moving_an_older_reading_window() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "bot", None);
    send(
        &mut sim,
        &agent,
        "u1",
        ChatRole::User,
        false,
        &"question ".repeat(100),
    );
    assert!(!sim
        .effects
        .iter()
        .any(|e| matches!(e, Effect::Audio(AudioCmd::Say { .. }))));
    let rev = sim.store.snapshot().agents[&agent].chat_rev;
    sim.apply(Command::Device(DeviceLine::DocLines {
        slot: 1,
        doc: DocKind::Reply,
        id: rev,
        first: 3,
    }));
    send(&mut sim, &agent, "a1", ChatRole::Assistant, true, "Hello");
    send(
        &mut sim,
        &agent,
        "a1",
        ChatRole::Assistant,
        true,
        "Hello **world**",
    );
    assert!(matches!(
        sim.line(Key::DocText(Slot(1), DocKind::Reply, rev)),
        Some(HostLine::DocText { first: 3, .. })
    ));
    assert!(!sim
        .effects
        .iter()
        .any(|e| matches!(e, Effect::Audio(AudioCmd::Say { .. }))));
    let a = &sim.store.snapshot().agents[&agent];
    assert_eq!(a.messages.len(), 2);
    assert_eq!(a.messages[1].text, "Hello **world**");
    assert!(a.reply.is_none());
    assert_eq!(a.chat_rev, rev);
    send(
        &mut sim,
        &agent,
        "a1",
        ChatRole::Assistant,
        false,
        "Hello **world**!",
    );
    let a = &sim.store.snapshot().agents[&agent];
    assert_eq!(a.messages.len(), 2);
    assert_eq!(
        a.reply.as_ref().unwrap().text.as_deref(),
        Some("Hello **world**!")
    );
    let redacted = redact(sim.store.snapshot(), &[Scope::Observe]);
    assert!(redacted.agents[&agent].messages.is_empty());
}

#[test]
fn conversation_is_bounded_and_each_message_has_its_own_markdown_document() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "bot", None);
    for i in 0..66 {
        send(
            &mut sim,
            &agent,
            &format!("a{i}"),
            ChatRole::Assistant,
            false,
            "```\nunfinished fence",
        );
    }
    let a = &sim.store.snapshot().agents[&agent];
    assert_eq!(a.messages.len(), 64);
    assert_eq!(a.messages[0].message.id, "a2");
    assert!(a.chat_rev > 1);
    let Some(HostLine::DocText {
        lines,
        total,
        first,
        ..
    }) = crate::store::agent_doc_window(a, DocKind::Reply, u16::MAX, 1, None)
    else {
        panic!("window");
    };
    assert_eq!(usize::from(first) + lines.len(), usize::from(total));
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("h0") && l.contains("64")),
        "a previous open fence cannot eat a message heading"
    );
}

#[test]
fn tools_are_compact_until_tapped_and_keep_their_state_through_updates_and_resync() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "bot", None);
    send(&mut sim, &agent, "t1", ChatRole::Tool, true, "**Command**\n\n```\ncargo test\n```\n\noutput");
    send(&mut sim, &agent, "t2", ChatRole::Tool, false, "**Read file**\n\nsecond tool details");
    let rev = sim.store.snapshot().agents[&agent].chat_rev;
    let window = |sim: &Sim| {
        let id = sim.store.snapshot().agents[&agent].chat_rev;
        let Some(HostLine::DocText { lines, total, .. }) = sim.line(Key::DocText(Slot(1), DocKind::Reply, id)) else { panic!("window"); };
        (total, lines)
    };
    let (total, rows) = window(&sim);
    assert_eq!(total, 4);
    assert_eq!(rows.iter().filter(|s| deck_wrap::tool_entry(s).is_some_and(|(_, open)| !open)).count(), 1);
    assert!(rows.join(" ").contains("cargo test"), "the command summary stays visible");
    assert!(!rows.join(" ").contains("output"));
    assert!(matches!(sim.line(Key::Reply(Slot(1))), Some(HostLine::Reply { text, .. }) if !text.contains("details")));
    sim.apply(Command::Device(DeviceLine::Tool { slot: 1, id: rev, index: 0 }));
    let opened = sim.store.snapshot().agents[&agent].chat_rev;
    assert_ne!(opened, rev);
    let (total, rows) = window(&sim);
    assert!(total > 4);
    assert!(rows.iter().any(|s| deck_wrap::tool_entry(s) == Some((0, true))));
    assert!(rows.join(" ").contains("cargo test"));
    // A duplicated tap cannot undo the first tap after the offsets change.
    sim.apply(Command::Device(DeviceLine::Tool { slot: 1, id: rev, index: 0 }));
    assert_eq!(sim.store.snapshot().agents[&agent].chat_rev, opened);
    send(&mut sim, &agent, "t1", ChatRole::Tool, false, "**Command · completed**\n\nall tests passed");
    assert!(window(&sim).1.join(" ").contains("all tests passed"));
    assert!(sim.store.project().iter().any(|op| matches!(op, DeviceOp::Put(_, HostLine::DocText { lines, .. }) if lines.iter().any(|s| deck_wrap::tool_entry(s) == Some((0, true))))));
    sim.apply(Command::Device(DeviceLine::Tool { slot: 1, id: opened, index: 0 }));
    assert_eq!(window(&sim).0, 4);
    send(&mut sim, &agent, "t1", ChatRole::Tool, false, &format!("**Command · completed**\n\n{}", "lots of output\n".repeat(100)));
    assert_eq!(window(&sim).0, 4, "updates never expand a collapsed tool");
    let rev = sim.store.snapshot().agents[&agent].chat_rev;
    sim.apply(Command::Device(DeviceLine::Tool { slot: 1, id: rev, index: 63 }));
    assert_eq!(sim.store.snapshot().agents[&agent].chat_rev, rev);
}

#[test]
fn returning_to_the_tail_or_reopening_a_channel_resumes_host_tail_windows() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "bot", None);
    send(&mut sim, &agent, "a1", ChatRole::Assistant, true, &"line\n\n".repeat(50));
    let rev = sim.store.snapshot().agents[&agent].chat_rev;
    sim.apply(Command::Device(DeviceLine::DocLines { slot: 1, doc: DocKind::Reply, id: rev, first: 3 }));
    sim.apply(Command::Device(DeviceLine::DocLines { slot: 1, doc: DocKind::Reply, id: rev, first: u16::MAX }));
    send(&mut sim, &agent, "a1", ChatRole::Assistant, true, &"more text\n\n".repeat(60));
    let Some(HostLine::DocText { first, total, lines, .. }) = sim.line(Key::DocText(Slot(1), DocKind::Reply, rev)) else { panic!(); };
    assert_eq!(first as usize + lines.len(), total as usize);
    sim.apply(Command::Device(DeviceLine::DocLines { slot: 1, doc: DocKind::Reply, id: rev, first: 3 }));
    sim.apply(Command::Device(DeviceLine::Channel { slot: 1, seq: 2 }));
    let Some(HostLine::DocText { first, total, lines, .. }) = sim.line(Key::DocText(Slot(1), DocKind::Reply, rev)) else { panic!(); };
    assert_eq!(first as usize + lines.len(), total as usize);
}

#[test]
fn related_reads_show_an_explored_summary_and_expand_as_one_group() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "bot", None);
    for (i, file) in ["voice.rs", "outbox.rs", "contract.rs", "store.rs"].into_iter().enumerate() {
        send(&mut sim, &agent, &format!("read-{i}"), ChatRole::Tool, false,
            &format!("**Read · completed**\n\n```json\n{{\"file_path\":\"/repo/src/{file}\"}}\n```\n\nprivate output {i}"));
    }
    let a = &sim.store.snapshot().agents[&agent];
    let (_, lines) = crate::store::chat_lines(a, 1, None);
    let text = lines.join(" ");
    assert!(text.contains("Explored") && text.contains("voice.rs") && text.contains("outbox.rs") && text.contains("contract.rs"));
    assert!(text.contains("1 more") && text.contains("Show details"));
    assert!(!text.contains("private output"));
    assert_eq!(lines.iter().filter(|l| deck_wrap::tool_entry(l).is_some()).count(), 1);
    let expanded = std::collections::BTreeSet::from(["read-0".to_string()]);
    let (_, lines) = crate::store::chat_lines(a, 1, Some(&expanded));
    assert!(lines.join(" ").contains("private output 3"));
}

fn plan_message(sim: &mut Sim, agent: &AgentId, id: &str, status: &str) {
    let plan = workflow::Plan::codex(&serde_json::json!({"plan":[{"step":"Inspect the input", "status":status}]})).unwrap();
    sim.api(UID, SessionId(1), Request::Reply(ReplySet {
        agent: agent.clone(), text: plan.text(),
        message: Some(MessageMeta { summary: None, images: Vec::new(), id: id.into(), role: ChatRole::Tool, streaming: false, runs: None, plan: Some(plan), activity: None }),
        speak: false, announce: None, lane: None,
    })).unwrap();
}

#[test]
fn workflow_survives_tail_eviction_and_rejects_older_plan_results() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "bot", None);
    plan_message(&mut sim, &agent, "p1", "pending");
    plan_message(&mut sim, &agent, "p2", "inProgress");
    plan_message(&mut sim, &agent, "p1", "completed");
    assert_eq!(sim.store.snapshot().agents[&agent].workflow.as_ref().unwrap().completed(), 0);
    plan_message(&mut sim, &agent, "p2", "completed");
    assert_eq!(sim.store.snapshot().agents[&agent].workflow.as_ref().unwrap().completed(), 1);
    for i in 0..70 {
        send(&mut sim, &agent, &format!("a{i}"), ChatRole::Assistant, false, "Progress");
    }
    assert_eq!(sim.store.snapshot().agents[&agent].messages.len(), 64);
    assert_eq!(sim.store.snapshot().agents[&agent].workflow.as_ref().unwrap().completed(), 1);
    send(&mut sim, &agent, "next-user", ChatRole::User, false, "Next task");
    assert!(sim.store.snapshot().agents[&agent].workflow.is_none());
}

#[test]
fn workflow_content_obeys_snapshot_and_event_permissions() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "bot", None);
    plan_message(&mut sim, &agent, "p1", "pending");
    let snapshot = sim.store.snapshot();
    assert!(redact(snapshot, &[Scope::Observe]).agents[&agent].workflow.is_none());
    assert!(redact(snapshot, &[Scope::ObserveContent]).agents[&agent].workflow.is_some());
    assert!(redact_for(snapshot, &[Scope::Observe], &std::collections::BTreeSet::from([agent.clone()])).agents[&agent].workflow.is_some());
    let event = Event { rev: 1, ts: Ts::from_millis(1), kind: EventKind::ReplySet, principal: UID,
        data: serde_json::json!({"agent":agent, "text":"private plan", "message":snapshot.agents[&agent].messages[0].message}) };
    let filtered = redact_event(&event, &[Scope::Observe], false).unwrap();
    assert!(filtered.data["message"]["plan"].is_null());
    assert!(!serde_json::to_string(&filtered).unwrap().contains("Inspect the input"));
    assert_eq!(redact_event(&event, &[Scope::Observe], true).unwrap(), event);
    assert_eq!(redact_event(&event, &[Scope::ObserveContent], false).unwrap(), event);
}

#[test]
fn device_workflow_is_opt_in_updates_live_and_survives_resync() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "bot", None);
    plan_message(&mut sim, &agent, "plan", "inProgress");
    assert!(!sim.store.project().iter().any(|op| matches!(op,
        DeviceOp::Put(_, HostLine::DocText { doc: DocKind::Workflow, .. }))));
    let rev = sim.store.snapshot().agents[&agent].chat_rev;
    sim.apply(Command::Device(DeviceLine::DocLines { slot: 1, doc: DocKind::Workflow, id: rev, first: 0 }));
    let Some(HostLine::DocText { lines, first: 0, .. }) = sim.line(Key::DocText(Slot(1), DocKind::Workflow, rev)) else { panic!("workflow window"); };
    assert!(lines.iter().any(|line| line == "w00001"));
    plan_message(&mut sim, &agent, "plan", "completed");
    let rev = sim.store.snapshot().agents[&agent].chat_rev;
    let Some(HostLine::DocText { lines, first: 0, .. }) = sim.line(Key::DocText(Slot(1), DocKind::Workflow, rev)) else { panic!("live workflow window"); };
    assert!(lines.iter().any(|line| line == "w00101"));
    assert!(sim.store.project().iter().any(|op| matches!(op,
        DeviceOp::Put(_, HostLine::DocText { doc: DocKind::Workflow, first: 0, .. }))));
    sim.apply(Command::Device(DeviceLine::DocLines { slot: 1, doc: DocKind::Reply, id: rev, first: u16::MAX }));
    send(&mut sim, &agent, "a1", ChatRole::Assistant, false, "Finished");
    assert!(sim.line(Key::DocText(Slot(1), DocKind::Reply, rev)).is_some());
    assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Device(DeviceOp::Put(_, HostLine::DocText { doc: DocKind::Workflow, .. })))));
}

#[test]
fn device_workflow_disclosures_keep_outputs_collapsed_and_reject_stale_taps() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "bot", None);
    let plan = workflow::Plan::codex(&serde_json::json!({"plan":(0..8).map(|i|
        serde_json::json!({"step":format!("Step {i}"), "status":if i < 5 {"completed"} else if i == 5 {"inProgress"} else {"pending"}})).collect::<Vec<_>>()})).unwrap();
    sim.api(UID, SessionId(1), Request::Reply(ReplySet { agent: agent.clone(), text: plan.text(),
        message: Some(MessageMeta { summary: None, images: Vec::new(), id: "p".into(), role: ChatRole::Tool, streaming: false, runs: None, plan: Some(plan), activity: None }),
        speak: false, announce: None, lane: None })).unwrap();
    send(&mut sim, &agent, "t1", ChatRole::Tool, false, "**Bash · failed**\n\n```\ncargo test\n```\n\nprivate output");
    let rev = sim.store.snapshot().agents[&agent].chat_rev;
    sim.apply(Command::Device(DeviceLine::DocLines { slot: 1, doc: DocKind::Workflow, id: rev, first: 0 }));
    let (_, lines) = workflow::device_lines(&sim.store.snapshot().agents[&agent], 0, None);
    assert!(lines.join(" ").contains("Step 5"));
    assert!(!lines.join(" ").contains("Step 0"));
    assert!(!lines.join(" ").contains("private output"));
    assert!(lines.iter().any(|s| deck_wrap::tool_entry(s) == Some((64, false))));
    let anchor = lines.iter().position(|s| deck_wrap::tool_entry(s) == Some((64, false))).unwrap();
    sim.apply(Command::Device(DeviceLine::Tool { slot: 1, id: rev, index: 64 }));
    let new_rev = sim.store.snapshot().agents[&agent].chat_rev;
    assert_ne!(new_rev, rev);
    let full = &sim.store.aux[&agent].expanded_tools;
    let (_, lines) = workflow::device_lines(&sim.store.snapshot().agents[&agent], 0, Some(full));
    assert!(lines.join(" ").contains("Step 0"));
    assert_eq!(lines.iter().position(|s| deck_wrap::tool_entry(s) == Some((64, true))), Some(anchor));
    assert!(lines[anchor + 1].contains("Step 0"));
    sim.apply(Command::Device(DeviceLine::Tool { slot: 1, id: rev, index: 64 }));
    assert_eq!(sim.store.snapshot().agents[&agent].chat_rev, new_rev);
    sim.apply(Command::Device(DeviceLine::Tool { slot: 1, id: new_rev, index: 1 }));
    let (_, lines) = workflow::device_lines(&sim.store.snapshot().agents[&agent], 0, Some(&sim.store.aux[&agent].expanded_tools));
    assert!(lines.join(" ").contains("private output"));
}

#[test]
fn background_workflow_runs_survive_new_turns_update_the_device_and_are_redacted() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "bot", None);
    let runs = vec![workflow::Run { id: "wf_test".into(), name: "Parallel design checks".into(),
        status: workflow::ActivityStatus::Running, completed: 0, total: 9,
        elapsed_ms: 76_000, tokens: 488_100 }];
    let meta = MessageMeta { summary: None, images: Vec::new(), id: "workflow:runs".into(), role: ChatRole::Tool, streaming: false,
        plan: None, activity: None, runs: Some(runs.clone()) };
    sim.api(UID, SessionId(1), Request::Reply(ReplySet {
        agent: agent.clone(), text: "Workflow runs".into(), message: Some(meta.clone()),
        speak: false, announce: None, lane: None,
    })).unwrap();
    send(&mut sim, &agent, "u1", ChatRole::User, false, "Another request");
    let snapshot = sim.store.snapshot();
    assert_eq!(snapshot.agents[&agent].workflow_runs, runs);
    assert!(workflow::activity(&snapshot.agents[&agent]).is_empty());
    for size in 0..=2 {
        let (_, lines) = workflow::device_lines(&snapshot.agents[&agent], size, None);
        assert!(lines.iter().any(|l| l == "w00009"));
        assert!(lines.iter().all(|l| l.len() <= deck_proto::caps().doc_line));
        assert!(!lines.join(" ").contains("Waiting for plan"));
    }
    assert_eq!(runs[0].metrics(), "0/9 · 1m16s · 488.1k tokens");
    assert!(redact(snapshot, &[Scope::Observe]).agents[&agent].workflow_runs.is_empty());
    assert_eq!(redact(snapshot, &[Scope::ObserveContent]).agents[&agent].workflow_runs, runs);
    let event = Event { rev: 1, ts: Ts::from_millis(1), kind: EventKind::ReplySet, principal: UID,
        data: serde_json::json!({"agent":agent,"text":"Workflow runs","message":meta}) };
    let filtered = redact_event(&event, &[Scope::Observe], false).unwrap();
    assert!(filtered.data["message"]["runs"].is_null());
    assert!(!serde_json::to_string(&filtered).unwrap().contains("Parallel design"));
}

fn history_message(id: &str, role: ChatRole) -> MessageMeta {
    MessageMeta { summary: None, images: Vec::new(), activity: None, runs: None, plan: None,
        id: id.into(), role, streaming: false }
}

/// R6, the lazy reader feed: a session's history read again replaces the
/// one kept, in order, as one new document. The stale messages and the
/// stored reply's copy go; the reply on file stays, since the adapters
/// store each turn's own; the bus hears one `reply.set` per message, as
/// an eager feed would have said them; the deck gets one reply document and
/// hears nothing spoken. The same history again changes nothing at all.
#[test]
fn a_history_read_again_replaces_the_kept_one_in_order_as_one_document() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "claude", None);
    let slot = sim.slot_of(&agent).unwrap();
    send(&mut sim, &agent, "A", ChatRole::Assistant, false, "Stale A");
    send(&mut sim, &agent, "B", ChatRole::User, false, "Stale B");
    sim.api(UID, SessionId(1), Request::Reply(ReplySet {
        agent: agent.clone(), text: "All green.".into(), message: None,
        speak: false, announce: None, lane: None,
    })).unwrap();
    let before = sim.store.snapshot().agents[&agent].clone();
    assert_eq!(before.messages.len(), 3);
    let history = vec![
        (history_message("U1", ChatRole::User), "Run the tests".to_string()),
        (history_message("tool:T1", ChatRole::Tool), "**Bash · completed**\n\n12 passed".to_string()),
        (history_message("A1", ChatRole::Assistant), "All green.".to_string()),
    ];
    sim.apply(Command::ChatReplace { agent: agent.clone(), messages: history.clone() });
    let after = &sim.store.snapshot().agents[&agent];
    let ids: Vec<&str> = after.messages.iter().map(|m| m.message.id.as_str()).collect();
    assert_eq!(ids, ["U1", "tool:T1", "A1"]);
    assert_ne!(after.chat_rev, before.chat_rev, "a new document");
    assert_eq!((&after.reply, after.reply_rev), (&before.reply, before.reply_rev));
    let published: Vec<&str> = sim.events.iter().filter(|e| e.kind == EventKind::ReplySet)
        .map(|e| e.data["message"]["id"].as_str().unwrap()).collect();
    assert_eq!(published, ["U1", "tool:T1", "A1"]);
    assert_eq!(sim.puts().iter().filter(|k| **k == Key::Reply(slot)).count(), 1);
    assert!(!sim.effects.iter().any(|e| matches!(e, Effect::Audio(AudioCmd::Say { .. }))));
    // Read again with nothing new: no event, no line, no new revision.
    let rev = after.chat_rev;
    sim.apply(Command::ChatReplace { agent: agent.clone(), messages: history });
    assert!(sim.events.is_empty() && sim.effects.is_empty(), "{:?}", sim.kinds());
    assert_eq!(sim.store.snapshot().agents[&agent].chat_rev, rev);
    // A message `agent.reply` would refuse is left out, not the whole read.
    sim.apply(Command::ChatReplace { agent: agent.clone(), messages: vec![
        (history_message("", ChatRole::Assistant), "No id".into()),
        (history_message("U2", ChatRole::User), "  ".into()),
        (history_message("A2", ChatRole::Assistant), "Done.".into()),
    ] });
    let ids: Vec<String> = sim.store.snapshot().agents[&agent].messages.iter().map(|m| m.message.id.clone()).collect();
    assert_eq!(ids, ["A2"]);
}

/// R6: a history longer than the store keeps, read again unchanged, is the
/// same document. Filing it trims to the newest 64, which bumps the
/// revision as it goes; the deck's copy is still the right one, so the
/// revision stays and nothing is sent. When the history does change, the
/// messages the kept one had keep the time they first arrived.
#[test]
fn a_long_history_read_again_is_the_same_document_and_old_messages_keep_their_time() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "claude", None);
    let long: Vec<(MessageMeta, String)> = (0..70)
        .map(|n| (history_message(&format!("m{n}"), ChatRole::Assistant), format!("Message {n}")))
        .collect();
    sim.apply(Command::ChatReplace { agent: agent.clone(), messages: long.clone() });
    let first = sim.store.snapshot().agents[&agent].clone();
    assert_eq!(first.messages.len(), 64);
    assert_eq!(first.messages[0].message.id, "m6", "the newest 64");

    sim.advance(std::time::Duration::from_secs(5));
    sim.apply(Command::ChatReplace { agent: agent.clone(), messages: long.clone() });
    assert!(sim.events.is_empty() && sim.effects.is_empty(), "{:?}", sim.kinds());
    assert_eq!(sim.store.snapshot().agents[&agent], first, "the revision and every arrival time");

    let mut longer = long;
    longer.push((history_message("m70", ChatRole::Assistant), "Message 70".into()));
    sim.apply(Command::ChatReplace { agent: agent.clone(), messages: longer });
    let after = &sim.store.snapshot().agents[&agent];
    assert_ne!(after.chat_rev, first.chat_rev, "a new document");
    assert_eq!(after.messages[0].message.id, "m7");
    assert_eq!(after.messages[0].at, first.messages[1].at, "m7 came with the first read");
    assert!(after.messages[63].at > first.messages[63].at, "m70 came just now");
}

/// R6: a session's workflow runs come with the history the reader feed
/// reads and count in the roster's `subs`; when the deck stops reading the
/// session nothing follows them, so they go (and the count with them) while
/// the history stays for the next read.
#[test]
fn the_workflow_runs_go_with_the_reader_and_the_history_stays() {
    let mut sim = Sim::new();
    let agent = sim.register(UID, SessionId(1), "claude", None);
    let slot = sim.slot_of(&agent).unwrap();
    let running = workflow::Run { id: "wf_1".into(), name: "Checks".into(),
        status: workflow::ActivityStatus::Running, completed: 1, total: 3, elapsed_ms: 1_000, tokens: 10 };
    let mut runs = history_message("workflow:runs", ChatRole::Tool);
    runs.runs = Some(vec![running]);
    sim.apply(Command::ChatReplace { agent: agent.clone(), messages: vec![
        (history_message("U1", ChatRole::User), "Check everything".into()),
        (runs, "Checks · running · 1/3".into()),
    ] });
    assert_eq!(sim.store.snapshot().agents[&agent].active_work(), 1);
    let subs = |sim: &Sim| match sim.line(Key::AgentState(slot)) {
        Some(HostLine::Agent { subs, .. }) => subs,
        other => panic!("{other:?}"),
    };
    assert_eq!(subs(&sim), Some(1), "the roster's count follows the runs");

    sim.apply(Command::ReaderLeft { agent: agent.clone() });
    let a = &sim.store.snapshot().agents[&agent];
    assert!(a.workflow_runs.is_empty());
    assert_eq!(a.messages.len(), 2, "the history stays");
    assert_eq!(subs(&sim), Some(0));
    assert_eq!(sim.kinds(), [EventKind::AgentChanged]);
    assert_eq!(sim.events[0].data["fields"], serde_json::json!({"workflow_runs": []}));
    // Nothing left to drop: nothing happens.
    sim.apply(Command::ReaderLeft { agent: agent.clone() });
    assert!(sim.events.is_empty() && sim.effects.is_empty());
}
