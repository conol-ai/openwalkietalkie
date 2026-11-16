//! `deckd grokbot …`: Grok Bot on the deck (`crate::grokbot`).
//!
//! **A bot's verbs** are the ones Grok Bot runs on this Mac for a bot
//! ("Execution on this computer" in its settings): `say` puts the bot's
//! reply on the deck and reads it out, `ask` puts a question card there and
//! prints the operator's answer, `state` says whether the bot is working.
//! They ask for `agent`, which a process an agent started still has (the
//! ancestry cap counts Grok Bot's own processes as an agent's, §4.2).
//!
//! **The owner's verbs** set a bot up: `add` (its name and its routine's
//! webhook; the key is typed, never put on the command line), `remove`,
//! `list`, `test`, `routine` (the instruction to give the routine) and
//! `prompt` (the message that has a bot make that routine itself). They ask
//! for `admin`, which a process an agent started is refused.
use super::{ask, finish, print_json, text, Service, Stop, ADMIN};
use clap::Subcommand;
use serde_json::{json, Value};
use std::io::Write;
use std::time::Duration;

/// What a bot's verbs ask for.
const AGENT: &[&str] = &["agent"];
/// How long past the question's own time `ask` waits for the reply.
const ASK_GRACE: Duration = Duration::from_secs(15);

#[derive(Subcommand, Debug)]
pub enum GrokbotCmd {
    /// Put a bot's reply on the deck and read it out (`grokbot.say`). `-`
    /// reads the reply from stdin.
    Say {
        /// The bot: its id or its name.
        #[arg(long)]
        bot: String,
        /// Show the reply without reading it out.
        #[arg(long)]
        quiet: bool,
        #[arg(long)]
        json: bool,
        /// The reply.
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        text: Vec<String>,
    },
    /// Ask the operator on the deck and print the answer (`grokbot.ask`):
    /// the chosen choice, or what they said. Exits 1 with "(no answer)" or
    /// "(snoozed)" when there is none.
    Ask {
        #[arg(long)]
        bot: String,
        /// A choice the deck offers (up to four, each up to 32 bytes).
        #[arg(long = "choice")]
        choices: Vec<String>,
        /// Seconds to wait for the operator (default 300, at most 600).
        #[arg(long)]
        timeout: Option<u64>,
        /// Take a choice only: no spoken answer.
        #[arg(long)]
        choices_only: bool,
        #[arg(long)]
        json: bool,
        /// The question.
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        question: Vec<String>,
    },
    /// Say whether a bot is `working`, `idle`, `ready` or in `error`
    /// (`grokbot.state`).
    State {
        #[arg(long)]
        bot: String,
        state: String,
        /// What it is doing, for the deck's roster.
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Set a bot up, or change one: its name on the deck and its routine's
    /// webhook ("POST to"). The routine's key is read without echo, or from
    /// stdin when it is not a terminal.
    Add {
        /// The bot's name, as the deck shows it.
        name: String,
        /// The routine's webhook URL.
        #[arg(long)]
        webhook: String,
        /// The bot's id (default: from its name).
        #[arg(long)]
        id: Option<String>,
        /// Keep the key the bot has rather than read a new one.
        #[arg(long)]
        keep_key: bool,
        #[arg(long)]
        json: bool,
    },
    /// Take a bot off the deck and forget its webhook and its key.
    Remove {
        bot: String,
        #[arg(long)]
        json: bool,
    },
    /// The bots: each one's webhook host, whether it has its key, and how
    /// its last call went.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Post a test message to a bot's webhook (`grokbot.test`).
    Test {
        bot: String,
        #[arg(long)]
        json: bool,
    },
    /// Print the instruction to give a bot's webhook routine in Grok Bot.
    Routine { bot: String },
    /// Print the message to send a bot in Grok Bot's chat: the bot makes
    /// its own webhook routine, with the instruction `routine` prints. Then
    /// `add` takes the routine's URL ("POST to") and key.
    Prompt {
        /// The bot's name on the deck, or a bot that is set up.
        bot: String,
    },
}

/// `deckd grokbot …`.
pub fn run(
    service: &dyn Service,
    cmd: GrokbotCmd,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, String> {
    finish(grokbot(service, cmd, out, err))
}

fn grokbot(
    service: &dyn Service,
    cmd: GrokbotCmd,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, Stop> {
    match cmd {
        GrokbotCmd::Say {
            bot,
            quiet,
            json,
            text: words,
        } => {
            let text = words_or_stdin(&words)?;
            let reply = ask(
                service,
                AGENT,
                "grokbot.say",
                json!({"bot": bot, "text": text, "speak": !quiet}),
                err,
            )?;
            if json {
                print_json(out, &reply)?;
            } else {
                writeln!(out, "on the deck")?;
            }
            Ok(0)
        }
        GrokbotCmd::Ask {
            bot,
            choices,
            timeout,
            choices_only,
            json,
            question,
        } => {
            let question = words_or_stdin(&question)?;
            let wait = Duration::from_secs(
                timeout
                    .unwrap_or(crate::grokbot::ASK_DEFAULT_S)
                    .clamp(5, crate::grokbot::ASK_MAX_S),
            );
            let mut params = json!({
                "bot": bot,
                "question": question,
                "choices": choices,
                "free_text": !choices_only,
                "timeout_s": wait.as_secs(),
            });
            if choices.is_empty() {
                params["free_text"] = Value::Bool(true);
            }
            let reply = service
                .call_for(AGENT, "grokbot.ask", params, wait + ASK_GRACE)
                .map_err(|failure| Stop::Failed(crate::cli::report(failure, err)))?;
            if json {
                print_json(out, &reply)?;
                return Ok(i32::from(reply.get("answer").is_none()));
            }
            let (line, code) = answer_line(&reply);
            writeln!(out, "{line}")?;
            Ok(code)
        }
        GrokbotCmd::State {
            bot,
            state,
            title,
            json,
        } => {
            let mut params = json!({"bot": bot, "state": state});
            if let Some(title) = title {
                params["title"] = Value::String(title);
            }
            let reply = ask(service, AGENT, "grokbot.state", params, err)?;
            if json {
                print_json(out, &reply)?;
            } else {
                writeln!(out, "on the deck")?;
            }
            Ok(0)
        }
        GrokbotCmd::Add {
            name,
            webhook,
            id,
            keep_key,
            json,
        } => {
            let mut params = json!({"name": name, "webhook": webhook});
            if let Some(id) = id {
                params["id"] = Value::String(id);
            }
            if !keep_key {
                let key = service
                    .read_key("Paste the routine's key (Authorization: Bearer …), then Return: ")
                    .map_err(Stop::Refused)?;
                // A pasted header is taken for the key it carries.
                let key = key
                    .trim()
                    .trim_start_matches("Authorization:")
                    .trim()
                    .trim_start_matches("Bearer")
                    .trim()
                    .to_string();
                params["key"] = Value::String(key);
            }
            let reply = ask(service, ADMIN, "grokbot.add", params, err)?;
            if json {
                print_json(out, &reply)?;
                return Ok(0);
            }
            let id = text(&reply, "id");
            writeln!(
                out,
                "{} is set up as `{id}`{}",
                text(&reply, "name"),
                if reply["has_key"].as_bool() == Some(true) {
                    ""
                } else {
                    ", with no key: add it again with its key"
                }
            )?;
            writeln!(
                out,
                "Give its routine this instruction (`deckd grokbot routine {id}` prints it again):"
            )?;
            let status = ask(service, ADMIN, "grokbot.status", json!({}), err)?;
            if let Some(routine) = find_bot(&status, id).and_then(|bot| bot["routine"].as_str()) {
                writeln!(out, "\n{routine}")?;
            }
            Ok(0)
        }
        GrokbotCmd::Remove { bot, json } => {
            let reply = ask(service, ADMIN, "grokbot.remove", json!({"bot": bot}), err)?;
            if json {
                print_json(out, &reply)?;
            } else {
                writeln!(out, "{} is off the deck, its webhook and key forgotten", text(&reply, "id"))?;
            }
            Ok(0)
        }
        GrokbotCmd::List { json } => {
            let status = ask(service, ADMIN, "grokbot.status", json!({}), err)?;
            if json {
                print_json(out, &status)?;
            } else {
                for line in list_lines(&status) {
                    writeln!(out, "{line}")?;
                }
            }
            Ok(0)
        }
        GrokbotCmd::Test { bot, json } => {
            let reply = ask(service, ADMIN, "grokbot.test", json!({"bot": bot}), err)?;
            if json {
                print_json(out, &reply)?;
            }
            if reply["ok"].as_bool() == Some(true) {
                if !json {
                    writeln!(
                        out,
                        "Grok Bot took the test (HTTP {}): the bot's reply comes to the deck",
                        reply["status"].as_u64().unwrap_or(0)
                    )?;
                }
                Ok(0)
            } else {
                if !json {
                    writeln!(err, "deckd: {}", refusal_line(&reply))?;
                }
                Ok(1)
            }
        }
        GrokbotCmd::Routine { bot } => {
            let status = ask(service, ADMIN, "grokbot.status", json!({}), err)?;
            match set_up_bot(&status, &bot).and_then(|b| b["routine"].as_str()) {
                Some(routine) => {
                    writeln!(out, "{routine}")?;
                    Ok(0)
                }
                None => Err(Stop::Refused(format!(
                    "no bot `{bot}` is set up (deckd grokbot list shows them)"
                ))),
            }
        }
        GrokbotCmd::Prompt { bot } => {
            let status = ask(service, ADMIN, "grokbot.status", json!({}), err)?;
            // A bot that is set up keeps its id; a new name gets the id the
            // service will give it when it is added.
            let (name, id) = match set_up_bot(&status, &bot) {
                Some(found) => (text(found, "name").to_string(), text(found, "id").to_string()),
                None => {
                    let name = bot.trim();
                    if let Some(problem) = crate::grokbot::name_problem(name) {
                        return Err(Stop::Refused(problem.to_string()));
                    }
                    (name.to_string(), crate::grokbot::slug(name))
                }
            };
            writeln!(out, "{}", crate::grokbot::setup_prompt(&name, &id, &invocation_of(&status)))?;
            Ok(0)
        }
    }
}

/// The bot that is set up under `bot`: its id, or its name ignoring case.
fn set_up_bot<'a>(status: &'a Value, bot: &str) -> Option<&'a Value> {
    find_bot(status, bot).or_else(|| {
        status["bots"].as_array()?.iter().find(|b| {
            b["name"]
                .as_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(bot.trim()))
        })
    })
}

/// The words of a reply or a question, or stdin for `-`.
fn words_or_stdin(words: &[String]) -> Result<String, Stop> {
    if words.len() == 1 && words[0] == "-" {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(Stop::Refused("stdin was empty".into()));
        }
        return Ok(text);
    }
    Ok(words.join(" "))
}

fn find_bot<'a>(status: &'a Value, id: &str) -> Option<&'a Value> {
    status["bots"]
        .as_array()?
        .iter()
        .find(|bot| bot["id"].as_str() == Some(id.trim()))
}

/// What `ask` prints and exits with: the answer and 0, or why there is
/// none and 1.
#[must_use]
pub fn answer_line(reply: &Value) -> (String, i32) {
    if let Some(answer) = reply.get("answer") {
        let said = answer["label"]
            .as_str()
            .or_else(|| answer["text"].as_str())
            .unwrap_or("")
            .to_string();
        return (said, 0);
    }
    if reply["snoozed"].as_bool() == Some(true) {
        return ("(snoozed)".into(), 1);
    }
    ("(no answer)".into(), 1)
}

/// Why a test call was not taken, in a sentence.
#[must_use]
pub fn refusal_line(reply: &Value) -> String {
    match reply["status"].as_u64() {
        Some(401 | 403) => format!(
            "Grok Bot refused the key (HTTP {}): copy the routine's key again",
            reply["status"]
        ),
        Some(404) => "Grok Bot does not know this webhook (HTTP 404): copy the routine's URL again"
            .to_string(),
        Some(status) => format!(
            "Grok Bot did not start a run (HTTP {status}): is the routine paused?"
        ),
        None => format!(
            "the webhook did not answer: {}",
            reply["why"].as_str().unwrap_or("no answer")
        ),
    }
}

/// `list`'s lines.
#[must_use]
pub fn list_lines(status: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    let store = match text(status, "key_store") {
        "keychain" => "keys in the Keychain",
        "credential_manager" => "keys in Windows Credential Manager",
        "file" => "keys in grokbot-keys.toml",
        other => other,
    };
    lines.push(format!(
        "Grok Bot: {} ({store})",
        if status["enabled"].as_bool() == Some(false) {
            "off"
        } else if status["live"].as_bool() == Some(true) {
            "on"
        } else {
            "on, waiting for a deck"
        }
    ));
    let bots = status["bots"].as_array().cloned().unwrap_or_default();
    if bots.is_empty() {
        lines.push(
            "  no bot is set up: deckd grokbot prompt <name>, send that to the bot, then \
             deckd grokbot add <name> --webhook <url>"
                .into(),
        );
    }
    for bot in &bots {
        let set_up = bot["set_up"].as_bool() == Some(true);
        let last = bot["last"]
            .as_object()
            .map(|last| match last.get("status").and_then(Value::as_u64) {
                Some(status) => format!("last call: HTTP {status}"),
                None => "last call: no answer".into(),
            })
            .unwrap_or_else(|| "no call yet".into());
        if set_up {
            lines.push(format!(
                "  {}  {}  {}  {}  {}  {last}",
                text(bot, "id"),
                text(bot, "name"),
                text(bot, "webhook_host"),
                if bot["has_key"].as_bool() == Some(true) { "key: yes" } else { "key: MISSING" },
                if bot["on_deck"].as_bool() == Some(true) { "on the deck" } else { "not on the deck" },
            ));
        } else {
            lines.push(format!(
                "  {}  {}  not set up: it talks to the deck, the deck cannot talk to it",
                text(bot, "id"),
                text(bot, "name"),
            ));
        }
    }
    if status["invocation"].is_string() || status["deckd"].is_string() {
        lines.push(format!(
            "A bot answers with: {} grokbot say --bot <id> '<reply>'",
            invocation_of(status)
        ));
    }
    lines
}

/// How the service's bots start their commands: as it says
/// (`grokbot.status`'s `invocation`, which names its data directory when a
/// bare `deckd` would not find it), else its `deckd` alone.
fn invocation_of(status: &Value) -> String {
    status["invocation"].as_str().map_or_else(
        || {
            let deckd = status["deckd"].as_str().unwrap_or("deckd");
            crate::grokbot::invocation(std::path::Path::new(deckd), None)
        },
        str::to_string,
    )
}

#[cfg(test)]
mod tests {
    use super::super::testkit::Script;
    use super::*;

    fn run_with(script: &Script, cmd: GrokbotCmd) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(script, cmd, &mut out, &mut err).unwrap_or(1);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn a_bot_says_and_asks_with_the_agent_scope() {
        let script = Script::new(vec![Ok(json!({"bot": "research", "agent": "ag_1", "spoken": true}))]);
        let (code, out, _) = run_with(
            &script,
            GrokbotCmd::Say {
                bot: "research".into(),
                quiet: false,
                json: false,
                text: vec!["Found".into(), "three flights.".into()],
            },
        );
        assert_eq!((code, out.as_str()), (0, "on the deck\n"));
        let call = script.call(0);
        assert_eq!(call.scopes, ["agent"]);
        assert_eq!(call.method, "grokbot.say");
        assert_eq!(call.params, json!({"bot": "research", "text": "Found three flights.", "speak": true}));

        let script = Script::new(vec![Ok(json!({"answer": {"choice": 1, "label": "No", "text": null}}))]);
        let (code, out, _) = run_with(
            &script,
            GrokbotCmd::Ask {
                bot: "research".into(),
                choices: vec!["Yes".into(), "No".into()],
                timeout: Some(9000),
                choices_only: true,
                json: false,
                question: vec!["Book it?".into()],
            },
        );
        assert_eq!((code, out.as_str()), (0, "No\n"));
        let call = script.call(0);
        assert_eq!(call.scopes, ["agent"]);
        assert_eq!(
            call.params,
            json!({"bot": "research", "question": "Book it?", "choices": ["Yes", "No"],
                   "free_text": false, "timeout_s": 600})
        );
    }

    #[test]
    fn no_answer_exits_one_and_says_why() {
        assert_eq!(answer_line(&json!({"timeout": true})), ("(no answer)".into(), 1));
        assert_eq!(answer_line(&json!({"snoozed": true})), ("(snoozed)".into(), 1));
        assert_eq!(
            answer_line(&json!({"answer": {"choice": null, "label": null, "text": "the cheap one"}})),
            ("the cheap one".into(), 0)
        );
    }

    #[test]
    fn add_reads_the_key_and_takes_a_pasted_header_for_it() {
        let script = Script::new(vec![
            Ok(json!({"id": "research", "name": "Research", "has_key": true})),
            Ok(json!({"bots": [{"id": "research", "routine": "Each run of this routine is…"}]})),
        ]);
        *script.key.borrow_mut() = Some("Authorization: Bearer sk-123 ".into());
        let (code, out, _) = run_with(
            &script,
            GrokbotCmd::Add {
                name: "Research".into(),
                webhook: "https://example.com/hook".into(),
                id: None,
                keep_key: false,
                json: false,
            },
        );
        assert_eq!(code, 0);
        assert!(out.starts_with("Research is set up as `research`\n"), "{out}");
        assert!(out.contains("Each run of this routine is…"));
        let call = script.call(0);
        assert_eq!(call.scopes, ["admin"]);
        assert_eq!(
            call.params,
            json!({"name": "Research", "webhook": "https://example.com/hook", "key": "sk-123"})
        );
        // No key typed: nothing is sent.
        let script = Script::new(vec![]);
        let (code, _, _) = run_with(
            &script,
            GrokbotCmd::Add {
                name: "Research".into(),
                webhook: "https://example.com/hook".into(),
                id: None,
                keep_key: false,
                json: false,
            },
        );
        assert_eq!(code, 1);
        assert!(script.methods().is_empty());
    }

    #[test]
    fn list_says_what_is_missing() {
        let lines = list_lines(&json!({
            "enabled": true, "live": true, "key_store": "keychain",
            "deckd": "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd",
            "bots": [
                {"id": "research", "name": "Research", "set_up": true, "webhook_host": "api.example.com",
                 "has_key": false, "on_deck": true, "last": {"status": 401, "ok": false}},
                {"id": "travel", "name": "Travel", "set_up": false, "on_deck": true, "last": null},
            ]
        }));
        assert_eq!(lines[0], "Grok Bot: on (keys in the Keychain)");
        assert!(lines[1].contains("key: MISSING") && lines[1].contains("last call: HTTP 401"), "{}", lines[1]);
        assert!(lines[2].contains("not set up"));
        assert!(lines[3].ends_with("grokbot say --bot <id> '<reply>'"));
        assert_eq!(refusal_line(&json!({"status": 401})), "Grok Bot refused the key (HTTP 401): copy the routine's key again");
    }

    #[test]
    fn prompt_writes_the_message_that_makes_a_bot_s_routine() {
        let status = json!({
            "deckd": "/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd",
            "bots": [{"id": "research", "name": "Research", "routine": "…"}]
        });
        // A new name: the id the service will give it.
        let script = Script::new(vec![Ok(status.clone())]);
        let (code, out, _) = run_with(&script, GrokbotCmd::Prompt { bot: "Travel Desk".into() });
        assert_eq!(code, 0);
        assert!(out.starts_with("Please make a routine named \"OpenWalkieTalkie\""), "{out}");
        assert!(out.contains("grokbot say --bot 'travel-desk'"), "{out}");
        assert_eq!(script.call(0).scopes, ["admin"]);
        // A bot that is set up, by its name in any case: its own id and name.
        let script = Script::new(vec![Ok(status.clone())]);
        let (_, out, _) = run_with(&script, GrokbotCmd::Prompt { bot: "research".into() });
        assert!(out.contains("addressed to you (Research)") && out.contains("--bot 'research'"), "{out}");
        // A service on its own directory (Omarchy's): its commands say where.
        let own = json!({
            "deckd": "/home/owner/.local/bin/deckd",
            "invocation": "'/home/owner/.local/bin/deckd' --data-dir '/home/owner/.local/share/openwalkietalkie'",
            "bots": []
        });
        let script = Script::new(vec![Ok(own)]);
        let (_, out, _) = run_with(&script, GrokbotCmd::Prompt { bot: "Research".into() });
        assert!(
            out.contains("--data-dir '/home/owner/.local/share/openwalkietalkie' grokbot say --bot 'research'"),
            "{out}"
        );
        // No name, no prompt: refused, with why.
        let script = Script::new(vec![Ok(status)]);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let refused = run(&script, GrokbotCmd::Prompt { bot: "  ".into() }, &mut out, &mut err);
        assert_eq!(refused, Err("the bot needs a name".to_string()));
        assert!(out.is_empty());
    }
}
