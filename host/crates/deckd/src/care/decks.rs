//! `care.decks` and `care.decks.forget`: the decks this Mac keeps a strip,
//! settings and Macropad keys for (`state.json`'s `decks`, §6.9 of
//! `docs/host-service.md`), and forgetting one.
//!
//! A deck is named by its label (`device::identity::label`), as everything
//! outside `state.json` names it, never by its address. What tells one deck
//! from another without it is in the list: when each last linked here, over
//! which bus, with which firmware on which board, and its strip. The label
//! is what `forget` takes.
//!
//! **Forgetting** drops a deck's record, so the next time it links it is a
//! deck new here and starts from the firmware's defaults (`State::select`).
//! The deck in use is never forgotten: the one linked, else the last one
//! identified. Its strip, settings and keys are the top level of
//! `state.json` and the snapshot's, the ones this Mac shows and sends, and
//! they would come back as its record at the next switch. Link another deck
//! first.
use crate::device::identity;
use crate::state::State;
use deck_model::{ApiError, FaceEntry, LastLinked};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Why the deck in use is not forgotten.
pub const IN_USE: &str = "in use: it is the deck linked, or else the last one identified, and its \
                          strip, settings and keys are the ones this Mac shows and sends. Link \
                          another deck first";

/// `care.decks` takes nothing.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Nothing {}

/// `care.decks.forget{deck}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Forget {
    deck: String,
}

/// One deck, as `care.decks` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Deck {
    /// Its label: eight hex digits, never its address.
    pub deck: String,
    /// The deck in use: the one linked, else the last one identified.
    pub current: bool,
    /// When it last linked here, over which bus, with which firmware on
    /// which board; `null` until it has.
    pub last_linked: Option<LastLinked>,
    /// Its strip as this Mac keeps it.
    pub faces: Vec<FaceEntry>,
    /// How many of its Macropad faces this Mac keeps keys for. A face whose
    /// keys were cleared (a Macropad taken off the strip) is none.
    pub macros: usize,
}

fn read<T: DeserializeOwned>(params: &serde_json::Value) -> Result<T, ApiError> {
    let params = if params.is_null() {
        serde_json::json!({})
    } else {
        params.clone()
    };
    serde_json::from_value(params).map_err(|e| ApiError::invalid_params(&e.to_string()))
}

/// `care.decks{}` → `{decks}`, the deck that linked last first and a deck
/// never linked here last.
pub fn list(state: &State, params: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    read::<Nothing>(params)?;
    let mut decks: Vec<Deck> = state
        .decks
        .iter()
        .map(|(id, record)| Deck {
            deck: identity::label(id),
            current: state.deck.as_deref() == Some(id.as_str()),
            last_linked: record.last_linked.clone(),
            faces: record.faces.clone(),
            macros: record
                .macros
                .values()
                .filter(|layout| !layout.keys.is_empty())
                .count(),
        })
        .collect();
    decks.sort_by(|a, b| {
        let at = |deck: &Deck| deck.last_linked.as_ref().map(|last| last.at);
        at(b).cmp(&at(a)).then_with(|| a.deck.cmp(&b.deck))
    });
    Ok(serde_json::json!({ "decks": decks }))
}

/// `care.decks.forget{deck}` → `{deck, forgotten:true}`: the record of the
/// deck labelled `deck` is gone from `state`, which the caller then saves.
/// A label that is not eight hex digits is `-32602`, one no deck here has is
/// `-32003 not_found{what:"deck"}`, and the deck in use is `-32009
/// invalid{field:"deck"}` ([`IN_USE`]).
pub fn forget(
    state: &mut State,
    params: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    let label = read::<Forget>(params)?.deck.trim().to_ascii_lowercase();
    if label.len() != 8 || !label.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::invalid_params(
            "deck: a deck's label as care.decks lists it, eight hex digits",
        ));
    }
    let Some(id) = state
        .decks
        .keys()
        .find(|id| identity::label(id) == label)
        .cloned()
    else {
        return Err(ApiError::not_found("deck"));
    };
    if state.deck.as_deref() == Some(id.as_str()) {
        return Err(ApiError::invalid("deck", IN_USE));
    }
    state.decks.remove(&id);
    Ok(serde_json::json!({ "deck": label, "forgotten": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use deck_model::{StatePatch, Template, Transport, Ts};
    use serde_json::json;

    const A: &str = "A1B2C3D4E5F6";
    const B: &str = "C0FFEE15600D";

    fn last(at: u64, transport: Transport) -> LastLinked {
        LastLinked {
            at: Ts(at),
            fw: Some("0.13.0-openwalkietalkie".into()),
            board: Some("Rev B".into()),
            transport: Some(transport),
        }
    }

    /// Deck A identified first, with a strip, keys for one Macropad (and
    /// the cleared keys of one taken off its strip) and a link over the
    /// cable; then deck B, in use now, linked later over Bluetooth with
    /// nothing of its own yet.
    fn two_decks() -> State {
        let mut state = State::default();
        state.select(Some(A));
        state.apply(&StatePatch::Faces(vec![
            FaceEntry {
                f: 1,
                t: Template::Agents,
                n: "Agents".into(),
            },
            FaceEntry {
                f: 5,
                t: Template::Settings,
                n: "Settings".into(),
            },
        ]));
        state.apply(&StatePatch::Macro {
            f: 3,
            src: None,
            keys: vec![deck_model::MacroKey {
                label: "Build".into(),
                binding: None,
                keys: Some("cmd+b".into()),
            }],
        });
        state.apply(&StatePatch::Macro {
            f: 4,
            src: None,
            keys: Vec::new(),
        });
        state.apply(&StatePatch::LastLinked(last(1_000, Transport::Usb)));
        state.select(Some(B));
        state.apply(&StatePatch::LastLinked(last(2_000, Transport::Bluetooth)));
        state
    }

    /// The list names each deck by its label, never its address, the deck
    /// in use marked and first here because it linked last, with what tells
    /// the decks apart.
    #[test]
    fn the_list_names_each_deck_by_its_label_with_what_tells_them_apart() {
        let state = two_decks();
        let reply = list(&state, &json!({})).unwrap();
        let decks = reply["decks"].as_array().unwrap();
        assert_eq!(decks.len(), 2);
        assert_eq!(decks[0]["deck"], identity::label(B));
        assert_eq!(decks[0]["current"], true);
        assert_eq!(decks[0]["last_linked"]["transport"], "bluetooth");
        assert_eq!(decks[0]["faces"], json!([]));
        assert_eq!(decks[1]["deck"], identity::label(A));
        assert_eq!(decks[1]["current"], false);
        assert_eq!(decks[1]["last_linked"]["at"], "1970-01-01T00:00:01.000Z");
        assert_eq!(decks[1]["faces"][0]["n"], "Agents");
        assert_eq!(decks[1]["macros"], 1);
        let text = reply.to_string().to_ascii_lowercase();
        assert!(!text.contains(&A.to_ascii_lowercase()), "{text}");
        assert!(!text.contains(&B.to_ascii_lowercase()), "{text}");
        // A deck never linked here comes last; an empty file lists nothing.
        let mut state = two_decks();
        state.decks.entry("0000000000AB".into()).or_default();
        let reply = list(&state, &serde_json::Value::Null).unwrap();
        assert_eq!(reply["decks"][2]["last_linked"], serde_json::Value::Null);
        assert_eq!(
            list(&State::default(), &json!({})).unwrap(),
            json!({ "decks": [] })
        );
        let error = list(&state, &json!({"all": true})).unwrap_err();
        assert_eq!(error.code, ApiError::INVALID_PARAMS);
    }

    /// Forgetting drops the record and nothing else; the label is read in
    /// any case. The deck in use stays, and so does everything when the
    /// label names no deck here or is not a label at all.
    #[test]
    fn a_deck_not_in_use_is_forgotten_and_the_one_in_use_is_kept() {
        let mut state = two_decks();
        let a = identity::label(A);
        let b = identity::label(B);
        let error = forget(&mut state, &json!({ "deck": b })).unwrap_err();
        assert_eq!(error.code, ApiError::INVALID, "{error:?}");
        assert_eq!(error.data["field"], "deck");
        assert_eq!(error.data["why"], IN_USE);
        let error = forget(&mut state, &json!({"deck": "0badc0de"})).unwrap_err();
        assert_eq!(error.code, ApiError::NOT_FOUND, "{error:?}");
        for params in [
            json!({}),
            json!({"deck": "abc"}),
            json!({"deck": A}),
            json!({"deck": a, "also": true}),
        ] {
            let error = forget(&mut state, &params).unwrap_err();
            assert_eq!(error.code, ApiError::INVALID_PARAMS, "{params}");
        }
        assert_eq!(state, two_decks(), "nothing was forgotten yet");
        let reply = forget(&mut state, &json!({ "deck": a.to_ascii_uppercase() })).unwrap();
        assert_eq!(reply, json!({"deck": a, "forgotten": true}));
        assert_eq!(state.decks.keys().collect::<Vec<_>>(), vec![B]);
        assert_eq!(
            state.deck.as_deref(),
            Some(B),
            "the deck in use stays in use"
        );
        // Forgotten, it is a deck new here the next time it links.
        assert_eq!(state.select(Some(A)), crate::state::Selected::New);
    }
}
