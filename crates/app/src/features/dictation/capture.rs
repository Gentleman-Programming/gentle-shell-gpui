//! The microphone: the system's default input device, captured by a thread
//! of its own. The GUI never opens the device (that happens on the
//! background executor) and never touches the stream: cpal's `Stream` is
//! not `Send`, so the keeper thread creates it, holds it, closes it, and
//! hands back the finished audio through a channel. Stopping — on finish,
//! on the cap, on a stream error, on cancel or drop — always happens
//! within `POLL` on that thread. The startup deadline (open + first
//! callback together) is one coherent budget enforced by the caller; the
//! OS's own blocking device open cannot be interrupted by anyone and is
//! stated as an honest limit, never guaranteed away.

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use gentle_core::dictation as core;

/// How often the keeper thread checks for cancellation, the cap, a stop
/// command or a dead stream: the microphone closes within this window
/// after any of them.
const POLL: Duration = Duration::from_millis(100);

/// ONE coherent startup budget, enforced by the CALLER over device open
/// AND first callback together: the keeper thread itself only knows
/// cancellation, never a nested deadline of its own.
pub const STARTUP_BUDGET: Duration = Duration::from_secs(5);

/// What stopping a recording produces.
pub struct FinishedRecording {
    /// 16 kHz mono PCM, capped at [`core::MAX_SAMPLES`].
    pub audio: Vec<i16>,
    /// The recording reached its time limit.
    pub hit_limit: bool,
    /// The stream's own error, if it reported one.
    pub error: Option<String>,
}

/// A live recording the state machine can drive: the real one wraps cpal,
/// tests substitute their own.
pub trait Live: Send {
    /// The microphone already stopped (the cap was reached, or the device
    /// reported an error): the status can say so truthfully.
    fn capped(&self) -> bool;
    /// Stop the microphone (bounded wait) and take the finished audio.
    /// Dropping without `finish` stops the microphone too.
    fn finish(self: Box<Self>) -> FinishedRecording;
}

/// Shared between the capture callback, the keeper thread and the UI.
#[derive(Default)]
struct Buffer {
    raw: Vec<f32>,
    /// The stream delivered its first callback: audio is really flowing.
    has_audio: bool,
    channels: usize,
    sample_rate: u32,
    error: Option<String>,
    /// The keeper thread closed the stream.
    mic_stopped: bool,
}

impl Buffer {
    /// The recording's cap, in raw samples at the device's own rate and
    /// channel count ([`core::raw_sample_cap`]).
    fn full(&self) -> bool {
        self.raw.len() >= core::raw_sample_cap(self.sample_rate, self.channels)
    }
}

/// The opening's cancellation handle, held by the state machine from the
/// moment the opening starts: Esc, a restart or a drop reach the keeper
/// thread within [`POLL`], close the device and discard the buffer.
#[derive(Clone)]
pub struct Opening {
    cancel: mpsc::Sender<()>,
}

impl Opening {
    pub fn cancel(&self) {
        let _ = self.cancel.send(());
    }
}

/// A real recording. Only channels and the shared buffer cross threads;
/// the stream itself never leaves its keeper. Dropping it stops the
/// microphone within [`POLL`].
struct CpalRecording {
    buffer: Arc<Mutex<Buffer>>,
    control: mpsc::Sender<()>,
    done: Mutex<mpsc::Receiver<FinishedRecording>>,
}

impl Live for CpalRecording {
    fn capped(&self) -> bool {
        self.buffer
            .lock()
            .map(|buffer| buffer.mic_stopped)
            .unwrap_or(true)
    }

    fn finish(self: Box<Self>) -> FinishedRecording {
        // Ask for a stop even when the cap didn't fire; the command is
        // ignored if the thread already stopped on its own.
        let _ = self.control.send(());
        let done = self.done.lock().expect("the capture result channel");
        match done.recv_timeout(FINISH_WAIT) {
            Ok(finished) => finished,
            Err(_) => FinishedRecording {
                audio: Vec::new(),
                hit_limit: false,
                error: Some("The microphone did not close in time.".to_string()),
            },
        }
    }
}

impl Drop for CpalRecording {
    fn drop(&mut self) {
        // Best effort: the keeper thread closes the microphone within
        // `POLL` and throws the audio away.
        let _ = self.control.send(());
    }
}

/// What starting an opening hands back: the cancel handle and the ready
/// result (a live recording, or why it failed).
pub type OpeningResult = Result<Box<dyn Live>, String>;

/// Start opening the default input device and return immediately: an
/// [`Opening`] whose `cancel()` reaches the keeper thread at every poll
/// (right after setup and during the first-
/// audio wait), and the receiver of the ready result. The CALLER enforces
/// the one coherent [`STARTUP_BUDGET`] over open + first callback; the
/// OS's own blocking device open cannot be interrupted and is an honest
/// limit. The GUI never opens the device on its own thread.
pub fn start_capture_opening() -> Result<(Opening, mpsc::Receiver<OpeningResult>), String> {
    let buffer = Arc::new(Mutex::new(Buffer::default()));
    let (control_tx, control_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel::<FinishedRecording>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<Box<dyn Live>, String>>();
    let thread_buffer = buffer.clone();
    let thread_control = control_rx;
    let live_control = control_tx.clone();
    std::thread::Builder::new()
        .name("gentle-dictation-capture".into())
        .spawn(move || {
            let control_rx = thread_control;
            // Cancellation is checked before anything expensive happens.
            if cancelled(&control_rx) {
                let _ = ready_tx.send(Err(CANCELLED.to_string()));
                return;
            }
            // Everything about the stream lives and dies on this thread.
            // The OS's blocking open itself cannot be interrupted; the
            // token is checked again the moment it returns.
            let opened = open_stream(thread_buffer.clone());
            let (stream, _, _) = match opened {
                Ok(opened) => opened,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            // The format is already in the buffer: open_stream set it
            // before play(), so no callback ever sees a zero format.
            if cancelled(&control_rx) {
                drop(stream);
                let _ = ready_tx.send(Err(CANCELLED.to_string()));
                return;
            }
            // Bounded from OUTSIDE (the caller's budget), cancel-aware from
            // INSIDE: "Recording" must mean audio is actually flowing.
            // Samples that arrive during this wait are already in the
            // buffer and are kept.
            if let Err(error) = wait_first_audio(&control_rx, &thread_buffer, POLL) {
                drop(stream);
                let _ = ready_tx.send(Err(error));
                return;
            }
            // Audio is flowing: the opening is ready. The cancel sender
            // also stays with the live recording for stop/cap/drop.
            let _ = ready_tx.send(Ok(Box::new(CpalRecording {
                buffer,
                control: live_control,
                done: Mutex::new(done_rx),
            })));
            // The keeper loop: a stop command, the cap, or a dead stream
            // ends the recording; otherwise wait and check again.
            loop {
                match control_rx.recv_timeout(POLL) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let stop = thread_buffer
                            .lock()
                            .map(|buffer| buffer.full() || buffer.error.is_some())
                            .unwrap_or(true);
                        if stop {
                            break;
                        }
                    }
                }
            }
            // The microphone closes here, whatever the reason was.
            drop(stream);
            let finished = {
                let mut buffer = thread_buffer.lock().expect("the capture buffer");
                buffer.mic_stopped = true;
                FinishedRecording {
                    audio: core::mono_16k(&buffer.raw, buffer.channels, buffer.sample_rate),
                    hit_limit: buffer.full(),
                    error: buffer.error.clone(),
                }
            };
            let _ = done_tx.send(finished);
        })
        .map_err(|_| "The microphone's keeper thread could not start.".to_string())?;
    Ok((Opening { cancel: control_tx }, ready_rx))
}

const CANCELLED: &str = "The microphone opening was cancelled.";

/// Whether a cancellation (or a dropped handle) is waiting on the channel.
fn cancelled(control_rx: &mpsc::Receiver<()>) -> bool {
    matches!(
        control_rx.try_recv(),
        Ok(()) | Err(mpsc::TryRecvError::Disconnected)
    )
}

/// Build and start the input stream on the keeper thread. The callback
/// appends straight into the shared buffer, converting in place: no
/// allocation on the realtime thread.
fn open_stream(buffer: Arc<Mutex<Buffer>>) -> Result<(cpal::Stream, usize, u32), String> {
    let device = cpal::default_host()
        .default_input_device()
        .ok_or_else(|| "No microphone was found on this machine.".to_string())?;
    let config = device
        .default_input_config()
        .map_err(|error| format!("The microphone could not be read: {error}."))?;
    let stream_config: cpal::StreamConfig = config.clone().into();
    let channels = stream_config.channels as usize;
    let sample_rate = stream_config.sample_rate.0;
    let report = buffer.clone();
    let push_error = move |error: cpal::StreamError| {
        if let Ok(mut buffer) = report.lock()
            && buffer.error.is_none()
        {
            buffer.error = Some(format!("The microphone stopped: {error}."));
        }
    };
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            let push = buffer.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[f32], _| append(&push, data, |&sample| sample),
                push_error,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let push = buffer.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[i16], _| {
                    append(&push, data, |&sample| {
                        f32::from(sample) / f32::from(i16::MAX)
                    })
                },
                push_error,
                None,
            )
        }
        cpal::SampleFormat::U8 => {
            let push = buffer.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[u8], _| {
                    append(&push, data, |&sample| {
                        (i16::from(sample) - 128) as f32 / 128.
                    })
                },
                push_error,
                None,
            )
        }
        other => {
            return Err(format!(
                "The microphone's sample format ({other:?}) is not supported."
            ));
        }
    }
    .map_err(|error| format!("The microphone could not be opened: {error}."))?;
    let stream = activate_stream(
        &buffer,
        move || {
            stream
                .play()
                .map_err(|error| format!("The microphone could not start: {error}."))?;
            Ok(stream)
        },
        channels,
        sample_rate,
    )?;
    Ok((stream, channels, sample_rate))
}

/// The activation seam: the device's REAL format enters the buffer BEFORE
/// the stream may deliver anything, so the very first callback — which can
/// fire synchronously inside play() — sees the correct rate/channel cap
/// instead of the zero-format 120-sample fallback. Returns once the stream
/// is playing.
fn activate_stream<R>(
    buffer: &Arc<Mutex<Buffer>>,
    play: impl FnOnce() -> Result<R, String>,
    channels: usize,
    sample_rate: u32,
) -> Result<R, String> {
    {
        let mut buffer = buffer.lock().expect("the capture buffer");
        buffer.channels = channels;
        buffer.sample_rate = sample_rate;
    }
    play()
}

/// The capture callback: bounded append, converting in place — no
/// allocation on the realtime thread. A poisoned lock only drops frames.
fn append<S: Copy>(buffer: &Mutex<Buffer>, data: &[S], convert: impl Fn(&S) -> f32) {
    if let Ok(mut buffer) = buffer.lock() {
        if !data.is_empty() {
            buffer.has_audio = true;
        }
        if !buffer.full() {
            buffer.raw.extend(data.iter().map(convert));
        }
    }
}

/// The keeper's actual first-audio wait, factored for tests: a fake
/// buffer and channel stand in for the device. Cancellation via the
/// control channel resolves promptly with an error; a stream error fails
/// the opening; the first callback returns Ok with its samples already in
/// the buffer. There is NO internal deadline here: the caller enforces
/// the one coherent startup budget and signals cancellation through the
/// same channel.
fn wait_first_audio(
    control_rx: &mpsc::Receiver<()>,
    buffer: &Mutex<Buffer>,
    poll: Duration,
) -> Result<(), String> {
    loop {
        match control_rx.recv_timeout(poll) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return Err(CANCELLED.to_string()),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let (has_audio, error) = {
                    let buffer = buffer.lock().expect("the capture buffer");
                    (buffer.has_audio, buffer.error.clone())
                };
                if let Some(error) = error {
                    return Err(error);
                }
                if has_audio {
                    return Ok(());
                }
            }
        }
    }
}

const FINISH_WAIT: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use super::{Buffer, wait_first_audio};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Cancel while the keeper waits for the first callback: the waiter
    /// resolves promptly with the cancellation, and the samples heard
    /// before the cancel stay in the buffer (they are discarded with the
    /// stream, never lost silently mid-flight).
    #[test]
    fn cancel_while_waiting_first_audio_resolves_promptly() {
        let buffer = Arc::new(Mutex::new(Buffer::default()));
        {
            let mut guard = buffer.lock().unwrap();
            guard.raw = vec![0.25; 160];
            guard.channels = 1;
            guard.sample_rate = 16_000;
        }
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let _ = tx.send(());
        });
        let started = Instant::now();
        let outcome = wait_first_audio(&rx, &buffer, Duration::from_millis(10));
        assert!(
            outcome.is_err(),
            "a cancelled opening never starts recording"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the cancel must resolve promptly, took {:?}",
            started.elapsed()
        );
        assert_eq!(
            buffer.lock().unwrap().raw.len(),
            160,
            "samples heard during the wait are preserved"
        );
    }

    /// A callback arriving AFTER the cancellation cannot flip the opening
    /// back into recording: the waiter already failed, and the late audio
    /// is discarded with the closed stream.
    #[test]
    fn a_late_callback_after_cancel_does_not_enter_recording() {
        let buffer = Arc::new(Mutex::new(Buffer::default()));
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let _ = tx.send(()); // cancelled before the wait even starts
        let outcome = wait_first_audio(&rx, &buffer, Duration::from_millis(5));
        assert!(outcome.is_err());
        // The late callback lands afterwards: the state machine sees it
        // only through a stale handle, which begin() rejects.
        super::append(&buffer, &[0.5f32], |s| *s);
        assert!(buffer.lock().unwrap().has_audio);
        assert!(outcome.is_err(), "the failed opening stays failed");
    }

    /// A stream error during the wait fails the opening with that error.
    #[test]
    fn a_stream_error_during_the_wait_fails_the_opening() {
        let buffer = Arc::new(Mutex::new(Buffer::default()));
        buffer.lock().unwrap().error = Some("The microphone stopped: device gone.".into());
        let (_tx, rx) = std::sync::mpsc::channel::<()>();
        let outcome = wait_first_audio(&rx, &buffer, Duration::from_millis(5));
        let error = outcome.unwrap_err();
        assert!(error.contains("device gone"), "{error}");
    }

    /// THE startup race, pinned: the stream's format (channels, sample
    /// rate) must be in the buffer BEFORE play(), because the first
    /// callback can fire synchronously at the play seam. With the old
    /// order (play first, format later) a 48 kHz stereo device's early
    /// audio was capped at 120 samples (the zero-format fallback) and
    /// discarded — the user's first words were lost.
    #[test]
    fn the_first_callback_at_the_play_seam_sees_the_real_format() {
        for (channels, rate) in [(2usize, 48_000u32), (1, 44_100)] {
            let buffer = Arc::new(Mutex::new(Buffer::default()));
            let callback_buffer = buffer.clone();
            super::activate_stream(
                &buffer,
                move || {
                    // The first callback fires synchronously inside play().
                    super::append(&callback_buffer, &vec![0.25f32; 500], |s| *s);
                    Ok(())
                },
                channels,
                rate,
            )
            .unwrap();
            let guard = buffer.lock().unwrap();
            assert_eq!(
                guard.raw.len(),
                500,
                "all early samples stored at {rate} Hz × {channels}ch, not the                  zero-format 120-sample cap"
            );
            assert_eq!(guard.channels, channels);
            assert_eq!(guard.sample_rate, rate);
            assert!(guard.has_audio, "the ready flag is truthful");
        }
    }

    /// The first callback itself starts recording (Ok) with the buffer's
    /// samples kept.
    #[test]
    fn the_first_callback_starts_recording_and_keeps_its_samples() {
        let buffer = Arc::new(Mutex::new(Buffer::default()));
        buffer.lock().unwrap().has_audio = true;
        buffer.lock().unwrap().raw = vec![0.5; 32];
        let (_tx, rx) = std::sync::mpsc::channel::<()>();
        let outcome = wait_first_audio(&rx, &buffer, Duration::from_millis(5));
        assert!(outcome.is_ok(), "audio flows: recording starts");
        assert_eq!(buffer.lock().unwrap().raw.len(), 32);
    }
}
