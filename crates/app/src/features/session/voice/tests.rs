use super::{CaptureLease, Shutdown, Status, Voice};
use pi_rpc::voice::VoiceEvent;
use std::sync::atomic::AtomicBool;

#[test]
fn cancellation_discards_late_results_and_keeps_the_app_lease_until_cleanup() {
    let busy = AtomicBool::new(false);
    let lease = CaptureLease::acquire(&busy).unwrap();
    let mut voice = Voice {
        status: Status::Starting,
        ..Voice::default()
    };
    voice.progress(VoiceEvent::Recording);
    assert_eq!(voice.status, Status::Recording);
    voice.progress(VoiceEvent::Transcribing);
    assert_eq!(voice.status, Status::Transcribing);
    voice.invalidate();
    assert_eq!(voice.status, Status::Cancelling);
    voice.progress(VoiceEvent::Transcribing);
    assert_eq!(voice.status, Status::Cancelling);
    assert!(CaptureLease::acquire(&busy).is_none());
    assert!(voice.finish(result("late")).is_none());
    assert_eq!(voice.status, Status::Idle);
    drop(lease);
    assert!(CaptureLease::acquire(&busy).is_some());
}

#[test]
fn terminal_events_offer_only_current_text_or_fixed_status() {
    let mut voice = Voice::default();
    for (code, status) in [
        ("not-installed", Status::Missing),
        ("no-input-device", Status::DeviceError),
        ("unsupported", Status::Unavailable),
        ("unexpected private detail", Status::Failed),
    ] {
        voice.status = Status::Starting;
        assert!(
            voice
                .finish(VoiceEvent::Error { code: code.into() })
                .is_none()
        );
        assert_eq!(voice.status, status);
    }
    voice.status = Status::Transcribing;
    assert_eq!(
        voice.finish(result(" spoken text ")).as_deref(),
        Some(" spoken text ")
    );
    assert_eq!(voice.status, Status::Idle);
}

#[test]
fn quit_closes_the_probe_gate_and_discards_an_active_result_across_tabs() {
    let shutdown = Shutdown::default();
    let mut pending = Voice {
        status: Status::Starting,
        ..Voice::default()
    };
    pending.observe_state(pending.state_epoch(), true);
    let mut active = Voice {
        status: Status::Transcribing,
        ..Voice::default()
    };
    assert!(pending.can_start(&shutdown));
    shutdown.begin();
    assert!(!pending.can_start(&shutdown));
    assert!(
        active
            .finish_if_running(result("late"), &shutdown)
            .is_none()
    );
    assert_eq!(active.status, Status::Idle);
}

#[test]
fn a_pending_capture_cannot_start_before_the_backend_answers_get_state() {
    let shutdown = Shutdown::default();
    let mut pending = Voice {
        status: Status::Starting,
        ..Voice::default()
    };
    let previous = pending.state_epoch();
    assert!(!pending.can_start(&shutdown));
    pending.observe_state(previous, false);
    assert!(!pending.can_start(&shutdown));
    pending.observe_state(previous, true);
    assert!(pending.can_start(&shutdown));
    pending.observe_state(previous, false);
    assert!(pending.can_start(&shutdown));

    pending.reset_state();
    pending.finish(VoiceEvent::Cancelled);
    pending.status = Status::Starting;
    pending.observe_state(previous, true);
    assert!(!pending.can_start(&shutdown));
    pending.observe_state(pending.state_epoch(), true);
    assert!(pending.can_start(&shutdown));
}

fn result(text: &str) -> VoiceEvent {
    VoiceEvent::Result {
        text: text.into(),
        truncated: false,
    }
}
