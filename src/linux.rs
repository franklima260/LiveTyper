//! KDE Wayland integration for the shared dictation engine.

mod pad;

use std::cell::RefCell;
use std::io::Write;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use gtk4::{gio, glib, prelude::*};

use crate::audio::{AudioBuffer, Capture, KeepOpen, SAMPLE_RATE};
use crate::dictation::{Action, Dictation, Event, Next, State};
use crate::draft::DraftView;
use crate::engine::{Engine, EngineConfig, EngineEvent};
use crate::hotkey::{Hotkeys, KeyEvent};
use crate::settings::{MicrophoneOpen, Settings};

use pad::{ScratchPad, Status};

const FRAME: Duration = Duration::from_millis(33);
const MODIFIER_WAIT: Duration = Duration::from_secs(3);

enum Message {
    Key(KeyEvent, Instant),
    Engine(EngineEvent),
    MicrophoneError(String),
    OutputFailed(String),
    OutputDone,
}

struct PendingOutput {
    text: String,
    until: Instant,
}

struct App {
    settings: Settings,
    hotkeys: Hotkeys,
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
    last_activity: Instant,
    models_ready: bool,
    deadline: Option<Instant>,
    message_until: Option<Instant>,
}

pub fn run(settings: Settings, warning: Option<String>) -> Result<(), String> {
    gtk4::init().map_err(|e| e.to_string())?;
    if !gtk4_layer_shell::is_supported() {
        return Err("LiveTyper needs a Wayland compositor with layer-shell support.".into());
    }

    let application = gtk4::Application::builder()
        .application_id("io.github.franklima260.LiveTyper")
        .build();
    application
        .register(gio::Cancellable::NONE)
        .map_err(|e| e.to_string())?;
    if application.is_remote() {
        return Ok(());
    }

    let (sender, receiver) = mpsc::channel();
    let keys = sender.clone();
    let hotkeys = Hotkeys::new(&settings.talk_key, move |event| {
        let _ = keys.send(Message::Key(event, Instant::now()));
    })?;

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
    let speech = sender.clone();
    let engine = Engine::new(
        EngineConfig {
            models_dir: settings.models_dir(),
            lookahead_ms: settings.lookahead_ms,
            recast_interval: Duration::from_millis(settings.recast_interval_ms.into()),
            recast_window_ms: i64::from(settings.recast_window_s) * 1000,
            live_threads: settings.live_threads,
            recast_threads: settings.recast_threads,
        },
        move |event| {
            let _ = speech.send(Message::Engine(event));
        },
    );

    let now = Instant::now();
    let mut app = App {
        pad: ScratchPad::new(&application, settings.scratch_pad_width),
        dictation: Dictation::new(Duration::from_millis(settings.min_hold_ms.into())),
        settings,
        hotkeys,
        capture,
        engine,
        sender,
        session_bus: application.dbus_connection(),
        generation: 0,
        audio: None,
        view: DraftView::default(),
        final_text: None,
        output: None,
        key_down_at: now,
        last_activity: now,
        models_ready: false,
        deadline: None,
        message_until: None,
    };
    if let Some(message) = warning {
        app.show_message(&message);
    }
    let app = Rc::new(RefCell::new(app));
    let main_loop = glib::MainLoop::new(None, false);
    let quit = main_loop.clone();
    app.borrow().pad.connect_close(move || quit.quit());
    let tick = app.clone();
    let source = glib::timeout_add_local(FRAME, move || {
        let mut app = tick.borrow_mut();
        for message in receiver.try_iter() {
            app.receive(message);
        }
        app.tick();
        glib::ControlFlow::Continue
    });
    main_loop.run();
    source.remove();
    Ok(())
}

impl App {
    fn receive(&mut self, message: Message) {
        match message {
            Message::Key(KeyEvent::TalkDown, at) => {
                self.key_down_at = at;
                self.step(Event::TalkKeyDown);
            }
            Message::Key(KeyEvent::TalkUp, at) => {
                self.step(Event::TalkKeyUp {
                    held: at.saturating_duration_since(self.key_down_at),
                });
            }
            Message::Key(KeyEvent::Escape, _) => self.step(Event::Escape),
            Message::Engine(EngineEvent::Ready { generation }) => {
                if generation != self.generation {
                    return;
                }
                self.models_ready = true;
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
                self.view = view;
                if self.dictation.next() == Next::None {
                    self.pad.draft(&self.view);
                }
            }
            Message::Engine(EngineEvent::Final {
                generation,
                text,
                stats,
            }) if generation == self.generation && self.dictation.state() == State::Finishing => {
                log::info!(
                    "dictation: audio {:.1} s, {} words, model load {:?} ms, first text {:?} ms, {} recasts, last recast {:?} ms",
                    stats.audio_ms as f64 / 1000.0,
                    stats.final_words,
                    stats.load_ms,
                    stats.first_text_ms,
                    stats.recasts,
                    stats.final_recast_ms,
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
            Message::OutputFailed(text) => self.copy_output(&text),
            Message::OutputDone => self.step(Event::PasteDone),
        }
    }

    fn step(&mut self, event: Event) {
        match self.dictation.handle(event) {
            Action::None => {}
            Action::Start => {
                self.start_capture();
                self.start_engine(false);
            }
            Action::Cancel => {
                self.capture.stop();
                self.engine.cancel(self.generation);
                self.deadline = None;
                self.audio = None;
                self.last_activity = Instant::now();
                self.hotkeys.set_escape(false);
                self.pad.hide();
            }
            Action::Finish => self.finish_capture(true),
            Action::Paste => {
                self.engine.cancel(self.generation);
                self.deadline = None;
                self.last_activity = Instant::now();
                if self.dictation.next() == Next::None {
                    self.audio = None;
                    self.hotkeys.set_escape(false);
                    self.pad.hide();
                }
                let text = self.final_text.take().unwrap_or_else(|| self.view.full());
                if text.trim().is_empty() {
                    self.step(Event::PasteDone);
                } else {
                    self.output = Some(PendingOutput {
                        text,
                        until: Instant::now() + MODIFIER_WAIT,
                    });
                }
            }
            Action::StartNext => self.start_capture(),
            Action::CancelNext => {
                self.capture.stop();
                self.audio = None;
                if self.dictation.state() == State::Finishing {
                    self.pad.show(Status::Finishing);
                    self.pad.draft(&self.view);
                } else {
                    self.hotkeys.set_escape(false);
                    self.pad.hide();
                }
            }
            Action::FinishNext => self.finish_capture(false),
            Action::Resume { cancel, recorded } => {
                if cancel {
                    self.engine.cancel(self.generation);
                }
                self.deadline = None;
                self.start_engine(recorded);
            }
        }
    }

    fn start_capture(&mut self) {
        let audio = Arc::new(AudioBuffer::default());
        self.capture.start(audio.clone());
        self.audio = Some(audio);
        self.message_until = None;
        self.hotkeys.set_escape(true);
        self.pad.show(if self.models_ready {
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
        if recorded {
            self.engine.stop(self.generation);
            if self.models_ready {
                self.set_deadline(self.audio_duration() / 2);
            }
        }
    }

    fn finish_capture(&mut self, current: bool) {
        self.capture.stop();
        self.pad.status(Status::Finishing);
        if current {
            self.engine.stop(self.generation);
            if self.models_ready {
                self.set_deadline(Duration::ZERO);
            }
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

    fn tick(&mut self) {
        let now = Instant::now();
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            log::info!(
                "release deadline of {} ms passed: inserting the current Draft",
                self.settings.release_deadline_ms
            );
            self.step(Event::DraftFinal);
        }
        if self.message_until.is_some_and(|deadline| now >= deadline) {
            self.message_until = None;
            self.pad.hide();
        }
        if let Some(audio) = &self.audio {
            self.pad.update_meter(audio.level());
        }
        if self.output.is_some() && self.hotkeys.modifiers_released() {
            let pending = self.output.take().unwrap();
            if pending
                .text
                .bytes()
                .all(|c| c.is_ascii_graphic() || c == b' ')
            {
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
