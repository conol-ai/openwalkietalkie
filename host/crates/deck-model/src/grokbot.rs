//! Grok Bot's names and words, shared by the service and the app: the id a
//! bot's name gives it, the commands a bot runs on this Mac to answer on the
//! deck, the instruction its webhook routine is given, and the prompt that
//! has a bot make that routine itself. Pure text, so the app can write the
//! prompt for a bot it has not added yet, word for word as the service
//! writes that bot's instruction once it is added.
use std::path::Path;

/// A bot's name on the deck, in characters.
pub const NAME_MAX: usize = 32;
/// A bot id: lower-case letters, digits and dashes.
pub const ID_MAX: usize = 32;
/// The routine a setup prompt asks a bot to make.
pub const ROUTINE_NAME: &str = "OpenWalkieTalkie";
/// What every webhook body says about answering (`how_to_answer`), so that
/// a routine with an older or a hand-written instruction also leaves its
/// answer in the chat ([`routine_text`] says the same).
pub const HOW_TO_ANSWER: &str = "Run reply_with with a short version of your answer for my \
    deck to read out, then answer me in this chat as you normally would, so your full answer \
    stays in this chat's history. To have me decide something, run ask_with instead.";

/// An id for a bot named `name`: lower-case ASCII letters and digits, runs
/// of anything else as one dash, at most [`ID_MAX`]. A name with no ASCII
/// letter or digit at all gets `bot-` and a hash of the name, so it still
/// has one, and always the same one.
#[must_use]
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let mut id: String = out.trim_matches('-').chars().take(ID_MAX).collect();
    while id.ends_with('-') {
        id.pop();
    }
    if id.is_empty() {
        // FNV-1a: stable across runs and builds, unlike the std hasher.
        let mut hash: u32 = 0x811c_9dc5;
        for byte in name.trim().as_bytes() {
            hash ^= u32::from(*byte);
            hash = hash.wrapping_mul(0x0100_0193);
        }
        id = format!("bot-{hash:08x}");
    }
    id
}

/// Why `id` cannot name a bot, or `None`.
#[must_use]
pub fn id_problem(id: &str) -> Option<&'static str> {
    if id.is_empty() || id.len() > ID_MAX {
        return Some("a bot id is 1 to 32 characters");
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || id.starts_with('-')
        || id.ends_with('-')
    {
        return Some("a bot id is lower-case letters, digits and dashes");
    }
    None
}

/// Why `name` cannot be a bot's name on the deck, or `None`.
#[must_use]
pub fn name_problem(name: &str) -> Option<&'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Some("the bot needs a name");
    }
    if name.chars().count() > NAME_MAX {
        return Some("a bot's name is at most 32 characters");
    }
    if name.chars().any(char::is_control) {
        return Some("a bot's name has no control characters");
    }
    None
}

/// `value` in single quotes for a POSIX shell.
#[must_use]
pub fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// How every command a bot runs starts: `deckd`, quoted for the local shell
/// (PowerShell on Windows, POSIX elsewhere), then
/// `--data-dir` when the service does not run where a bare `deckd` looks
/// (`data_dir`): a test service's own directory, or Omarchy's
/// `~/.local/share/openwalkietalkie`, which its systemd unit sets. A bot's
/// shell has none of the service's environment, so the command has to say.
#[must_use]
pub fn invocation(deckd: &Path, data_dir: Option<&Path>) -> String {
    // PowerShell needs the call operator before a quoted executable path.
    let quote = |value: &str| {
        if cfg!(windows) { format!("'{}'", value.replace('\'', "''")) } else { sh_quote(value) }
    };
    let program = quote(&deckd.to_string_lossy());
    let deckd = if cfg!(windows) { format!("& {program}") } else { program };
    match data_dir {
        Some(dir) => format!("{deckd} --data-dir {}", quote(&dir.to_string_lossy())),
        None => deckd,
    }
}

/// The command a bot runs to put its reply on the deck.
#[must_use]
pub fn reply_command(invocation: &str, id: &str) -> String {
    format!("{invocation} grokbot say --bot {} '<your reply>'", sh_quote(id))
}

/// The command a bot runs to ask the operator on the deck.
#[must_use]
pub fn ask_command(invocation: &str, id: &str) -> String {
    format!(
        "{invocation} grokbot ask --bot {} --choice 'Yes' --choice 'No' '<your question>'",
        sh_quote(id)
    )
}

/// The instruction the owner gives the bot's webhook routine (in their own
/// words if they like): how to read an order and how to answer it. The deck
/// gets a short version of the answer to read out; the chat still gets the
/// whole answer, because Grok Bot shows a run's result in the bot's chat and
/// an answer given only as a command's argument would leave nothing there.
#[must_use]
pub fn routine_text(name: &str, id: &str, invocation: &str) -> String {
    let shell = if cfg!(windows) {
        "Run the commands below in PowerShell. Inside single-quoted text, double any apostrophe.\n"
    } else { "" };
    format!(
        "Each run of this routine is a message I spoke into my OpenWalkieTalkie deck, \
         addressed to you ({name}). The JSON body's \"text\" is what I said: treat it as a \
         message from me in this chat and do what it asks.\n\
         {shell}When you have an answer or you are done, run this on my computer, with a short \
         version of your answer I can listen to in place of <your reply>:\n{}\n\
         Then also answer me here in this chat as you normally would, so your full answer \
         stays in this chat's history.\n\
         If you need me to decide something, run this instead, with your question and up to \
         four short choices; it prints my answer:\n{}\n\
         Every message's body repeats both commands as \"reply_with\" and \"ask_with\".",
        reply_command(invocation, id),
        ask_command(invocation, id),
    )
}

/// What the owner sends a bot in Grok Bot's chat so that the bot makes its
/// own webhook routine, with [`routine_text`] as the routine's instruction.
/// The owner then copies the routine's URL and key into the app.
#[must_use]
pub fn setup_prompt(name: &str, id: &str, invocation: &str) -> String {
    format!(
        "Please make a routine named \"{ROUTINE_NAME}\" with a webhook trigger, so my \
         OpenWalkieTalkie deck can send you what I say. Give the routine this instruction, \
         word for word:\n\n{}",
        routine_text(name, id, invocation)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setup_prompt_asks_for_a_webhook_routine_with_the_bot_s_own_instruction() {
        let deckd = invocation(
            Path::new("/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd"),
            None,
        );
        let prompt = setup_prompt("Travel Desk", &slug("Travel Desk"), &deckd);
        assert!(
            prompt.starts_with(
                "Please make a routine named \"OpenWalkieTalkie\" with a webhook trigger"
            ),
            "{prompt}"
        );
        assert!(prompt.ends_with(&routine_text("Travel Desk", "travel-desk", &deckd)));
        assert!(prompt.contains(
            "'/Applications/OpenWalkieTalkie.app/Contents/MacOS/deckd' grokbot say --bot 'travel-desk'"
        ));
        assert!(prompt.contains("addressed to you (Travel Desk)"));
        // The deck gets a short version; the chat keeps the whole answer.
        assert!(prompt.contains("with a short version of your answer I can listen to"));
        assert!(prompt.contains("so your full answer stays in this chat's history"));
    }

    #[test]
    #[cfg(not(windows))]
    fn a_service_on_its_own_directory_is_named_in_every_command() {
        // Omarchy's unit runs deckd on ~/.local/share/openwalkietalkie, which a
        // bare `deckd` would not look in.
        let own = invocation(
            Path::new("/home/owner/.local/bin/deckd"),
            Some(Path::new("/home/owner/.local/share/openwalkietalkie")),
        );
        assert_eq!(
            reply_command(&own, "research"),
            "'/home/owner/.local/bin/deckd' --data-dir '/home/owner/.local/share/openwalkietalkie' \
             grokbot say --bot 'research' '<your reply>'"
        );
        assert!(ask_command(&own, "research").starts_with(&own));
        // The platform's own place needs no flag.
        assert_eq!(invocation(Path::new("/usr/bin/deckd"), None), "'/usr/bin/deckd'");
    }

    #[test]
    #[cfg(windows)]
    fn windows_commands_call_the_program_and_quote_powershell_paths() {
        let command = invocation(
            Path::new(r"C:\Program Files\Owner's Deck\deckd.exe"),
            Some(Path::new(r"C:\Users\Owner's Deck\data")),
        );
        assert_eq!(command, r"& 'C:\Program Files\Owner''s Deck\deckd.exe' --data-dir 'C:\Users\Owner''s Deck\data'");
        assert!(reply_command(&command, "research").starts_with(&command));
        assert!(ask_command(&command, "research").starts_with(&command));
    }

    #[test]
    fn a_name_gives_the_same_id_the_service_gives_it() {
        assert_eq!(slug("  Travel Desk!  "), "travel-desk");
        assert_eq!(name_problem("Travel Desk"), None);
        assert_eq!(name_problem("   "), Some("the bot needs a name"));
        assert_eq!(id_problem(&slug("研究")), None);
    }
}
