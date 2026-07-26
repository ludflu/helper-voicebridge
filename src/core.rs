//! Pure core of the bridge: a session state machine with no I/O.
//!
//! Contract: `transition` is a total, deterministic, side-effect-free function.
//! Given a `State` and an `Event` it returns the next `State` plus the list of
//! `Effect`s the imperative shell must perform. Nothing in this module touches
//! D-Bus, audio devices, or the network — that's the caller's job.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// No active session; waiting for the headphone button.
    Idle,
    /// Button pressed (AT+BVRA=1 seen); capturing mic audio.
    Listening,
    /// Button released; audio handed to STT.
    Transcribing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Headset opened a voice-recognition / SCO session.
    RecognitionStarted,
    /// Voice session ended (button release, second press, or mic idle).
    RecognitionStopped,
    /// STT produced a transcript (empty string means "nothing heard").
    TranscriptReady(String),
    /// STT failed outright (audio device error, etc).
    TranscriptFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Start recording from the HFP mic source.
    StartCapture,
    /// Stop recording and run STT over the captured buffer.
    StopCaptureAndTranscribe,
    /// Write VoiceRecognition=false back to oFono so the headset's LED/state
    /// returns to normal (completes the AT+BVRA handshake).
    ClearRecognitionFlag,
}

/// The one function this module exists to provide. No hidden state, no
/// clock, no randomness — same inputs always yield the same outputs.
pub fn transition(state: &State, event: Event) -> (State, Vec<Effect>) {
    use Effect::*;
    use State::*;

    match (state, event) {
        (Idle, Event::RecognitionStarted) => (Listening, vec![StartCapture]),

        // Bose QC opens SCO with AT+BVRA=1 but often omits AT+BVRA=0 on release;
        // a second button press while recording sends another start — treat as stop.
        (Listening, Event::RecognitionStarted) => {
            (Transcribing, vec![StopCaptureAndTranscribe])
        }

        (Listening, Event::RecognitionStopped) => {
            (Transcribing, vec![StopCaptureAndTranscribe])
        }

        (Transcribing, Event::TranscriptReady(_)) | (Transcribing, Event::TranscriptFailed) => {
            (Idle, vec![ClearRecognitionFlag])
        }

        (s, _) => (s.clone(), vec![]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path() {
        let s = State::Idle;
        let (s, fx) = transition(&s, Event::RecognitionStarted);
        assert_eq!(s, State::Listening);
        assert_eq!(fx, vec![Effect::StartCapture]);

        let (s, fx) = transition(&s, Event::RecognitionStopped);
        assert_eq!(s, State::Transcribing);
        assert_eq!(fx, vec![Effect::StopCaptureAndTranscribe]);

        let (s, fx) = transition(
            &s,
            Event::TranscriptReady("what's the weather".into()),
        );
        assert_eq!(s, State::Idle);
        assert_eq!(fx, vec![Effect::ClearRecognitionFlag]);
    }

    #[test]
    fn empty_transcript_returns_to_idle() {
        let (s, fx) = transition(&State::Transcribing, Event::TranscriptReady("  ".into()));
        assert_eq!(s, State::Idle);
        assert_eq!(fx, vec![Effect::ClearRecognitionFlag]);
    }

    #[test]
    fn second_press_while_listening_stops_capture() {
        let (s, fx) = transition(&State::Listening, Event::RecognitionStarted);
        assert_eq!(s, State::Transcribing);
        assert_eq!(fx, vec![Effect::StopCaptureAndTranscribe]);
    }

    #[test]
    fn stray_event_is_a_no_op() {
        let (s, fx) = transition(&State::Idle, Event::TranscriptReady(String::new()));
        assert_eq!(s, State::Idle);
        assert!(fx.is_empty());
    }
}
