//! Screen recording to an MP4 (H.264 + AAC) with ScreenCaptureKit: an area,
//! a window or a whole display, with system audio and/or the microphone.
//!
//! `SCRecordingOutput` writes the file itself, which needs macOS 15. While a
//! recording runs, the menu bar icon turns into a stop button with a timer.
//!
//! ScreenCaptureKit calls back on its own threads, so callbacks only send
//! [`Event`]s over a channel; the main thread polls it and owns all state.

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
    Started,
    Finished,
    Failed(String),
}

/// ScreenCaptureKit objects are thread-safe; this only moves a result from
/// its completion handler to the main thread.
struct Sendable<T>(T);
unsafe impl<T> Send for Sendable<T> {}

enum Phase {
    /// Waiting for the list of displays and windows.
    Preparing,
    Starting,
    Recording(Instant),
    Stopping(Duration),
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
    /// Runs after the file is finished, e.g. quitting the app.
    then: Option<Box<dyn FnOnce()>>,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
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
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
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
            then: None,
        })
    });
    app().set_recording_indicator(Some("●"));
    watch();
}

/// Stops the recording; the file is finished asynchronously.
pub fn stop() {
    stop_then(|| {});
}

/// Stops, and runs `then` once the movie file is complete (or right away
/// when nothing is recording).
pub fn stop_then(then: impl FnOnce() + 'static) {
    if !is_active() {
        return then();
    }
    let (stream, tx) = SESSION.with(|s| {
        let mut s = s.borrow_mut();
        let s = s.as_mut().expect("active session");
        s.then = Some(Box::new(then));
        let stream = match s.phase {
            Phase::Recording(t) => {
                s.phase = Phase::Stopping(t.elapsed());
                s.stream.clone()
            }
            // Not capturing yet: drop the session without a file.
            Phase::Preparing | Phase::Starting => {
                let _ = s.tx.send(Event::Failed(String::new()));
                None
            }
            Phase::Stopping(_) => None,
        };
        (stream, s.tx.clone())
    });
    let Some(stream) = stream else { return };
    // Stopping the capture also finishes the recording output's file;
    // `recordingOutputDidFinishRecording` reports when it's written.
    let done = RcBlock::new(move |error: *mut NSError| {
        if !error.is_null() {
            let _ = tx.send(Event::Failed(describe(error)));
        }
    });
    unsafe { stream.stopCaptureWithCompletionHandler(Some(&done)) };
}

/// Polls the session's events on the main thread and keeps the timer current.
fn watch() {
    let events: Vec<Event> = SESSION.with(|s| match s.borrow().as_ref() {
        Some(s) => s.rx.try_iter().collect(),
        None => vec![],
    });
    for e in events {
        handle(e);
        if !is_active() {
            return;
        }
    }
    let label = SESSION.with(|s| match s.borrow().as_ref().map(|s| &s.phase) {
        Some(Phase::Recording(t)) => Some(elapsed(t.elapsed())),
        Some(Phase::Stopping(d)) => Some(elapsed(*d)),
        Some(_) => Some("●".to_string()),
        None => None,
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
    match e {
        Event::Content(Ok(content)) => {
            if let Err(e) = begin(&content.0) {
                fail(&e);
            }
        }
        Event::Content(Err(e)) => fail(&e),
        Event::Started => SESSION.with(|s| {
            if let Some(s) = s.borrow_mut().as_mut() {
                if matches!(s.phase, Phase::Starting) {
                    s.phase = Phase::Recording(Instant::now());
                }
            }
        }),
        Event::Finished => finish(),
        Event::Failed(e) => fail(&e),
    }
}

/// Builds the stream once ScreenCaptureKit has listed what's on screen.
fn begin(content: &SCShareableContent) -> Result<(), String> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let (target, path, tx) = SESSION.with(|s| {
        let s = s.borrow();
        let s = s.as_ref().expect("session");
        (s.target, s.path.clone(), s.tx.clone())
    });
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
    let started = RcBlock::new(move |error: *mut NSError| {
        if !error.is_null() {
            let _ = tx.send(Event::Failed(describe(error)));
        }
    });
    unsafe { stream.startCaptureWithCompletionHandler(Some(&started)) };
    SESSION.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.phase = Phase::Starting;
            s.stream = Some(stream);
            s.output = Some(output);
            s.delegate = Some(delegate);
        }
    });
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

/// The movie is complete on disk.
fn finish() {
    let Some(s) = take() else { return };
    if app().settings.borrow().play_sound {
        play_sound("Screen Capture");
    }
    let dir = s.path.parent().and_then(|d| d.file_name()).map_or("folder".into(), |n| n.to_string_lossy());
    toast(&format!("Saved recording to {dir}"));
    ui::reveal_in_finder(&s.path);
    if let Some(then) = s.then {
        then();
    }
}

fn fail(message: &str) {
    let Some(s) = take() else { return };
    if !message.is_empty() {
        eprintln!("recording failed: {message}");
    }
    if let Some(stream) = &s.stream {
        unsafe { stream.stopCaptureWithCompletionHandler(None) };
    }
    let started = matches!(s.phase, Phase::Recording(_) | Phase::Stopping(_));
    if started && s.path.exists() {
        // Keep what was recorded; the file may still play.
        if !message.is_empty() {
            alert("Recording stopped", &format!("{message}\n\nWhat was recorded so far is in {}.", s.path.display()));
        }
        ui::reveal_in_finder(&s.path);
    } else {
        // A failed start leaves an empty or unplayable file behind.
        let _ = std::fs::remove_file(&s.path);
        if !message.is_empty() {
            alert("Recording failed", message);
        }
    }
    if let Some(then) = s.then {
        then();
    }
}

fn take() -> Option<Session> {
    let s = SESSION.with(|s| s.borrow_mut().take());
    app().set_recording_indicator(None);
    s
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
    #[ivars = Ivars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl SCStreamDelegate for Delegate {
        #[unsafe(method(stream:didStopWithError:))]
        fn stream_did_stop(&self, _stream: &SCStream, error: &NSError) {
            self.send(Event::Failed(error.localizedDescription().to_string()));
        }
    }

    unsafe impl SCRecordingOutputDelegate for Delegate {
        #[unsafe(method(recordingOutputDidStartRecording:))]
        fn did_start(&self, _output: &SCRecordingOutput) {
            self.send(Event::Started);
        }

        #[unsafe(method(recordingOutput:didFailWithError:))]
        fn did_fail(&self, _output: &SCRecordingOutput, error: &NSError) {
            self.send(Event::Failed(error.localizedDescription().to_string()));
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
