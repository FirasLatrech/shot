//! Screen recording to an MP4 (H.264 + AAC) with ScreenCaptureKit: an area,
//! a window or a whole display, with system audio and/or the microphone.
//!
//! `SCRecordingOutput` writes the file itself, which needs macOS 15. While a
//! recording runs, the menu bar icon turns into a stop button with a timer.
//!
//! ScreenCaptureKit calls back on its own threads, so callbacks only send
//! [`Event`]s over a channel; the main thread polls it and owns all state.
//!
//! The one rule that matters: once a stream exists, the session ends only
//! after the stream has been told to stop and the file reported finished (or
//! a timeout passed). Dropping a live stream leaves it writing forever.

use std::{
    cell::RefCell,
    path::PathBuf,
    sync::{mpsc, Mutex},
    time::{Duration, Instant},
};

use block2::RcBlock;
use objc2::{define_class, msg_send, rc::Retained, runtime::ProtocolObject, AnyThread, DefinedClass};
use objc2_core_foundation::CGRect;
use objc2_core_media::{CMTime, CMTimeFlags};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSError, NSObject, NSObjectProtocol, NSOperatingSystemVersion, NSProcessInfo, NSString,
    NSURL,
};
use objc2_screen_capture_kit::{
    SCContentFilter, SCRecordingOutput, SCRecordingOutputConfiguration, SCRecordingOutputDelegate, SCShareableContent,
    SCStream, SCStreamConfiguration, SCStreamDelegate,
};
use shot_core::{Pt, Rect};

use crate::{
    app::{after, alert, app, play_sound, toast},
    capture, ui,
};

/// How long to wait for the file to be finished after asking to stop.
const STOP_TIMEOUT: Duration = Duration::from_secs(8);

/// What to record, in global CG points.
#[derive(Debug, Clone, Copy)]
pub enum Target {
    Area(Rect),
    Window {
        id: u32,
        bounds: Rect,
    },
    /// The whole display containing this point.
    Display(Pt),
}

enum Event {
    Content(Result<Sendable<Retained<SCShareableContent>>, String>),
    /// `startCapture` finished; `Err` means nothing is being captured.
    CaptureStarted(Result<(), String>),
    /// The recording output wrote its first frames.
    RecordingStarted,
    /// The movie file is complete.
    Finished,
    /// The recording or the stream failed while live.
    Error(String),
    /// `stopCapture` finished.
    CaptureStopped,
}

/// ScreenCaptureKit objects are thread-safe; this only moves a result from
/// its completion handler to the main thread.
struct Sendable<T>(T);
unsafe impl<T> Send for Sendable<T> {}

enum Phase {
    /// Waiting for the list of displays and windows; no stream yet.
    Preparing,
    /// The stream exists and is starting or capturing. `since` is when the
    /// first frames were recorded.
    Live { since: Option<Instant> },
    /// Asked the stream to stop; waiting until the file is finished and the
    /// capture has stopped (the file is only safe once both have happened).
    Stopping { at: Instant, length: Duration, finished: bool, stopped: bool },
}

struct Session {
    target: Target,
    path: PathBuf,
    phase: Phase,
    rx: mpsc::Receiver<Event>,
    tx: mpsc::Sender<Event>,
    /// Kept alive for the whole recording: ScreenCaptureKit holds these weakly.
    stream: Option<Retained<SCStream>>,
    output: Option<Retained<SCRecordingOutput>>,
    delegate: Option<Retained<Delegate>>,
    /// Set by a stop request before the stream existed.
    cancelled: bool,
    /// A failure to report once the session ends.
    error: Option<String>,
    /// Runs after the session ends, e.g. quitting the app.
    then: Option<Box<dyn FnOnce()>>,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    SESSION.with(|s| s.borrow_mut().as_mut().map(f))
}

/// `SCRecordingOutput` and microphone capture arrived in macOS 15.
pub fn supported() -> bool {
    let v = NSOperatingSystemVersion { majorVersion: 15, minorVersion: 0, patchVersion: 0 };
    NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(v)
}

pub fn is_active() -> bool {
    SESSION.with(|s| s.borrow().is_some())
}

pub fn start(target: Target) {
    start_to(target, None);
}

/// Records into `path`, or a timestamped file in the save folder.
pub fn start_to(target: Target, path: Option<PathBuf>) {
    if is_active() {
        return;
    }
    if !supported() {
        return alert("Screen recording needs macOS 15", "Update macOS to record videos with Shot.");
    }
    let Some(path) = path.or_else(output_path) else {
        return alert("Couldn't record", "The save folder isn't available.");
    };
    let (tx, rx) = mpsc::channel();
    let content_tx = tx.clone();
    let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        let result = match unsafe { Retained::retain(content) } {
            Some(c) => Ok(Sendable(c)),
            None => Err(describe(error)),
        };
        let _ = content_tx.send(Event::Content(result));
    });
    SESSION.with(|s| {
        *s.borrow_mut() = Some(Session {
            target,
            path,
            phase: Phase::Preparing,
            rx,
            tx,
            stream: None,
            output: None,
            delegate: None,
            cancelled: false,
            error: None,
            then: None,
        })
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
    app().set_recording_indicator(Some("●"));
    watch();
}

/// Stops the recording; the file is finished asynchronously.
pub fn stop() {
    stop_then(|| {});
}

/// Stops, and runs `then` once the session has ended (right away when
/// nothing is recording).
pub fn stop_then(then: impl FnOnce() + 'static) {
    if !is_active() {
        return then();
    }
    with(|s| {
        s.then = Some(Box::new(then));
        request_stop(s);
    });
}

/// Moves a live session to `Stopping`. Safe to call more than once.
fn request_stop(s: &mut Session) {
    match s.phase {
        Phase::Preparing => s.cancelled = true,
        Phase::Live { since } => {
            s.phase = Phase::Stopping {
                at: Instant::now(),
                length: since.map_or(Duration::ZERO, |t| t.elapsed()),
                finished: false,
                stopped: s.stream.is_none(),
            };
            if let Some(stream) = &s.stream {
                let tx = s.tx.clone();
                let done = RcBlock::new(move |error: *mut NSError| {
                    if !error.is_null() {
                        eprintln!("recording: stopCapture: {}", describe(error));
                    }
                    let _ = tx.send(Event::CaptureStopped);
                });
                unsafe { stream.stopCaptureWithCompletionHandler(Some(&done)) };
            }
        }
        Phase::Stopping { .. } => {}
    }
}

/// Polls the session's events on the main thread and keeps the timer current.
fn watch() {
    let events: Vec<Event> = with(|s| s.rx.try_iter().collect()).unwrap_or_default();
    for e in events {
        handle(e);
    }
    let timed_out = with(|s| matches!(s.phase, Phase::Stopping { at, .. } if at.elapsed() > STOP_TIMEOUT));
    if timed_out == Some(true) {
        with(|s| {
            s.error.get_or_insert_with(|| "The recording didn't finish cleanly, so the file may not play.".into());
        });
        end();
    }
    let label = with(|s| match s.phase {
        Phase::Live { since: Some(t) } => elapsed(t.elapsed()),
        Phase::Stopping { length, .. } => elapsed(length),
        _ => "●".to_string(),
    });
    match label {
        Some(l) => {
            app().set_recording_indicator(Some(&l));
            after(0.2, watch);
        }
        None => app().set_recording_indicator(None),
    }
}

fn handle(e: Event) {
    if std::env::var_os("SHOT_DEBUG").is_some() {
        let name = match &e {
            Event::Content(r) => format!("content ok={}", r.is_ok()),
            Event::CaptureStarted(r) => format!("capture started {r:?}"),
            Event::RecordingStarted => "recording started".into(),
            Event::Finished => "finished".into(),
            Event::Error(m) => format!("error {m}"),
            Event::CaptureStopped => "capture stopped".into(),
        };
        eprintln!("recording: {name}");
    }
    match e {
        Event::Content(result) => {
            if with(|s| s.cancelled) == Some(true) {
                return end();
            }
            let result = result.and_then(|c| begin(&c.0));
            if let Err(e) = result {
                with(|s| s.error = Some(e));
                end();
            }
        }
        Event::CaptureStarted(Ok(())) => {}
        Event::CaptureStarted(Err(e)) => {
            // Nothing was captured, so there's no file to wait for.
            with(|s| {
                s.error = Some(e);
                s.stream = None;
            });
            end();
        }
        Event::RecordingStarted => {
            with(|s| {
                if let Phase::Live { since: since @ None } = &mut s.phase {
                    *since = Some(Instant::now());
                }
            });
        }
        Event::Finished => {
            with(|s| {
                // Finished on its own while live: stop the capture too.
                request_stop(s);
                if let Phase::Stopping { finished, .. } = &mut s.phase {
                    *finished = true;
                }
            });
            end_when_done();
        }
        Event::Error(e) => {
            eprintln!("recording: {e}");
            with(|s| {
                s.error.get_or_insert(e);
                request_stop(s);
            });
        }
        Event::CaptureStopped => {
            with(|s| {
                if let Phase::Stopping { stopped, .. } = &mut s.phase {
                    *stopped = true;
                }
            });
            end_when_done();
        }
    }
}

/// Builds and starts the stream once ScreenCaptureKit has listed what's on screen.
fn begin(content: &SCShareableContent) -> Result<(), String> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let (target, path, tx) = with(|s| (s.target, s.path.clone(), s.tx.clone())).ok_or("No recording")?;
    let app = app();
    let (system_audio, mic, clicks) = {
        let s = app.settings.borrow();
        (s.record_system_audio, s.record_microphone, s.record_show_clicks)
    };
    let displays = capture::displays(mtm);
    let scale_at = |p: Pt| displays.iter().find(|d| d.bounds.contains(p)).or(displays.first()).map_or(2.0, |d| d.scale);

    let config = unsafe { SCStreamConfiguration::new() };
    let (filter, size, scale) = unsafe {
        match target {
            Target::Window { id, bounds } => {
                let window = content
                    .windows()
                    .iter()
                    .find(|w| w.windowID() == id)
                    .ok_or("That window is no longer on screen.")?;
                let filter = SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window);
                config.setIgnoreShadowsSingleWindow(true);
                (filter, bounds, scale_at(bounds.center()))
            }
            Target::Area(r) => {
                let (filter, display) = display_filter(content, r.center())?;
                let local = Rect::new(r.x - display.x, r.y - display.y, r.w, r.h);
                config.setSourceRect(to_cg(local));
                (filter, r, scale_at(r.center()))
            }
            Target::Display(p) => {
                let (filter, display) = display_filter(content, p)?;
                (filter, display, scale_at(p))
            }
        }
    };
    // H.264 needs even dimensions.
    let px = |v: f32| ((v as f64 * scale).round() as usize / 2 * 2).max(2);
    unsafe {
        config.setWidth(px(size.w));
        config.setHeight(px(size.h));
        config.setMinimumFrameInterval(CMTime { value: 1, timescale: 60, flags: CMTimeFlags::Valid, epoch: 0 });
        config.setQueueDepth(8);
        config.setShowsCursor(true);
        config.setShowMouseClicks(clicks);
        config.setCapturesAudio(system_audio);
        config.setExcludesCurrentProcessAudio(true);
        config.setSampleRate(48_000);
        config.setChannelCount(2);
        config.setCaptureMicrophone(mic);
    }

    let delegate = Delegate::new(tx.clone());
    let rec_config = unsafe { SCRecordingOutputConfiguration::new() };
    unsafe { rec_config.setOutputURL(&NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))) };
    let output = unsafe {
        SCRecordingOutput::initWithConfiguration_delegate(
            SCRecordingOutput::alloc(),
            &rec_config,
            ProtocolObject::from_ref(&*delegate),
        )
    };
    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &config,
            Some(ProtocolObject::from_ref(&*delegate)),
        )
    };
    unsafe { stream.addRecordingOutput_error(&output) }.map_err(|e| e.localizedDescription().to_string())?;
    // From here on the session owns a stream and must stop it before ending.
    with(|s| {
        s.phase = Phase::Live { since: None };
        s.stream = Some(stream.clone());
        s.output = Some(output);
        s.delegate = Some(delegate);
    });
    let started = RcBlock::new(move |error: *mut NSError| {
        let result = if error.is_null() { Ok(()) } else { Err(describe(error)) };
        let _ = tx.send(Event::CaptureStarted(result));
    });
    unsafe { stream.startCaptureWithCompletionHandler(Some(&started)) };
    if app.settings.borrow().play_sound {
        play_sound("Tink");
    }
    Ok(())
}

/// A filter for the display containing `p`, minus Shot's own windows (the
/// selection overlay, toasts, pins), and that display's bounds.
fn display_filter(content: &SCShareableContent, p: Pt) -> Result<(Retained<SCContentFilter>, Rect), String> {
    unsafe {
        let displays = content.displays();
        let display = displays
            .iter()
            .find(|d| from_cg(d.frame()).contains(p))
            .or_else(|| displays.firstObject())
            .ok_or("No display to record.")?;
        let me = std::process::id() as i32;
        let ours: Vec<_> = content.applications().iter().filter(|a| a.processID() == me).collect();
        let filter = SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
            SCContentFilter::alloc(),
            &display,
            &NSArray::from_retained_slice(&ours),
            &NSArray::new(),
        );
        Ok((filter, from_cg(display.frame())))
    }
}

/// Ends the session once the file is finished and the capture stopped.
fn end_when_done() {
    let done = with(|s| matches!(s.phase, Phase::Stopping { finished: true, stopped: true, .. }));
    if done == Some(true) {
        end();
    }
}

/// Ends the session: reports the result and runs the pending `then`.
fn end() {
    let Some(s) = SESSION.with(|s| s.borrow_mut().take()) else { return };
    app().set_recording_indicator(None);
    // Ending while live (the output finished on its own): stop capturing too.
    if let (Phase::Live { .. }, Some(stream)) = (&s.phase, &s.stream) {
        unsafe { stream.stopCaptureWithCompletionHandler(None) };
    }
    let has_file = std::fs::metadata(&s.path).is_ok_and(|m| m.len() > 0);
    let recorded = has_file && matches!(s.phase, Phase::Stopping { .. } | Phase::Live { since: Some(_) });
    if recorded {
        if app().settings.borrow().play_sound {
            play_sound("Screen Capture");
        }
        match &s.error {
            Some(e) => {
                alert("Recording stopped early", &format!("{e}\n\nWhat was recorded is in {}.", s.path.display()))
            }
            None => {
                let dir = s.path.parent().and_then(|d| d.file_name()).map_or("folder".into(), |n| n.to_string_lossy());
                toast(&format!("Saved recording to {dir}"));
            }
        }
        ui::reveal_in_finder(&s.path);
    } else {
        // Nothing usable was written.
        let _ = std::fs::remove_file(&s.path);
        if let Some(e) = &s.error {
            eprintln!("recording failed: {e}");
            alert("Recording failed", e);
        }
    }
    // Keep the stream objects alive until here; a stream dropped while
    // capturing keeps writing on its own.
    drop((s.stream, s.output, s.delegate));
    if let Some(then) = s.then {
        then();
    }
}

fn output_path() -> Option<PathBuf> {
    let dir = app().save_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let base = format!("Shot Recording {}", crate::app::timestamp());
    (0..)
        .map(|i| dir.join(if i == 0 { format!("{base}.mp4") } else { format!("{base} ({i}).mp4") }))
        .find(|p| !p.exists())
}

fn elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn describe(error: *mut NSError) -> String {
    match unsafe { error.as_ref() } {
        Some(e) => e.localizedDescription().to_string(),
        None => "Unknown error".into(),
    }
}

fn to_cg(r: Rect) -> CGRect {
    crate::cg::rect(r.x as f64, r.y as f64, r.w as f64, r.h as f64)
}

fn from_cg(r: CGRect) -> Rect {
    Rect::new(r.origin.x as f32, r.origin.y as f32, r.size.width as f32, r.size.height as f32)
}

// ---------------------------------------------------------------- delegate

struct Ivars {
    tx: Mutex<mpsc::Sender<Event>>,
}

define_class!(
    // Called on ScreenCaptureKit's queues, never the main thread.
    #[unsafe(super(NSObject))]
    #[name = "ShotRecordingDelegate"]
    #[ivars = Ivars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl SCStreamDelegate for Delegate {
        #[unsafe(method(stream:didStopWithError:))]
        fn stream_did_stop(&self, _stream: &SCStream, error: &NSError) {
            self.send(Event::Error(error.localizedDescription().to_string()));
        }
    }

    unsafe impl SCRecordingOutputDelegate for Delegate {
        #[unsafe(method(recordingOutputDidStartRecording:))]
        fn did_start(&self, _output: &SCRecordingOutput) {
            self.send(Event::RecordingStarted);
        }

        #[unsafe(method(recordingOutput:didFailWithError:))]
        fn did_fail(&self, _output: &SCRecordingOutput, error: &NSError) {
            self.send(Event::Error(error.localizedDescription().to_string()));
        }

        #[unsafe(method(recordingOutputDidFinishRecording:))]
        fn did_finish(&self, _output: &SCRecordingOutput) {
            self.send(Event::Finished);
        }
    }
);

impl Delegate {
    fn new(tx: mpsc::Sender<Event>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { tx: Mutex::new(tx) });
        unsafe { msg_send![super(this), init] }
    }

    fn send(&self, e: Event) {
        if let Ok(tx) = self.ivars().tx.lock() {
            let _ = tx.send(e);
        }
    }
}
