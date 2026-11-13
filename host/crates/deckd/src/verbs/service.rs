//! `deckd service restart|stop`: the Service page's **Restart** and
//! **Stop…**. The service answers first and goes a moment later
//! (`service::Exit::GRACE`): a restart is launchd starting it again (exit
//! 75), a stop leaves it stopped (exit 0) until it is started again.
use super::{ask, finish, print_json, text, Service, Stop, ADMIN};
use clap::Subcommand;
use serde_json::{json, Value};
use std::io::Write;

#[derive(Subcommand, Debug)]
pub enum ServiceCmd {
    /// Restart the service (`service.restart`). With `--when-idle` it is
    /// refused while a press, a transcription, speech or a firmware update
    /// is under way.
    Restart {
        #[arg(long)]
        when_idle: bool,
        #[arg(long)]
        json: bool,
    },
    /// Stop the service (`service.stop`): it stays stopped until the app or
    /// launchd starts it again.
    Stop {
        #[arg(long)]
        json: bool,
    },
}

/// `deckd service …`.
pub fn run(
    service: &dyn Service,
    cmd: ServiceCmd,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, String> {
    finish(lifecycle(service, cmd, out, err))
}

fn lifecycle(
    service: &dyn Service,
    cmd: ServiceCmd,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, Stop> {
    let (method, params, json) = match cmd {
        ServiceCmd::Restart { when_idle, json } if when_idle => {
            ("service.restart", json!({"when_idle": true}), json)
        }
        ServiceCmd::Restart { json, .. } => ("service.restart", json!({}), json),
        ServiceCmd::Stop { json } => ("service.stop", json!({}), json),
    };
    let reply = ask(service, ADMIN, method, params, err)?;
    if json {
        print_json(out, &reply)?;
    } else {
        writeln!(out, "{}", exit_text(&reply))?;
    }
    Ok(0)
}

/// What the service said it does: the first request wins, so the reply's
/// action is the one that happens.
#[must_use]
pub fn exit_text(reply: &Value) -> String {
    let does = match text(reply, "action") {
        "restart" => "restarts",
        "stop" => "stops",
        other => other,
    };
    format!(
        "the service (pid {}) {does} in {} ms",
        reply["pid"].as_u64().unwrap_or(0),
        reply["in_ms"].as_u64().unwrap_or(0)
    )
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{run, Script};
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> ServiceCmd {
        let mut argv = vec!["deckd", "service"];
        argv.extend_from_slice(args);
        match crate::cli::Cli::try_parse_from(argv).unwrap().cmd {
            crate::cli::Cmd::Service { cmd } => cmd,
            other => panic!("{other:?}"),
        }
    }

    /// Each verb sends what the Service page sends, and says what the
    /// service answered it will do.
    #[test]
    fn restart_and_stop_send_what_the_service_page_sends() {
        for (args, method, params) in [
            (&["restart"][..], "service.restart", json!({})),
            (
                &["restart", "--when-idle"],
                "service.restart",
                json!({"when_idle": true}),
            ),
            (&["stop"], "service.stop", json!({})),
        ] {
            let reply = json!({"action": "restart", "exit": 75, "pid": 812, "in_ms": 250});
            let script = Script::new(vec![Ok(reply)]);
            let ran = run(|out, err| super::run(&script, parse(args), out, err));
            assert_eq!(ran.result, Ok(0), "{args:?}");
            let asked = script.call(0);
            assert_eq!(
                (asked.method.as_str(), asked.params),
                (method, params),
                "{args:?}"
            );
            assert_eq!(asked.scopes, ["admin"]);
            assert_eq!(ran.out, "the service (pid 812) restarts in 250 ms\n");
        }
        assert_eq!(
            exit_text(&json!({"action": "stop", "exit": 0, "pid": 9, "in_ms": 250})),
            "the service (pid 9) stops in 250 ms"
        );
    }
}
