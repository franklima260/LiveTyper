//! Linux desktop integration for the shared dictation engine.

mod a11y;
mod pad;
mod portal;
mod trace_window;
mod tray;

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use gtk4::{gio, glib, prelude::*};

use crate::audio::{AudioBuffer, Capture, KeepOpen, SAMPLE_RATE};
use crate::dictation::{Action, Dictation, Event, Next, State, insertion_text};
use crate::draft::DraftView;
use crate::engine::{Engine, EngineConfig, EngineEvent};
use crate::hotkey::{Hotkeys, KeyEvent};
use crate::settings::{MicrophoneOpen, Settings};
use crate::trace::{EngineTrace, PasteTrace, ReadBack, TraceStore};

use trace_window::SharedStore;

use pad::{ScratchPad, Status};

const APP_ID: &str = "io.github.franklima260.LiveTyper";
const FRAME: Duration = Duration::from_millis(33);
const MODIFIER_WAIT: Duration = Duration::from_secs(3);
const PORTAL_FOCUS_FALLBACK: Duration = Duration::from_millis(350);
const PORTAL_FOCUS_TIMEOUT: Duration = Duration::from_secs(1);
const PORTAL_FOCUS_POLL: Duration = Duration::from_millis(20);
/// The Text Trace reads the Target Text Box back this long after a paste.
const READ_BACK_DELAY: Duration = Duration::from_millis(300);
const TEXT_TRACE_ACTION: &str = "text-trace";

enum Message {
    Key(KeyEvent, Instant),
    Engine(EngineEvent),
    MicrophoneError(String),
    OutputFailed(String),
    OutputDone,
    PortalOutputFailed { text: String, error: String },
    TogglePause,
}

struct PendingOutput {
    text: String,
    until: Instant,
}

/// The Text Trace, when `text_trace` is on.
#[derive(Clone)]
struct Trace {
    store: SharedStore,
    focus: Rc<RefCell<Option<Rc<a11y::FocusTracker>>>>,
}

impl Trace {
    fn record_paste(&self, id: Option<u64>, paste: PasteTrace) {
        let Some(id) = id else { return };
        if let Some(item) = self.store.borrow_mut().get_mut(id) {
            item.paste = Some(paste);
        }
    }

    fn has_paste(&self, id: Option<u64>) -> bool {
        id.is_some_and(|id| {
            self.store
                .borrow()
                .items()
                .any(|item| item.id == id && item.paste.is_some())
        })
    }

    fn sent_chars(&self, id: Option<u64>) -> Option<usize> {
        let id = id?;
        let store = self.store.borrow();
        let item = store.items().find(|item| item.id == id)?;
        item.paste.as_ref().map(|paste| paste.sent.chars().count())
    }

    fn set_paste_result(&self, id: Option<u64>, result: &str) {
        let Some(id) = id else { return };
        let mut store = self.store.borrow_mut();
        if let Some(paste) = store.get_mut(id).and_then(|item| item.paste.as_mut()) {
            paste.result = result.to_owned();
        }
    }

    /// Reads the Target Text Box back shortly after a paste.
    fn read_back(&self, id: Option<u64>, sent_chars: usize) {
        let Some(id) = id else { return };
        let trace = self.clone();
        glib::spawn_future_local(async move {
            glib::timeout_future(READ_BACK_DELAY).await;
            let focus = trace.focus.borrow().clone();
            let read = match focus {
                Some(focus) => focus.read_back(sent_chars).await,
                None => ReadBack::Unavailable("the accessibility bus is not connected".into()),
            };
            if let Some(item) = trace.store.borrow_mut().get_mut(id) {
                item.read_back = Some(read);
            }
        });
    }
}

struct App {
    settings: Settings,
    keyboard: Keyboard,
    last_portal_paste: Rc<Cell<Option<(u32, Instant)>>>,
    capture: Capture,
    engine: Engine,
    pad: ScratchPad,
    sender: Sender<Message>,
    session_bus: Option<gio::DBusConnection>,
    dictation: Dictation,
    generation: u64,
    audio: Option<Arc<AudioBuffer>>,
    view: DraftView,
    final_text: Option<String>,
    output: Option<PendingOutput>,
    key_down_at: Instant,
    recording_started_at: Instant,
    next_recording_started_at: Instant,
    last_activity: Instant,
    models_ready: bool,
    deadline: Option<Instant>,
    released_at: Instant,
    next_released_at: Option<Instant>,
    selection: Option<&'static str>,
    message_until: Option<Instant>,
    trace: Option<Trace>,
    /// The engine trace of the Dictation that finishes, until it is pasted.
    engine_trace: Option<EngineTrace>,
    /// The Text Trace entry of the text that is being pasted.
    trace_id: Option<u64>,
    tray: Option<tray::Tray>,
    /// Dictation is paused from the tray menu: the Talk Key does nothing and the microphone is closed.
    paused: bool,
    /// Why the last Dictation could not run (no microphone, no models), until one works again.
    problem: Option<String>,
}

enum Keyboard {
    Evdev(Hotkeys),
    Portal { shortcuts: portal::Shortcuts },
}

impl Keyboard {
    fn set_escape(&self, enabled: bool) {
        if let Self::Evdev(hotkeys) = self {
            hotkeys.set_escape(enabled);
        }
    }

    fn modifiers_released(&self) -> bool {
        match self {
            Self::Evdev(hotkeys) => hotkeys.modifiers_released(),
            Self::Portal { .. } => true,
        }
    }
}

pub fn run(settings: Settings, warning: Option<String>) -> Result<(), String> {
    gtk4::init().map_err(|e| e.to_string())?;
    let layer_shell = gtk4_layer_shell::is_supported();

    let open_trace = std::env::args().any(|arg| arg == "--text-trace");
    let application = gtk4::Application::builder().application_id(APP_ID).build();
    // Memory only; it exists only while the setting is on (ADR 0010).
    let trace_store: Option<SharedStore> = settings.text_trace.then(|| {
        Rc::new(RefCell::new(TraceStore::new(
            settings.text_trace_keep as usize,
        )))
    });
    let action = gio::SimpleAction::new(TEXT_TRACE_ACTION, None);
    {
        let application = application.clone();
        let store = trace_store.clone();
        action.connect_activate(move |_, _| trace_window::open(&application, store.clone()));
    }
    application.add_action(&action);
    application
        .register(gio::Cancellable::NONE)
        .map_err(|e| e.to_string())?;
    if application.is_remote() {
        if open_trace {
            // Opens the window in the LiveTyper that already runs.
            application.activate_action(TEXT_TRACE_ACTION, None);
            if let Some(bus) = application.dbus_connection() {
                let _ = bus.flush_sync(gio::Cancellable::NONE);
            }
        }
        return Ok(());
    }
    if trace_store.is_some() {
        log::info!("Text Trace is on: the last Dictations are kept in memory only");
    }

    let (sender, receiver) = mpsc::channel();
    let keys = sender.clone();
    let on_key = move |event| {
        let _ = keys.send(Message::Key(event, Instant::now()));
    };
    let (keyboard, session_bus) = if layer_shell {
        (
            Keyboard::Evdev(Hotkeys::new(&settings.talk_key, on_key.clone())?),
            application.dbus_connection(),
        )
    } else {
        let trigger = crate::hotkey::Chord::parse(&settings.talk_key)?.portal_trigger();
        let (bus, shortcuts) = glib::MainContext::default().block_on(async {
            let bus = portal::connect(APP_ID).await?;
            let shortcuts = portal::Shortcuts::new(&bus, &trigger, on_key.clone()).await?;
            Ok::<_, String>((bus, shortcuts))
        })?;
        log::info!("Talk Key uses the desktop Global Shortcuts portal");
        (Keyboard::Portal { shortcuts }, Some(bus))
    };

    let microphone = sender.clone();
    let keep_open = match settings.microphone_open {
        MicrophoneOpen::Always => KeepOpen::Always,
        MicrophoneOpen::AfterUse => KeepOpen::After(Duration::from_secs(
            u64::from(settings.microphone_open_after_use_min) * 60,
        )),
        MicrophoneOpen::Dictation => KeepOpen::Never,
    };
    let capture = Capture::new(settings.microphone.clone(), keep_open, move |message| {
        let _ = microphone.send(Message::MicrophoneError(message));
    });
    // On small GNOME systems, leave CPU headroom for the desktop. A positive
    // setting overrides this default, and layer-shell desktops retain automatic sizing.
    let recast_threads = if matches!(keyboard, Keyboard::Portal { .. })
        && settings.recast_threads == 0
        && thread::available_parallelism().is_ok_and(|cores| cores.get() <= 4)
    {
        1
    } else {
        settings.recast_threads
    };
    let speech = sender.clone();
    let engine = Engine::new(
        EngineConfig {
            models_dir: settings.models_dir(),
            lookahead_ms: settings.lookahead_ms,
            recast_interval: Duration::from_millis(settings.recast_interval_ms.into()),
            recast_window_ms: i64::from(settings.recast_window_s) * 1000,
            live_threads: settings.live_threads,
            recast_threads,
            text_trace: settings.text_trace,
        },
        move |event| {
            let _ = speech.send(Message::Engine(event));
        },
    );

    let now = Instant::now();
    let mut app = App {
        pad: ScratchPad::new(
            &application,
            settings.scratch_pad_width,
            layer_shell,
            on_key,
        ),
        dictation: Dictation::new(Duration::from_millis(settings.min_hold_ms.into())),
        settings,
        keyboard,
        last_portal_paste: Rc::default(),
        capture,
        engine,
        sender,
        session_bus,
        generation: 0,
        audio: None,
        view: DraftView::default(),
        final_text: None,
        output: None,
        key_down_at: now,
        recording_started_at: now,
        next_recording_started_at: now,
        last_activity: now,
        models_ready: false,
        deadline: None,
        released_at: now,
        next_released_at: None,
        selection: None,
        message_until: None,
        trace: trace_store.map(|store| Trace {
            store,
            focus: Rc::default(),
        }),
        engine_trace: None,
        trace_id: None,
        tray: None,
        paused: false,
        problem: None,
    };
    if let (Some(trace), Some(bus)) = (&app.trace, &app.session_bus) {
        let focus = trace.focus.clone();
        let bus = bus.clone();
        glib::spawn_future_local(async move {
            match a11y::FocusTracker::new(&bus).await {
                Ok(tracker) => {
                    focus.replace(Some(Rc::new(tracker)));
                }
                Err(error) => log::warn!("Text Trace read-back unavailable: {error}"),
            }
        });
    }
    if open_trace {
        application.activate_action(TEXT_TRACE_ACTION, None);
    }
    if let Some(message) = warning {
        app.show_message(&message);
    }
    let app = Rc::new(RefCell::new(app));
    let main_loop = glib::MainLoop::new(None, false);
    let quit = main_loop.clone();
    app.borrow().pad.connect_close(move || quit.quit());
    let tray_bus = app.borrow().session_bus.clone();
    if let Some(bus) = tray_bus {
        let quit = main_loop.clone();
        let application = application.clone();
        let sender = app.borrow().sender.clone();
        match tray::Tray::new(&bus, move |command| match command {
            tray::MenuCommand::TextTrace => application.activate_action(TEXT_TRACE_ACTION, None),
            tray::MenuCommand::TogglePause => {
                let _ = sender.send(Message::TogglePause);
            }
            tray::MenuCommand::OpenSettings => open_file(&crate::paths::settings_file()),
            tray::MenuCommand::OpenLog => open_file(&crate::paths::log_file()),
            tray::MenuCommand::Quit => quit.quit(),
        }) {
            Ok(tray) => app.borrow_mut().tray = Some(tray),
            Err(error) => log::warn!("{error}"),
        }
    }
    let tick = app.clone();
    let source = glib::timeout_add_local(FRAME, move || {
        let mut app = tick.borrow_mut();
        for message in receiver.try_iter() {
            app.receive(message);
        }
        app.tick();
        app.pad.flush_overlay();
        if let Keyboard::Portal { shortcuts } = &app.keyboard
            && let Some(shell) = app.pad.shell_bus()
        {
            let sender = app.sender.clone();
            shortcuts.poll_control(&shell, move |event| {
                let _ = sender.send(Message::Key(event, Instant::now()));
            });
        }
        glib::ControlFlow::Continue
    });
    main_loop.run();
    source.remove();
    Ok(())
}

impl App {
    fn set_escape(&self, enabled: bool) {
        self.keyboard.set_escape(enabled);
        self.pad.set_escape(enabled);
    }

    fn receive(&mut self, message: Message) {
        match message {
            Message::Key(KeyEvent::TalkDown, _) if self.paused => {
                self.show_message("Dictation is paused. Resume it from the tray menu.");
            }
            Message::Key(KeyEvent::TalkUp, _) if self.paused => {}
            Message::Key(KeyEvent::TalkDown, at) => {
                self.key_down_at = at;
                self.step(Event::TalkKeyDown);
            }
            Message::Key(KeyEvent::TalkUp, at) => {
                self.step(Event::TalkKeyUp {
                    held: at.saturating_duration_since(self.key_down_at),
                });
            }
            Message::Key(KeyEvent::Escape, _) => {
                if self.dictation.next() == Next::None && self.output.take().is_some() {
                    self.set_escape(false);
                    self.step(Event::PasteDone);
                } else {
                    self.step(Event::Escape);
                }
            }
            Message::Engine(EngineEvent::Ready { generation }) => {
                if generation != self.generation {
                    return;
                }
                self.models_ready = true;
                self.problem = None;
                match self.dictation.state() {
                    State::Recording => self.pad.status(Status::Listening),
                    State::Finishing if self.deadline.is_none() => {
                        self.set_deadline(self.audio_duration() / 2);
                    }
                    _ => {}
                }
            }
            Message::Engine(EngineEvent::Loading { generation })
                if generation == self.generation && self.dictation.state() == State::Recording =>
            {
                self.pad.status(Status::Loading);
            }
            Message::Engine(EngineEvent::Draft { generation, view })
                if generation == self.generation
                    && matches!(self.dictation.state(), State::Recording | State::Finishing) =>
            {
                if self.expire_deadline() {
                    return;
                }
                self.problem = None;
                self.view = view;
                if self.dictation.next() == Next::None {
                    self.pad.draft(&self.view);
                }
            }
            Message::Engine(EngineEvent::Final {
                generation,
                text,
                mut stats,
            }) if generation == self.generation && self.dictation.state() == State::Finishing => {
                if self.expire_deadline() {
                    return;
                }
                self.engine_trace = stats.trace.take().map(|trace| *trace);
                self.problem = None;
                self.selection = Some(stats.final_selection.as_str());
                log::info!(
                    "dictation: audio {:.1} s, {} words, model load {:?} ms, first text {:?} ms, {} recasts, last recast {:?} ms, preview cancellation {:?} ms",
                    stats.audio_ms as f64 / 1000.0,
                    stats.final_words,
                    stats.load_ms,
                    stats.first_text_ms,
                    stats.recasts,
                    stats.final_recast_ms,
                    stats.preview_cancel_ms,
                );
                self.final_text = Some(text);
                self.step(Event::DraftFinal);
            }
            Message::Engine(EngineEvent::Error {
                generation,
                message,
            }) if generation == self.generation => {
                if matches!(self.dictation.state(), State::Recording | State::Finishing) {
                    self.fail(&message);
                }
            }
            Message::Engine(_) => {}
            Message::MicrophoneError(message) => {
                if self.dictation.next() != Next::None {
                    log::warn!("{message}");
                    self.step(Event::Escape);
                } else if matches!(self.dictation.state(), State::Recording | State::Finishing) {
                    self.fail(&message);
                }
            }
            Message::TogglePause => self.toggle_pause(),
            Message::OutputFailed(text) => self.copy_output(&text),
            Message::OutputDone => {
                if let Some(trace) = &self.trace {
                    trace.set_paste_result(self.trace_id, "ok");
                    if let Some(sent) = trace.sent_chars(self.trace_id) {
                        trace.read_back(self.trace_id, sent);
                    }
                }
                self.step(Event::PasteDone);
            }
            Message::PortalOutputFailed { text, error } => {
                log::warn!("Portal could not insert the dictation: {error}");
                self.step(Event::PasteDone);
                self.pad.show(Status::Message(
                    "Automatic paste unavailable. Last dictation:",
                ));
                self.message_until = None;
                self.pad.draft(&DraftView {
                    checked: text,
                    unchecked: String::new(),
                });
            }
        }
    }

    fn step(&mut self, event: Event) {
        self.handle_action(event);
    }

    /// Shows the tray icon for the current state, with the reason in its tooltip.
    fn update_tray(&self) {
        let Some(tray) = &self.tray else { return };
        let state = self.dictation.state();
        let recording = state == State::Recording || self.dictation.next() == Next::Recording;
        let (icon, tip) = if self.paused {
            (
                tray::IconState::Muted,
                "Paused. Choose Resume dictation in this menu.".to_owned(),
            )
        } else if recording {
            (tray::IconState::Recording, "Recording".to_owned())
        } else if matches!(state, State::Finishing | State::Pasting) {
            (tray::IconState::Busy, "Finishing".to_owned())
        } else if let Some(problem) = &self.problem {
            (tray::IconState::Muted, format!("Cannot dictate: {problem}"))
        } else if self.capture.is_failing() {
            (
                tray::IconState::Muted,
                "The microphone cannot open".to_owned(),
            )
        } else if !self.capture.is_open() {
            (
                tray::IconState::Muted,
                format!(
                    "Microphone closed. It opens when you hold {}.",
                    self.settings.talk_key
                ),
            )
        } else {
            (
                tray::IconState::Ready,
                format!("Hold {} to dictate", self.settings.talk_key),
            )
        };
        tray.set_state(icon, &tip);
    }

    /// Pausing cancels a Dictation that records (one that finishes still pastes) and closes the
    /// microphone. Resuming opens it again if it must be open all the time.
    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        if self.paused
            && (self.dictation.state() == State::Recording
                || self.dictation.next() == Next::Recording)
        {
            self.step(Event::Escape);
        }
        self.capture.pause(self.paused);
        if let Some(tray) = &self.tray {
            tray.set_paused(self.paused);
        }
        self.show_message(if self.paused {
            "Dictation paused. Resume it from the tray menu."
        } else {
            "Dictation resumed."
        });
    }

    fn handle_action(&mut self, event: Event) {
        match self.dictation.handle(event) {
            Action::None => {}
            Action::Start => {
                self.recording_started_at = self.key_down_at;
                self.start_capture();
                self.start_engine(false);
            }
            Action::Cancel => {
                self.capture.stop();
                self.engine.cancel(self.generation);
                self.deadline = None;
                self.audio = None;
                self.last_activity = Instant::now();
                self.set_escape(false);
                self.pad.hide();
            }
            Action::Finish => self.finish_capture(true),
            Action::Paste => {
                self.engine.cancel(self.generation);
                self.deadline = None;
                self.last_activity = Instant::now();
                if self.dictation.next() == Next::None {
                    self.audio = None;
                    self.pad.hide();
                }
                let selection = self.selection.take().unwrap_or("deadline-draft");
                log::info!(
                    "dictation selection: {selection}, {} ms after release",
                    self.released_at.elapsed().as_millis()
                );
                let text = insertion_text(self.final_text.take(), &self.view);
                self.trace_id = self.trace.as_ref().map(|trace| {
                    let time = glib::DateTime::now_local()
                        .and_then(|now| now.format("%H:%M:%S"))
                        .map(|time| time.to_string())
                        .unwrap_or_default();
                    let mut engine = self.engine_trace.take().unwrap_or_else(|| EngineTrace {
                        selection,
                        final_text: text.clone(),
                        ..EngineTrace::default()
                    });
                    if engine.selection != selection {
                        // The release deadline won over the engine's own pick.
                        engine.selection = selection;
                        engine.final_text.clone_from(&text);
                    }
                    trace.store.borrow_mut().push(time, Some(engine))
                });
                if text.is_empty() {
                    if self.dictation.next() == Next::None {
                        self.set_escape(false);
                    }
                    self.step(Event::PasteDone);
                } else {
                    self.output = Some(PendingOutput {
                        text,
                        until: Instant::now() + MODIFIER_WAIT,
                    });
                }
            }
            Action::StartNext => {
                self.next_recording_started_at = self.key_down_at;
                self.next_released_at = None;
                self.start_capture();
            }
            Action::CancelNext => {
                self.capture.stop();
                self.audio = None;
                if self.dictation.state() == State::Finishing {
                    self.pad.show_overlay(Status::Finishing);
                    self.pad.draft(&self.view);
                } else {
                    self.set_escape(self.output.is_some());
                    self.pad.hide();
                }
            }
            Action::FinishNext => self.finish_capture(false),
            Action::Resume { cancel, recorded } => {
                self.recording_started_at = self.next_recording_started_at;
                if cancel {
                    self.engine.cancel(self.generation);
                }
                self.deadline = None;
                self.start_engine(recorded);
            }
        }
    }

    fn start_capture(&mut self) {
        log::info!("Dictation recording started");
        let audio = Arc::new(AudioBuffer::default());
        self.capture.start(audio.clone());
        self.audio = Some(audio);
        self.message_until = None;
        self.set_escape(true);
        self.pad.show_overlay(if self.models_ready {
            Status::Listening
        } else {
            Status::Loading
        });
    }

    fn start_engine(&mut self, recorded: bool) {
        self.generation += 1;
        let audio = self.audio.clone().expect("dictation has a capture buffer");
        self.engine.start(self.generation, audio);
        self.view = DraftView::default();
        self.final_text = None;
        self.selection = None;
        self.engine_trace = None;
        if recorded {
            self.released_at = self.next_released_at.take().unwrap_or_else(Instant::now);
            self.engine.stop(self.generation);
            if self.models_ready {
                self.set_deadline(self.audio_duration() / 2);
            }
        }
    }

    fn finish_capture(&mut self, current: bool) {
        self.capture.stop();
        log::info!(
            "Dictation recording finished ({} ms of audio)",
            self.audio_duration().as_millis()
        );
        self.pad.status(Status::Finishing);
        if current {
            self.released_at = Instant::now();
            self.engine.stop(self.generation);
            if self.models_ready {
                self.set_deadline(Duration::ZERO);
            }
        } else {
            self.next_released_at = Some(Instant::now());
        }
    }

    fn audio_duration(&self) -> Duration {
        let samples = self.audio.as_ref().map_or(0, |audio| audio.len());
        Duration::from_millis((samples * 1000 / SAMPLE_RATE) as u64)
    }

    fn set_deadline(&mut self, extra: Duration) {
        self.deadline = Some(
            Instant::now()
                + Duration::from_millis(self.settings.release_deadline_ms.into())
                + extra,
        );
    }

    fn fail(&mut self, message: &str) {
        log::error!("{message}");
        self.problem = Some(message.trim_end_matches('.').to_owned());
        self.step(Event::Failed);
        if self.dictation.state() == State::Idle {
            self.show_message(message);
        }
    }

    fn show_message(&mut self, message: &str) {
        self.pad.show(Status::Message(message));
        self.message_until = Some(Instant::now() + Duration::from_secs(6));
    }

    fn copy_output(&mut self, text: &str) {
        let copied = self
            .session_bus
            .as_ref()
            .is_some_and(|bus| copy_to_plasma_clipboard(bus, text));
        if let Some(trace) = &self.trace {
            let result = if copied {
                "not typed; copied to the Klipper clipboard for the user to paste"
            } else {
                "not typed, and could not copy"
            };
            if trace.has_paste(self.trace_id) {
                trace.set_paste_result(self.trace_id, result);
            } else {
                trace.record_paste(
                    self.trace_id,
                    PasteTrace {
                        method: "Klipper clipboard copy".into(),
                        follow_up: None,
                        sent: text.to_owned(),
                        result: result.into(),
                    },
                );
            }
        }
        self.step(Event::PasteDone);
        if self.dictation.state() == State::Idle {
            if copied {
                self.show_message("Dictation copied. Paste it in the target app.");
            } else {
                self.show_message("Could not insert or copy the dictation.");
                self.pad.draft(&DraftView {
                    checked: text.to_owned(),
                    unchecked: String::new(),
                });
            }
        } else if !copied {
            log::warn!("Could not insert or copy the last dictation.");
        }
    }

    fn paste_with_portal(&self, text: String) {
        let trace = self.trace.clone();
        let trace_id = self.trace_id;
        let bus = self
            .session_bus
            .clone()
            .expect("portal requires a session bus");
        let shell = self.pad.shell_bus();
        let started = self.recording_started_at;
        let last_paste = self.last_portal_paste.clone();
        let sender = self.sender.clone();
        glib::spawn_future_local(async move {
            let result = async {
                // Remember which editor had focus before the portal can show its dialog.
                let expected = if let Some(shell) = &shell {
                    match pad::paste_target(shell).await {
                        Some((0, _)) => {
                            return Err("No GNOME window is focused for automatic paste.".into());
                        }
                        Some((target, _)) => Some(target),
                        None => None,
                    }
                } else {
                    None
                };
                let session = portal::PasteSession::new(&bus).await?;
                let target = if let (Some(shell), Some(expected)) = (&shell, expected) {
                    let started_waiting = Instant::now();
                    loop {
                        let Some((current, modifiers_held)) = pad::paste_target(shell).await else {
                            return Err("Could not verify GNOME focus for automatic paste.".into());
                        };
                        let elapsed = started_waiting.elapsed();
                        if current == expected {
                            // Shift, Alt or Super would change the synthetic Ctrl+V.
                            if !modifiers_held {
                                break Some(current);
                            }
                            if elapsed >= MODIFIER_WAIT {
                                return Err(
                                    "Modifiers are still held. Paste the draft manually.".into()
                                );
                            }
                        } else if elapsed >= PORTAL_FOCUS_TIMEOUT {
                            return Err("GNOME did not return focus to the original window.".into());
                        }
                        glib::timeout_future(PORTAL_FOCUS_POLL).await;
                    }
                } else {
                    // Without the optional extension, allow GNOME time to dismiss its dialog.
                    glib::timeout_future(PORTAL_FOCUS_FALLBACK).await;
                    None
                };
                let (sent, follow_up) =
                    portal::follow_up_text(&text, target, started, last_paste.get());
                if let Some(trace) = &trace {
                    trace.record_paste(
                        trace_id,
                        PasteTrace {
                            method: "Remote Desktop portal Ctrl+V".into(),
                            follow_up: Some(follow_up),
                            sent: sent.clone(),
                            result: "pending".into(),
                        },
                    );
                }
                session
                    .paste(sent, async {
                        if let (Some(shell), Some(target)) = (&shell, target)
                            && pad::paste_target(shell).await != Some((target, false))
                        {
                            return Err("Focus or modifiers changed before automatic paste.".into());
                        }
                        Ok(())
                    })
                    .await?;
                last_paste.set(target.map(|target| (target, Instant::now())));
                Ok(())
            }
            .await;
            let message = match result {
                Ok(()) => {
                    log::info!("Dictation sent through the Remote Desktop paste portal");
                    Message::OutputDone
                }
                Err(error) => {
                    if let Some(trace) = &trace {
                        if !trace.has_paste(trace_id) {
                            trace.record_paste(
                                trace_id,
                                PasteTrace {
                                    method: "Remote Desktop portal Ctrl+V".into(),
                                    follow_up: None,
                                    sent: text.clone(),
                                    result: String::new(),
                                },
                            );
                        }
                        trace.set_paste_result(trace_id, &format!("failed: {error}"));
                    }
                    Message::PortalOutputFailed { text, error }
                }
            };
            let _ = sender.send(message);
        });
    }

    fn expire_deadline(&mut self) -> bool {
        if self.dictation.state() != State::Finishing
            || !self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return false;
        }
        self.step(Event::DraftFinal);
        true
    }

    fn tick(&mut self) {
        self.expire_deadline();
        self.update_tray();
        let now = Instant::now();
        if self.message_until.is_some_and(|deadline| now >= deadline) {
            self.message_until = None;
            self.pad.hide();
        }
        if let Some(audio) = &self.audio {
            self.pad.update_meter(audio.level());
        }
        if self.output.is_some() && self.keyboard.modifiers_released() {
            let pending = self.output.take().unwrap();
            if self.dictation.next() == Next::None {
                self.set_escape(false);
            }
            if matches!(self.keyboard, Keyboard::Portal { .. }) {
                self.paste_with_portal(pending.text);
            } else if pending
                .text
                .bytes()
                .all(|c| c.is_ascii_graphic() || c == b' ')
            {
                if let Some(trace) = &self.trace {
                    trace.record_paste(
                        self.trace_id,
                        PasteTrace {
                            method: "ydotool type".into(),
                            follow_up: None,
                            sent: pending.text.clone(),
                            result: "pending".into(),
                        },
                    );
                }
                let sender = self.sender.clone();
                let bus = self.session_bus.clone();
                thread::spawn(move || {
                    let event = match type_text(&pending.text, bus.as_ref()) {
                        Ok(()) => Message::OutputDone,
                        Err(error) => {
                            log::warn!("ydotool could not insert the dictation: {error}");
                            Message::OutputFailed(pending.text)
                        }
                    };
                    let _ = sender.send(event);
                });
            } else {
                self.copy_output(&pending.text);
            }
        } else if self
            .output
            .as_ref()
            .is_some_and(|output| now >= output.until)
        {
            let pending = self.output.take().unwrap();
            if self.dictation.next() == Next::None {
                self.set_escape(false);
            }
            self.copy_output(&pending.text);
        }
        if self.dictation.state() == State::Idle
            && self.models_ready
            && self.settings.unload_after_min != 0
            && self.last_activity.elapsed()
                >= Duration::from_secs(u64::from(self.settings.unload_after_min) * 60)
        {
            self.engine.unload();
            self.models_ready = false;
        }
    }
}

// ydotool sends physical US key positions. Use the clipboard if KWin cannot confirm the layout.
fn uses_us_layout(bus: &gio::DBusConnection) -> Option<bool> {
    let query = |method| {
        bus.call_sync(
            Some("org.kde.keyboard"),
            "/Layouts",
            "org.kde.KeyboardLayouts",
            method,
            None,
            None,
            gio::DBusCallFlags::NO_AUTO_START,
            250,
            gio::Cancellable::NONE,
        )
        .ok()
    };
    let layouts = query("getLayoutsList")?
        .try_child_get::<Vec<(String, String, String)>>(0)
        .ok()??;
    let active = query("getLayout")?.try_child_get::<u32>(0).ok()?? as usize;
    Some(is_plain_us_layout(layouts.get(active)?))
}

fn is_plain_us_layout((short_name, _, long_name): &(String, String, String)) -> bool {
    // KWin returns the layout code, the user display name, and the translated XKB name.
    short_name == "us" && long_name == "English (US)"
}

/// Opens a file in its default app.
fn open_file(path: &std::path::Path) {
    let uri = gio::File::for_path(path).uri();
    if let Err(error) = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>) {
        log::error!("cannot open {}: {error}", path.display());
    }
}

fn type_text(text: &str, bus: Option<&gio::DBusConnection>) -> std::io::Result<()> {
    if bus.and_then(uses_us_layout) != Some(true) {
        return Err(std::io::Error::other(
            "the active keyboard layout is not plain US",
        ));
    }
    let mut child = Command::new("ydotool")
        .args([
            "type",
            "--key-delay=1",
            "--key-hold=1",
            "--escape=0",
            "--file=-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let written = child.stdin.take().unwrap().write_all(text.as_bytes());
    let status = child.wait()?;
    written?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(status.to_string()))
    }
}

// A Wayland window with no keyboard focus cannot claim the seat's clipboard.
fn copy_to_plasma_clipboard(bus: &gio::DBusConnection, text: &str) -> bool {
    bus.call_sync(
        Some("org.kde.klipper"),
        "/klipper",
        "org.kde.klipper.klipper",
        "setClipboardContents",
        Some(&(text,).to_variant()),
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        250,
        gio::Cancellable::NONE,
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::is_plain_us_layout;

    #[test]
    fn kde_layout_check_uses_the_long_name_and_ignores_the_display_name() {
        assert!(!is_plain_us_layout(&(
            "us".into(),
            String::new(),
            "English (Dvorak)".into(),
        )));
        assert!(is_plain_us_layout(&(
            "us".into(),
            "Work keyboard".into(),
            "English (US)".into(),
        )));
        assert!(!is_plain_us_layout(&(
            "gb".into(),
            "English (US)".into(),
            "English (UK)".into(),
        )));
    }
}
