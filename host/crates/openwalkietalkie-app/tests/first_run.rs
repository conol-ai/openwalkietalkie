//! First run's rules, read without a window and with a fake installer, so
//! nothing here looks for a deck, registers a service or writes a file:
//! the stepper's marks, the probe's answer, the search in words, every
//! branch of the step machine (nothing registered before **Add These**,
//! nothing at all by **Not Now**), when adding the plugin is finished and
//! the command that finishes it when it is not (the Integrations page's
//! rule, `present::finished`), step 2's and step 4's lines, and that every
//! word the page says has a row.
use deck_client::ClientError;
use deck_gui_core::admin::{Status, Verb};
use deck_gui_core::i18n::Language;
use deck_gui_core::TestStatus;
use deck_model::{ApiError, Tone, Transport, Ts};
use openwalkietalkie_app::firstrun::{
    adding_line, adding_remedy, claude_status, mark, plugin_outcome, search_status, set_up_step,
    step_name, Adding, Bus, Flow, Found, Installer, LineKind, Mark, PluginOutcome, Probe, Remedy,
    Search, Step, PROBE_EVERY, SERVICE_WAIT,
};
use openwalkietalkie_app::i18n;
use openwalkietalkie_app::present::{finish_command, reason, test_line, Leftover, LoginItem};
use serde_json::{json, Value};
use std::path::Path;

const EN: Language = Language::English;
const ZH: Language = Language::SimplifiedChinese;

/// The installed app's own `deckd`.
const DECKD: &str = "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd";

/// What first run asked this Mac for, in order. It does none of it.
#[derive(Default)]
struct Fake {
    asked: Vec<&'static str>,
}

impl Installer for Fake {
    fn probe(&mut self) {
        self.asked.push("probe");
    }

    fn register_agent(&mut self) {
        self.asked.push("register_agent");
    }

    fn wait_service(&mut self) {
        self.asked.push("wait_service");
    }

    fn install_claude(&mut self) {
        self.asked.push("install_claude");
    }
}

impl Fake {
    /// Whether anything that changes this Mac was asked for: registering
    /// the service or adding the plugin.
    fn changed_anything(&self) -> bool {
        self.asked
            .iter()
            .any(|asked| *asked == "register_agent" || *asked == "install_claude")
    }
}

/// `seconds` into the boards' day.
fn at(seconds: u64) -> Ts {
    Ts::from_millis(1_794_355_200_000 + seconds * 1_000)
}

fn probe(json: &str) -> Probe {
    Probe::parse(json).expect("a probe's answer")
}

/// No deck, as `deckd probe --json` says it.
fn absent() -> Probe {
    probe(r#"{"present":false,"instances":[]}"#)
}

/// A deck on the cable.
fn on_usb() -> Probe {
    probe(r#"{"present":true,"instances":[{"bus":"usb"}]}"#)
}

/// Step 2 with the deck found by cable, nothing pressed.
fn found() -> Flow {
    let mut flow = Flow::new(Step::Adds, true);
    flow.seen(Found::Cable);
    flow
}

/// Step 2 after **Add These** and a registration macOS enabled, with no
/// service answering yet, at `since`.
fn waiting(since: Ts, fake: &mut Fake) -> Flow {
    let mut flow = found();
    flow.add_these(fake);
    flow.registered(Ok(LoginItem::Enabled), false, since, fake);
    flow
}

/// The service's advice beside every install of the plugin, which leaves
/// nothing undone.
fn advice() -> Vec<&'static str> {
    vec![
        "a session already open loads the plugin after `/reload-plugins`; new sessions load it at start",
        "For voice messages through Claude Channels, follow Integrations → Claude voice messages in \
         the app. Channels require per-session opt-in; cross-session inbox delivery remains the \
         fallback.",
    ]
}

/// An applied install's receipt, with these next steps.
fn receipt(next_steps: &[&str]) -> Value {
    json!({
        "target": "claude",
        "changed": ["/Users/owner/Library/Application Support/OpenWalkieTalkie/claude-marketplace/.claude-plugin/marketplace.json"],
        "backups": [],
        "notes": ["claude: Successfully added marketplace: openwalkietalkie"],
        "conflicts": [],
        "next_steps": next_steps,
    })
}

/// The two commands a service that found no `claude` leaves for the owner.
const LEFT: [&str; 2] = [
    "run: claude plugin marketplace add /Users/owner/Library/Application Support/OpenWalkieTalkie/claude-marketplace --scope user --json",
    "run: claude plugin install openwalkietalkie@openwalkietalkie --scope user --json",
];

#[test]
fn the_stepper_marks_the_steps_behind_the_open_one_done() {
    for at in Step::ALL {
        for step in Step::ALL {
            let expected = if step < at {
                Mark::Done
            } else if step == at {
                Mark::Current
            } else {
                Mark::Ahead
            };
            assert_eq!(mark(step, at), expected, "{step:?} while {at:?} is open");
        }
    }
    assert_eq!(Step::ALL.map(Step::number), [1, 2, 3, 4]);
    for step in Step::ALL {
        assert_eq!(Step::numbered(step.number()), step);
    }
    // Past either end, the nearest step.
    assert_eq!(Step::numbered(0), Step::Connect);
    assert_eq!(Step::numbered(9), Step::Integrations);
    i18n::install();
    assert_eq!(
        Step::ALL.map(|step| step_name(step, EN)),
        ["Connect", "What this adds", "Voice", "Integrations"]
    );
    // The last two are the pages' own names, as the sidebar says them.
    assert_eq!(
        Step::ALL.map(|step| step_name(step, ZH)),
        ["连接", "将添加的内容", "语音", "集成"]
    );
}

#[test]
fn the_probe_is_read_from_its_json_and_anything_else_is_no_answer() {
    assert_eq!(on_usb().found(), Some(Found::Cable));
    assert_eq!(on_usb().buses, [Bus::Usb]);
    let paired = probe(r#"{"present":true,"instances":[{"bus":"bluetooth"}]}"#);
    assert_eq!(paired.found(), Some(Found::Bluetooth));
    // Plugged in and paired: the cable is the link the service takes.
    let both = probe(r#"{"present":true,"instances":[{"bus":"bluetooth"},{"bus":"usb"}]}"#);
    assert_eq!(both.found(), Some(Found::Cable));
    assert_eq!(both.buses, [Bus::Bluetooth, Bus::Usb]);
    assert_eq!(absent().found(), None);
    assert!(!absent().present);
    // A bus nobody named, or none at all: found, linked somehow.
    let other = probe(r#"{"present":true,"instances":[{"bus":"unknown"}]}"#);
    assert_eq!(other.found(), Some(Found::Linked));
    assert_eq!(other.buses, [Bus::Unknown]);
    assert_eq!(
        probe(r#"{"present":true,"instances":[]}"#).found(),
        Some(Found::Linked)
    );
    // `present` is trusted over the list.
    assert_eq!(
        probe(r#"{"present":false,"instances":[{"bus":"usb"}]}"#).found(),
        None
    );
    // As printed, with its line feed, and after a line of something else.
    assert_eq!(
        Probe::parse("{\"present\":true,\"instances\":[{\"bus\":\"usb\"}]}\n"),
        Some(on_usb())
    );
    assert_eq!(
        Probe::parse("deckd: starting\n{\"present\":false,\"instances\":[]}\n"),
        Some(absent())
    );
    // Garbage: nothing, the plain-text form, a `deckd` too old for the verb,
    // JSON that is not the probe's.
    for garbage in [
        "",
        "no OpenWalkieTalkie found\n",
        "OpenWalkieTalkie present: usb\n",
        "error: unrecognized subcommand 'probe'\n\nUsage: deckd <COMMAND>\n",
        "{",
        "[]",
        r#"{"instances":[{"bus":"usb"}]}"#,
        r#"{"present":"yes","instances":[]}"#,
    ] {
        assert_eq!(Probe::parse(garbage), None, "{garbage:?}");
    }
}

#[test]
fn the_search_says_where_it_stands() {
    i18n::install();
    assert_eq!(
        search_status(Search::Looking, EN),
        ("Looking for your deck", Tone::Blue)
    );
    assert_eq!(
        search_status(Search::CannotLook, EN),
        ("This copy cannot look for a deck", Tone::Gray)
    );
    assert_eq!(
        search_status(Search::Found(Found::Cable), EN),
        ("Deck found, linked by cable", Tone::Green)
    );
    assert_eq!(
        search_status(Search::Found(Found::Bluetooth), EN),
        ("Deck found, linked over Bluetooth", Tone::Green)
    );
    assert_eq!(
        search_status(Search::Found(Found::Linked), EN),
        ("Deck found", Tone::Green)
    );
    assert_eq!(
        search_status(Search::Found(Found::Cable), ZH).0,
        "已找到设备，通过数据线连接"
    );
    assert_eq!(
        search_status(Search::Found(Found::Linked), ZH).0,
        "已找到设备"
    );
    // A deck a service has linked, by the transport it reports.
    assert_eq!(Found::of(Some(Transport::Usb)), Found::Cable);
    assert_eq!(Found::of(Some(Transport::Bluetooth)), Found::Bluetooth);
    assert_eq!(Found::of(None), Found::Linked);
}

#[test]
fn step_one_looks_every_two_seconds_and_a_deck_found_opens_step_two() {
    let mut fake = Fake::default();
    let mut flow = Flow::new(Step::Connect, true);
    assert_eq!(flow.search(), Search::Looking);
    // At once, then one look at a time.
    flow.tick(at(0), &mut fake);
    flow.tick(at(1), &mut fake);
    assert_eq!(fake.asked, ["probe"]);
    // The next look is due two seconds after the last answer, not before.
    flow.probed(Ok(absent()), at(1));
    flow.tick(at(2), &mut fake);
    assert_eq!(fake.asked, ["probe"]);
    flow.tick(at(1 + PROBE_EVERY.as_secs()), &mut fake);
    assert_eq!(fake.asked, ["probe", "probe"]);
    // A long while with no deck: looking, and nothing else.
    let mut now = 3;
    for _ in 0..60 {
        flow.probed(Ok(absent()), at(now));
        now += PROBE_EVERY.as_secs();
        flow.tick(at(now), &mut fake);
    }
    assert_eq!(flow.step(), Step::Connect);
    assert_eq!(flow.search(), Search::Looking);
    assert!(fake.asked.iter().all(|asked| *asked == "probe"));
    // Plugged in: step 2, the deck found by cable, and nothing pressed.
    flow.probed(Ok(on_usb()), at(now));
    assert_eq!(flow.step(), Step::Adds);
    assert_eq!(flow.search(), Search::Found(Found::Cable));
    assert_eq!(*flow.adding(), Adding::Idle);
    // Found, it looks no more, and still nothing is registered.
    let looks = fake.asked.len();
    for second in now..now + 30 {
        flow.tick(at(second), &mut fake);
    }
    assert_eq!(fake.asked.len(), looks);
    assert!(!fake.changed_anything());
}

#[test]
fn a_deck_a_running_service_has_linked_is_found_without_a_look() {
    let mut flow = Flow::new(Step::Connect, true);
    flow.seen(Found::Bluetooth);
    assert_eq!(flow.step(), Step::Adds);
    assert_eq!(flow.search(), Search::Found(Found::Bluetooth));
    // A look that answers after it changes nothing.
    flow.probed(Ok(absent()), at(5));
    assert_eq!(flow.search(), Search::Found(Found::Bluetooth));
}

#[test]
fn a_copy_that_cannot_look_says_so_and_goes_on_by_hand() {
    let mut fake = Fake::default();
    let mut flow = Flow::new(Step::Connect, false);
    assert_eq!(flow.search(), Search::CannotLook);
    assert!(!flow.may_add(), "nobody has vouched for a deck yet");
    for second in 0..10 {
        flow.tick(at(second), &mut fake);
    }
    assert!(fake.asked.is_empty());
    flow.advance();
    assert_eq!(flow.step(), Step::Adds);
    assert_eq!(flow.search(), Search::CannotLook);
    // Going on by hand says there is a deck: Add These may add.
    assert!(flow.may_add());
    flow.add_these(&mut fake);
    assert_eq!(fake.asked, ["register_agent"]);
    let mut fake = Fake::default();
    // A `deckd` that gives no answer is the same.
    let mut flow = Flow::new(Step::Connect, true);
    flow.tick(at(0), &mut fake);
    flow.probed(Err("deckd gave no probe answer".into()), at(1));
    assert_eq!(flow.search(), Search::CannotLook);
    // Looking, Continue is not offered: it does nothing.
    let mut flow = Flow::new(Step::Connect, true);
    flow.advance();
    assert_eq!(flow.step(), Step::Connect);
    assert!(!fake.changed_anything());
}

#[test]
fn set_up_opens_step_one_until_this_mac_has_seen_a_deck() {
    // Owner decision 7: with no deck ever seen, Set Up… starts where the
    // window looks for one; with one seen, at what this app adds.
    assert_eq!(set_up_step(false), 1);
    assert_eq!(set_up_step(true), 2);
    assert_eq!(Step::numbered(set_up_step(false)), Step::Connect);
    assert_eq!(Step::numbered(set_up_step(true)), Step::Adds);
}

#[test]
fn set_up_from_the_deck_page_opens_step_two_and_adds_nothing_until_the_deck_is_found() {
    let mut fake = Fake::default();
    let mut flow = Flow::new(Step::numbered(set_up_step(true)), true);
    assert_eq!(flow.step(), Step::Adds);
    assert_eq!(flow.search(), Search::Looking);
    // Still looking: Add These adds nothing, pressed as often as it is.
    assert!(!flow.may_add());
    flow.add_these(&mut fake);
    assert_eq!(*flow.adding(), Adding::Idle);
    flow.tick(at(0), &mut fake);
    assert_eq!(fake.asked, ["probe"]);
    flow.add_these(&mut fake);
    flow.probed(Ok(absent()), at(1));
    flow.add_these(&mut fake);
    assert!(!fake.changed_anything());
    // Found: now it may.
    flow.tick(at(1 + PROBE_EVERY.as_secs()), &mut fake);
    flow.probed(Ok(on_usb()), at(4));
    assert_eq!(flow.step(), Step::Adds);
    assert_eq!(flow.search(), Search::Found(Found::Cable));
    assert!(flow.may_add());
    flow.add_these(&mut fake);
    assert_eq!(fake.asked, ["probe", "probe", "register_agent"]);
}

#[test]
fn a_step_two_that_cannot_look_goes_back_to_step_one_to_go_on_by_hand() {
    // Set Up… on a copy with no `deckd` to look with: step 1, where the
    // owner vouches for the deck.
    let flow = Flow::new(Step::Adds, false);
    assert_eq!(flow.step(), Step::Connect);
    assert_eq!(flow.search(), Search::CannotLook);
    // A look that fails on step 2: the same.
    let mut fake = Fake::default();
    let mut flow = Flow::new(Step::Adds, true);
    flow.tick(at(0), &mut fake);
    flow.probed(Err("deckd did not answer within 10 s".into()), at(1));
    assert_eq!(flow.step(), Step::Connect);
    assert_eq!(flow.search(), Search::CannotLook);
    assert!(!flow.may_add());
    flow.advance();
    assert_eq!(flow.step(), Step::Adds);
    assert!(flow.may_add());
    // A deck found on step 2 stays found, and the step with it.
    let mut flow = found();
    flow.probed(Err("late".into()), at(2));
    assert_eq!((flow.step(), flow.search()), (Step::Adds, Search::Found(Found::Cable)));
}

#[test]
fn the_two_lines_that_send_the_owner_elsewhere_each_have_their_button() {
    assert_eq!(adding_remedy(&Adding::Approval), Some(Remedy::LoginItems));
    assert_eq!(adding_remedy(&Adding::ServiceFailed), Some(Remedy::OpenService));
    for other in [
        Adding::Idle,
        Adding::Registering,
        Adding::Waiting { since: at(0) },
        Adding::Installing,
        Adding::Added,
        Adding::OneMoreStep {
            command: format!("{DECKD} integrations install claude"),
        },
        Adding::NotAdded("hold_s leaves no headroom".into()),
    ] {
        assert_eq!(adding_remedy(&other), None, "{other:?}");
    }
    i18n::install();
    for button in ["Open Login Items", "Open Service"] {
        assert!(deck_gui_core::i18n::has_entry(button), "{button}");
        assert_ne!(Language::Japanese.text(button), button);
    }
    assert_eq!(ZH.text("Open Login Items"), "打开登录项");
}

#[test]
fn step_three_says_where_speech_goes_conol_chats_included() {
    i18n::install();
    let lead = "Speech is recognised and spoken on this Mac unless you choose otherwise.\nWhile \
                the Conol switch below is on, what you say to Conol chats is heard by Conol's \
                cloud.";
    assert!(i18n::firstrun::STRINGS.iter().any(|row| row.0 == lead));
    // Every language says it on two lines, and names Conol's cloud.
    for language in Language::ALL {
        let said = language.text(lead);
        assert_eq!(said.lines().count(), 2, "{language:?}: {said}");
        assert!(said.contains("Conol"), "{language:?}: {said}");
    }
    // The old sentence, which said nothing of Conol, is gone.
    assert!(!deck_gui_core::i18n::has_entry(
        "Speech is recognised and spoken on this Mac unless you choose otherwise."
    ));
}

#[test]
fn add_these_registers_the_service_waits_for_it_and_adds_the_plugin() {
    let mut fake = Fake::default();
    let mut flow = found();
    flow.add_these(&mut fake);
    assert_eq!(fake.asked, ["register_agent"]);
    assert_eq!(*flow.adding(), Adding::Registering);
    // Pressed again while it is under way: nothing more.
    flow.add_these(&mut fake);
    assert_eq!(fake.asked, ["register_agent"]);
    flow.registered(Ok(LoginItem::Enabled), false, at(10), &mut fake);
    assert_eq!(fake.asked, ["register_agent", "wait_service"]);
    assert_eq!(*flow.adding(), Adding::Waiting { since: at(10) });
    // While waiting it does not look for the deck.
    flow.tick(at(29), &mut fake);
    assert_eq!(*flow.adding(), Adding::Waiting { since: at(10) });
    flow.service(false, &mut fake);
    assert_eq!(fake.asked, ["register_agent", "wait_service"]);
    // The service answers: the plugin.
    flow.service(true, &mut fake);
    assert_eq!(
        fake.asked,
        ["register_agent", "wait_service", "install_claude"]
    );
    assert_eq!(*flow.adding(), Adding::Installing);
    // Another event while it is added asks nothing again.
    flow.service(true, &mut fake);
    assert_eq!(fake.asked.len(), 3);
    // Added: step 3 by itself.
    flow.plugin(PluginOutcome::Added);
    assert_eq!(flow.step(), Step::Voice);
    assert_eq!(*flow.adding(), Adding::Added);
    flow.advance();
    assert_eq!(flow.step(), Step::Integrations);
    flow.finish();
    assert!(flow.ended());
    flow.tick(at(60), &mut fake);
    assert_eq!(fake.asked.len(), 3);
}

#[test]
fn a_service_that_does_not_answer_in_twenty_seconds_did_not_start_and_add_these_tries_again() {
    let mut fake = Fake::default();
    let mut flow = waiting(at(10), &mut fake);
    flow.tick(at(10 + SERVICE_WAIT.as_secs() - 1), &mut fake);
    assert_eq!(*flow.adding(), Adding::Waiting { since: at(10) });
    flow.tick(at(10 + SERVICE_WAIT.as_secs()), &mut fake);
    assert_eq!(*flow.adding(), Adding::ServiceFailed);
    assert_eq!(fake.asked, ["register_agent", "wait_service"]);
    // **Add These** again registers again.
    flow.add_these(&mut fake);
    assert_eq!(*flow.adding(), Adding::Registering);
    assert_eq!(
        fake.asked,
        ["register_agent", "wait_service", "register_agent"]
    );
    // One that answers late, after it was given up on, is still used.
    let mut fake = Fake::default();
    let mut flow = waiting(at(0), &mut fake);
    flow.tick(at(SERVICE_WAIT.as_secs()), &mut fake);
    flow.service(true, &mut fake);
    assert_eq!(*flow.adding(), Adding::Installing);
    assert_eq!(fake.asked.last(), Some(&"install_claude"));
}

#[test]
fn a_service_held_for_the_owners_approval_is_waited_for_as_long_as_that_takes() {
    let mut fake = Fake::default();
    let mut flow = found();
    flow.add_these(&mut fake);
    flow.registered(Ok(LoginItem::RequiresApproval), false, at(0), &mut fake);
    assert_eq!(*flow.adding(), Adding::Approval);
    assert_eq!(fake.asked, ["register_agent", "wait_service"]);
    flow.tick(at(10 * SERVICE_WAIT.as_secs()), &mut fake);
    assert_eq!(*flow.adding(), Adding::Approval);
    // Allowed in Login Items: the service starts and answers.
    flow.service(true, &mut fake);
    assert_eq!(*flow.adding(), Adding::Installing);
}

#[test]
fn a_registration_that_did_not_take_carries_on_only_with_a_service_already_answering() {
    for answer in [
        Err("a window on DECKD_DATA_DIR changes nothing outside its own window".to_string()),
        Ok(LoginItem::NotRegistered),
        Ok(LoginItem::NotFound),
        Ok(LoginItem::Unknown(7)),
    ] {
        let mut fake = Fake::default();
        let mut flow = found();
        flow.add_these(&mut fake);
        flow.registered(answer.clone(), false, at(0), &mut fake);
        assert_eq!(*flow.adding(), Adding::ServiceFailed, "{answer:?}");
        assert_eq!(fake.asked, ["register_agent"], "{answer:?}");
        // A service that answers already is the one to add the plugin to.
        let mut fake = Fake::default();
        let mut flow = found();
        flow.add_these(&mut fake);
        flow.registered(answer.clone(), true, at(0), &mut fake);
        assert_eq!(*flow.adding(), Adding::Installing, "{answer:?}");
        assert_eq!(fake.asked, ["register_agent", "install_claude"]);
    }
}

#[test]
fn a_plugin_left_unfinished_keeps_step_two_until_continue() {
    let command = format!("{DECKD} integrations install claude");
    for outcome in [
        PluginOutcome::OneMoreStep {
            command: command.clone(),
        },
        PluginOutcome::NotAdded("the settings file holds a fatal key".into()),
    ] {
        let mut fake = Fake::default();
        let mut flow = waiting(at(0), &mut fake);
        // An answer to nothing asked is not taken.
        flow.plugin(outcome.clone());
        assert!(matches!(flow.adding(), Adding::Waiting { .. }));
        flow.service(true, &mut fake);
        flow.plugin(outcome.clone());
        assert_eq!(flow.step(), Step::Adds, "{outcome:?}");
        assert!(flow.adding().settled());
        assert!(!flow.adding().busy());
        // It is not tried again by itself, nor by **Add These**.
        flow.service(true, &mut fake);
        flow.add_these(&mut fake);
        assert_eq!(fake.asked.len(), 3);
        flow.advance();
        assert_eq!(flow.step(), Step::Voice);
    }
}

#[test]
fn not_now_registers_nothing() {
    // From step 1 through to step 2, then Not Now.
    let mut fake = Fake::default();
    let mut flow = Flow::new(Step::Connect, true);
    flow.tick(at(0), &mut fake);
    flow.probed(Ok(on_usb()), at(1));
    flow.not_now();
    assert!(flow.ended());
    for second in 2..40 {
        flow.tick(at(second), &mut fake);
        flow.service(true, &mut fake);
    }
    flow.add_these(&mut fake);
    flow.advance();
    assert_eq!(fake.asked, ["probe"]);
    assert_eq!(flow.step(), Step::Adds);
    // From the Deck page's Set Up…, before the deck was even found.
    let mut fake = Fake::default();
    let mut flow = Flow::new(Step::Adds, true);
    flow.not_now();
    flow.tick(at(0), &mut fake);
    flow.add_these(&mut fake);
    assert!(fake.asked.is_empty());
}

#[test]
fn adding_is_finished_only_with_nothing_left_to_run_and_the_plugin_read_back_installed() {
    let deckd = Some(Path::new(DECKD));
    let finish = format!("{DECKD} integrations install claude");
    // The service found no `claude` on its PATH: it wrote the files and
    // left the registration as next steps. Not finished, whatever the
    // status said.
    let mut steps = advice();
    steps.extend(LEFT);
    let unfinished = receipt(&steps);
    for status in [Some(&Status::Outdated), Some(&Status::Installed), None] {
        assert_eq!(
            plugin_outcome(Ok(&unfinished), status, deckd),
            PluginOutcome::OneMoreStep {
                command: finish.clone()
            },
            "{status:?}"
        );
    }
    // It ran them: the advice it gives with every install leaves nothing
    // undone, once the plugin reads back as installed.
    let finished = receipt(&advice());
    assert_eq!(
        plugin_outcome(Ok(&finished), Some(&Status::Installed), deckd),
        PluginOutcome::Added
    );
    // Read back as anything else: not finished.
    for status in [
        Status::Outdated,
        Status::Absent,
        Status::Unknown("half".into()),
    ] {
        assert_eq!(
            plugin_outcome(Ok(&finished), Some(&status), deckd),
            PluginOutcome::OneMoreStep {
                command: finish.clone()
            },
            "{status:?}"
        );
    }
    // A read back that failed takes the receipt at its word, as the
    // Integrations page does: it left nothing undone.
    assert_eq!(
        plugin_outcome(Ok(&finished), None, deckd),
        PluginOutcome::Added
    );
    // A command that failed is not done either.
    let warned = receipt(&["WARNING: `claude plugin uninstall …` failed (exit 1)"]);
    assert!(matches!(
        plugin_outcome(Ok(&warned), Some(&Status::Installed), deckd),
        PluginOutcome::OneMoreStep { .. }
    ));
    // Refused: in the installer's own words.
    let refused = ClientError::Rpc {
        code: ApiError::CONFLICT,
        message: "conflict".into(),
        data: Some(json!({"why": "settings.json holds the fatal key hooks"})),
    };
    assert_eq!(
        plugin_outcome(Err(&refused), None, deckd),
        PluginOutcome::NotAdded("settings.json holds the fatal key hooks".into())
    );
}

#[test]
fn the_command_left_is_one_a_terminal_runs() {
    let mut steps = advice();
    steps.extend(LEFT);
    steps.push("WARNING: `claude plugin marketplace remove openwalkietalkie` failed (exit 1)");
    let leftover = Leftover::of(&receipt(&steps));
    assert_eq!(
        leftover.commands,
        [
            LEFT[0].trim_start_matches("run: "),
            LEFT[1].trim_start_matches("run: "),
        ]
    );
    assert_eq!(
        leftover.warnings,
        ["WARNING: `claude plugin marketplace remove openwalkietalkie` failed (exit 1)"]
    );
    assert!(Leftover::of(&receipt(&advice())).is_empty());
    // This app's own `deckd`, quoted when its path would split.
    let install = |deckd: Option<&Path>, commands: &[String]| {
        finish_command(deckd, Verb::Install, "claude", commands)
    };
    assert_eq!(
        install(Some(Path::new(DECKD)), &leftover.commands),
        "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd integrations install claude"
    );
    assert_eq!(
        install(
            Some(Path::new(
                "/Volumes/Work Disk/OpenWalkieTalkie.app/Contents/MacOS/deckd"
            )),
            &leftover.commands
        ),
        "'/Volumes/Work Disk/OpenWalkieTalkie.app/Contents/MacOS/deckd' integrations install claude"
    );
    // With no `deckd` beside this copy, the service's own commands, one
    // after the other; with none of those either, the verb itself.
    assert_eq!(
        install(None, &leftover.commands),
        format!(
            "{} && {}",
            LEFT[0].trim_start_matches("run: "),
            LEFT[1].trim_start_matches("run: ")
        )
    );
    assert_eq!(install(None, &[]), "deckd integrations install claude");
}

#[test]
fn the_plugin_is_read_back_from_its_own_row_and_a_refusal_in_its_own_words() {
    let status = json!({
        "targets": [
            {"target": "codex", "status": "installed"},
            {"target": "claude", "status": "outdated", "detail": "not registered"},
        ],
        "warnings": [],
    });
    assert_eq!(claude_status(&status), Some(Status::Outdated));
    assert_eq!(claude_status(&json!({"targets": []})), None);
    assert_eq!(claude_status(&json!({})), None);
    let refused = ClientError::Rpc {
        code: ApiError::CONFLICT,
        message: "conflict".into(),
        data: Some(json!({"why": "hold_s leaves no headroom"})),
    };
    assert_eq!(reason(&refused), "hold_s leaves no headroom");
    let bare = ClientError::Rpc {
        code: ApiError::CONFLICT,
        message: "conflict".into(),
        data: None,
    };
    assert_eq!(reason(&bare), bare.to_string());
    assert_eq!(reason(&ClientError::Disconnected), "disconnected");
}

#[test]
fn step_two_says_how_adding_goes() {
    i18n::install();
    let line = |adding: Adding| adding_line(&adding, EN);
    assert_eq!(line(Adding::Idle), None);
    assert_eq!(line(Adding::Added), None);
    assert_eq!(
        line(Adding::Registering),
        Some(("Adding the background service…".into(), LineKind::Busy))
    );
    assert_eq!(
        line(Adding::Waiting { since: at(0) }),
        Some(("Waiting for the service…".into(), LineKind::Busy))
    );
    assert_eq!(
        line(Adding::Installing),
        Some(("Adding the Claude Code plugin…".into(), LineKind::Busy))
    );
    assert_eq!(
        line(Adding::Approval),
        Some((
            "macOS is waiting for your approval in System Settings > General > Login Items.".into(),
            LineKind::Turn
        ))
    );
    assert_eq!(
        line(Adding::ServiceFailed),
        Some((
            "The service did not start. Open Service to see why.".into(),
            LineKind::Problem
        ))
    );
    assert_eq!(
        line(Adding::OneMoreStep {
            command: format!("{DECKD} integrations install claude")
        }),
        Some((
            "One more step: run this command in a terminal.".into(),
            LineKind::Turn
        ))
    );
    assert_eq!(
        line(Adding::NotAdded("hold_s leaves no headroom".into())),
        Some((
            "The plugin was not added: hold_s leaves no headroom".into(),
            LineKind::Problem
        ))
    );
    assert_eq!(
        adding_line(&Adding::NotAdded("x".into()), ZH),
        Some(("未能添加插件：x".into(), LineKind::Problem))
    );
    // Under way, Add These waits; over, Continue takes its place.
    assert!(Adding::Approval.busy() && !Adding::Approval.settled());
    assert!(!Adding::ServiceFailed.busy() && !Adding::ServiceFailed.settled());
    assert!(!Adding::Idle.busy() && !Adding::Added.settled());
}

#[test]
fn step_four_says_how_the_last_test_question_went_in_the_integrations_pages_words() {
    i18n::install();
    // Step 4 shows the Integrations page's test question with that page's
    // line (`present::test_line`, every state of it read in
    // `tests/integrations_page.rs`): here the service's own reply, as the
    // harness stages step 4 from it, in every language.
    let fixture: Value = serde_json::from_str(include_str!(
        "../../deck-gui-core/tests/fixtures/daemon/care_test_status.json"
    ))
    .unwrap();
    let status = TestStatus::parse(&fixture);
    for (language, said) in [
        (
            EN,
            "The last test question arrived on the deck and was answered there after 3 s.",
        ),
        (ZH, "上次的测试提问已送达设备，3 秒后在设备上得到回答。"),
        (
            Language::TraditionalChinese,
            "上次的測試提問已送達裝置，並在 3 秒後於裝置上獲得回答。",
        ),
        (
            Language::Japanese,
            "前回のテスト質問はデバイスに届き、3 秒後にデバイスで回答されました。",
        ),
    ] {
        let line = test_line(status.question.as_ref(), language).map(|(line, _)| line);
        assert_eq!(line.as_deref(), Some(said), "{language:?}");
    }
    // Nothing before the first test.
    assert_eq!(test_line(None, EN), None);
    // A service older than the test question: the step says so instead.
    let older = "This needs a newer background service.";
    for language in [ZH, Language::TraditionalChinese, Language::Japanese] {
        assert_ne!(language.text(older), older, "{language:?}");
    }
}

#[test]
fn every_row_the_page_adds_is_the_only_row_of_its_key() {
    i18n::install();
    let all = deck_gui_core::i18n::rows();
    for &(english, ..) in i18n::firstrun::STRINGS {
        let rows = all.iter().filter(|row| row.0 == english).count();
        assert_eq!(rows, 1, "{english:?} is in another table too");
    }
    // The sheet's 34 rows, copied as written (step 3's sentence with a
    // second line about Conol's chats), and the page's own `Deck found`
    // and `Open Login Items`.
    assert_eq!(i18n::firstrun::STRINGS.len(), 36);
}

#[test]
fn every_word_the_page_says_has_a_row() {
    let source = include_str!("../src/firstrun.rs");
    assert_eq!(i18n::missing_rows(source), Vec::<String>::new());
    // Pinned, so a lookup the scanner stopped seeing fails here rather than
    // passing as a page with less to translate.
    assert_eq!(i18n::literals(source).len(), 54);
}

#[test]
fn a_fresh_test_services_own_installer_status_says_the_plugin_is_not_added_yet() {
    i18n::install();
    // `deck-gui-core`'s recorded fixtures: the installer's answer with and
    // without its probes (first run reads it without), and a test question
    // sent with no deck linked, read back.
    let quick: Value = serde_json::from_str(include_str!(
        "../../deck-gui-core/tests/fixtures/daemon/recorded/integrations.status.no_detect.json"
    ))
    .expect("JSON");
    let probed: Value = serde_json::from_str(include_str!(
        "../../deck-gui-core/tests/fixtures/daemon/recorded/integrations.status.json"
    ))
    .expect("JSON");
    assert_eq!(claude_status(&quick), Some(Status::Absent));
    assert_eq!(claude_status(&probed), Some(Status::Absent));
    let after: Value = serde_json::from_str(include_str!(
        "../../deck-gui-core/tests/fixtures/daemon/recorded/care.test.status.after_question.json"
    ))
    .expect("JSON");
    let tests = TestStatus::parse(&after);
    assert_eq!(
        test_line(tests.question.as_ref(), EN).map(|(line, _)| line),
        Some("The deck is not linked, so no test question was sent.".to_string())
    );
}
