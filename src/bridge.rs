//! Pure helpers for the async event loop (generation tracking, barge-in, stop_pending).

use crate::core::{transition, Effect, Event, State};

/// Whether a button press during transcription should cancel the in-flight turn.
pub fn should_barge_in(state: &State, event: &Event) -> bool {
    *state == State::Transcribing && matches!(event, Event::RecognitionStarted)
}

/// Drop transcription completions from an older generation after barge-in.
pub fn accept_transcript(generation: u64, current_gen: u64) -> bool {
    generation == current_gen
}

/// Resolve a background transcription completion, if it is still current.
pub fn incoming_to_event(generation: u64, event: Event, current_gen: u64) -> Option<Event> {
    if accept_transcript(generation, current_gen) {
        Some(event)
    } else {
        None
    }
}

/// Session fields the event loop mutates besides capture handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopState {
    pub core: State,
    pub stop_pending: bool,
    pub transcribe_gen: u64,
}

/// Side effects the shell should run after a transition (excluding I/O handles).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopAction {
    StartCapture,
    StopCaptureAndTranscribe,
    ClearRecognitionFlag,
    AbortPlayback,
}

/// Apply one event and return updated loop state plus actions to perform.
pub fn apply_event(state: &LoopState, event: Event) -> (LoopState, Vec<LoopAction>) {
    let mut next = state.clone();
    let mut actions = Vec::new();

    if should_barge_in(&state.core, &event) {
        next.transcribe_gen = next.transcribe_gen.wrapping_add(1);
        actions.push(LoopAction::AbortPlayback);
    }

    let (core, effects) = transition(&state.core, event);
    next.core = core;

    for effect in effects {
        match effect {
            Effect::StartCapture => {
                actions.push(LoopAction::StartCapture);
                if next.stop_pending {
                    next.stop_pending = false;
                    actions.push(LoopAction::StopCaptureAndTranscribe);
                }
            }
            Effect::StopCaptureAndTranscribe => {
                actions.push(LoopAction::StopCaptureAndTranscribe);
            }
            Effect::ClearRecognitionFlag => actions.push(LoopAction::ClearRecognitionFlag),
        }
    }

    (next, actions)
}

/// Record that stop arrived before parecord was ready.
pub fn request_stop_before_capture(state: &LoopState) -> LoopState {
    let mut next = state.clone();
    next.stop_pending = true;
    next
}

/// After TTS playback the QC AVRCP toggle often sends **pause** (0xc4) on the next
/// press while we are idle; core treats that as `RecognitionStopped`, which is a no-op
/// in `Idle`. Remap to `RecognitionStarted` for the btmon backend only.
pub fn remap_headset_button(use_btmon: bool, state: &State, event: Event) -> Event {
    if use_btmon
        && *state == State::Idle
        && event == Event::RecognitionStopped
    {
        Event::RecognitionStarted
    } else {
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle() -> LoopState {
        LoopState {
            core: State::Idle,
            stop_pending: false,
            transcribe_gen: 0,
        }
    }

    #[test]
    fn should_barge_in_only_when_transcribing() {
        assert!(should_barge_in(
            &State::Transcribing,
            &Event::RecognitionStarted
        ));
        assert!(!should_barge_in(
            &State::Listening,
            &Event::RecognitionStarted
        ));
        assert!(!should_barge_in(
            &State::Transcribing,
            &Event::RecognitionStopped
        ));
    }

    #[test]
    fn accept_transcript_matches_generation() {
        assert!(accept_transcript(2, 2));
        assert!(!accept_transcript(1, 2));
    }

    #[test]
    fn incoming_to_event_drops_stale() {
        assert!(incoming_to_event(1, Event::TranscriptReady("x".into()), 2).is_none());
        assert!(matches!(
            incoming_to_event(2, Event::TranscriptReady("x".into()), 2),
            Some(Event::TranscriptReady(_))
        ));
    }

    #[test]
    fn apply_recognition_started_from_idle() {
        let (next, actions) = apply_event(&idle(), Event::RecognitionStarted);
        assert_eq!(next.core, State::Listening);
        assert_eq!(actions, vec![LoopAction::StartCapture]);
    }

    #[test]
    fn apply_barge_in_bumps_generation_and_aborts_playback() {
        let state = LoopState {
            core: State::Transcribing,
            stop_pending: false,
            transcribe_gen: 4,
        };
        let (next, actions) = apply_event(&state, Event::RecognitionStarted);
        assert_eq!(next.core, State::Listening);
        assert_eq!(next.transcribe_gen, 5);
        assert_eq!(
            actions,
            vec![LoopAction::AbortPlayback, LoopAction::StartCapture]
        );
    }

    #[test]
    fn apply_stop_pending_triggers_transcribe_when_capture_starts() {
        let state = LoopState {
            core: State::Idle,
            stop_pending: true,
            transcribe_gen: 0,
        };
        let (next, actions) = apply_event(&state, Event::RecognitionStarted);
        assert_eq!(next.core, State::Listening);
        assert!(!next.stop_pending);
        assert_eq!(
            actions,
            vec![
                LoopAction::StartCapture,
                LoopAction::StopCaptureAndTranscribe,
            ]
        );
    }

    #[test]
    fn apply_transcript_ready_clears_to_idle() {
        let state = LoopState {
            core: State::Transcribing,
            stop_pending: false,
            transcribe_gen: 1,
        };
        let (next, actions) = apply_event(&state, Event::TranscriptReady("hello".into()));
        assert_eq!(next.core, State::Idle);
        assert_eq!(actions, vec![LoopAction::ClearRecognitionFlag]);
    }

    #[test]
    fn request_stop_before_capture_sets_flag() {
        let next = request_stop_before_capture(&idle());
        assert!(next.stop_pending);
    }

    #[test]
    fn remap_idle_avrcp_pause_to_start_for_btmon() {
        assert_eq!(
            remap_headset_button(true, &State::Idle, Event::RecognitionStopped),
            Event::RecognitionStarted
        );
        assert_eq!(
            remap_headset_button(false, &State::Idle, Event::RecognitionStopped),
            Event::RecognitionStopped
        );
        assert_eq!(
            remap_headset_button(true, &State::Listening, Event::RecognitionStopped),
            Event::RecognitionStopped
        );
    }
}
