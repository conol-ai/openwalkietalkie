//! A PTT hold can contain several Apple speech-recognition utterances.
//!
//! On-device recognition on macOS 27 reports a settled utterance (with
//! speech metadata) while `isFinal` is still false. After a pause, the next
//! result contains only the new utterance, with untimed, revisable words.
//! Keep the settled phrases until the PTT session ends. Use their audio
//! ranges, not matching words, to distinguish a revision from a new phrase:
//! saying the same sentence twice must not erase the second occurrence.

#[derive(Clone, Copy, Debug)]
pub(super) struct SpeechSpan {
    pub start: f64,
    pub end: f64,
}

struct Phrase {
    span: SpeechSpan,
    text: String,
}

#[derive(Default)]
pub(super) struct AppleTranscript {
    settled: Vec<Phrase>,
    pending: String,
}

impl AppleTranscript {
    /// `span` is present only for a settled result, whose word timings are
    /// available. A normal partial is a replacement for the pending phrase,
    /// never an addition to it. An empty final clears that pending preview.
    pub fn update(&mut self, text: String, span: Option<SpeechSpan>) -> String {
        self.pending.clear();
        if let Some(span) = span.filter(|s| {
            s.start.is_finite() && s.end.is_finite() && s.start >= 0.0 && s.end > s.start
        }) {
            // A later result can revise the same phrase or cover the entire
            // recording (older recognizers return cumulative transcripts).
            // Replace the overlapping suffix in either case. Nonoverlapping
            // phrases before this audio range remain part of the PTT hold.
            self.settled.retain(|phrase| phrase.span.end <= span.start);
            if !text.trim().is_empty() {
                self.settled.push(Phrase { span, text });
            }
        } else {
            self.pending = text;
        }
        self.settled
            .iter()
            .map(|phrase| phrase.text.trim())
            .chain(std::iter::once(self.pending.trim()))
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled(start: f64, end: f64) -> Option<SpeechSpan> {
        Some(SpeechSpan { start, end })
    }

    #[test]
    fn pauses_preserve_all_three_phrases_in_preview_and_final() {
        // Timing and callback order from a real, on-device en-US run:
        // metadata-bearing results at the pauses still had isFinal=false.
        let mut transcript = AppleTranscript::default();
        transcript.update("Please keep the first".into(), None);
        transcript.update("Please keep the first sentence".into(), settled(0.0, 1.65));
        assert_eq!(
            transcript.update("Then".into(), None),
            "Please keep the first sentence Then"
        );
        transcript.update("Then add the second sentence".into(), settled(5.28, 6.39));
        transcript.update("Finally keep all three sentence".into(), None);
        assert_eq!(
            transcript.update("Finally keep all three sentences".into(), settled(9.48, 11.67)),
            "Please keep the first sentence Then add the second sentence Finally keep all three sentences"
        );
    }

    #[test]
    fn partial_corrections_replace_words_without_duplicating_them() {
        let mut transcript = AppleTranscript::default();
        transcript.update("Turn on".into(), None);
        transcript.update("Turn off".into(), None);
        assert_eq!(
            transcript.update("Turn off the light".into(), settled(0.2, 2.0)),
            "Turn off the light"
        );
        transcript.update("Next turn on".into(), None);
        assert_eq!(
            transcript.update("Next turn off the fan".into(), None),
            "Turn off the light Next turn off the fan"
        );
    }

    #[test]
    fn repeated_words_are_preserved_when_spoken_again_after_a_pause() {
        let mut transcript = AppleTranscript::default();
        transcript.update("Try again".into(), settled(0.1, 1.0));
        assert_eq!(
            transcript.update("Try again".into(), None),
            "Try again Try again"
        );
        assert_eq!(
            transcript.update("Try again".into(), settled(4.0, 5.0)),
            "Try again Try again"
        );
    }

    #[test]
    fn final_revises_the_last_settled_phrase_without_appending_it_twice() {
        let mut transcript = AppleTranscript::default();
        transcript.update("First phrase".into(), settled(0.0, 1.0));
        transcript.update("Second freeze".into(), settled(4.0, 5.0));
        assert_eq!(
            transcript.update("Second phrase.".into(), settled(4.05, 5.1)),
            "First phrase Second phrase."
        );
    }

    #[test]
    fn cumulative_final_replaces_the_phrases_it_covers() {
        let mut transcript = AppleTranscript::default();
        transcript.update("First phrase".into(), settled(0.2, 1.0));
        transcript.update("Second phrase".into(), settled(4.0, 5.0));
        assert_eq!(
            transcript.update("First phrase. Second phrase.".into(), settled(0.2, 5.0)),
            "First phrase. Second phrase."
        );
    }

    #[test]
    fn an_empty_final_does_not_promote_an_unconfirmed_preview() {
        let mut transcript = AppleTranscript::default();
        transcript.update("Unconfirmed words".into(), None);
        assert_eq!(transcript.update(String::new(), None), "");
        transcript.update("Settled phrase".into(), settled(0.1, 1.0));
        transcript.update("Unconfirmed tail".into(), None);
        assert_eq!(transcript.update(String::new(), None), "Settled phrase");
    }

    #[test]
    fn a_final_without_timing_still_replaces_its_preview() {
        let mut transcript = AppleTranscript::default();
        transcript.update("The wrong words".into(), None);
        assert_eq!(
            transcript.update("The right words".into(), None),
            "The right words"
        );
    }
}
