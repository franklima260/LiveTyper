//! The UI thread: the hidden owner window, the tray, the Scratch Pad and the Dictation state machine.
//!
//! Other threads (keys, microphone, engine, paste) send `UiEvent`s. The owner window wakes on
//! `WM_APP_EVENT` and handles them in order, so all UI state lives on this one thread.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EVENT_SYSTEM_FOREGROUND,
    GetClassNameW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, IsWindow, KillTimer,
    MSG, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
    SetForegroundWindow, SetTimer, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE,
    WINEVENT_OUTOFCONTEXT, WM_APP, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WM_TIMER, WNDCLASSW,
};
use windows::core::{Result, w};

use crate::audio::{AudioBuffer, Capture, KeepOpen, SAMPLE_RATE};
use crate::dictation::{Action, Dictation, Event, Next, State, insertion_text};
use crate::draft::DraftView;
use crate::engine::{Engine, EngineConfig, EngineEvent, Stats};
use crate::hotkey::{Hotkeys, KeyEvent};
use crate::icon::{IconState, Icons};
use crate::paste::{self, PasteResult};
use crate::paths;
use crate::scratch_pad::{ScratchPad, Status};
use crate::settings::{MicrophoneOpen, Settings};
use crate::tray::{self, MenuCommand, WM_TRAY};

pub enum UiEvent {
    Key(KeyEvent),
    Engine(EngineEvent),
    MicrophoneError(String),
    Pasted {
        /// The generation of the Dictation, or `None` for Paste Last Dictation.
        dictation: Option<u64>,
        result: PasteResult,
    },
}

const WM_APP_EVENT: u32 = WM_APP + 2;

const TIMER_LEVEL: usize = 1;
const TIMER_DEADLINE: usize = 2;
const TIMER_IDLE: usize = 3;
const TIMER_HIDE_MESSAGE: usize = 4;
const TIMER_PASTE: usize = 5;

const LEVEL_INTERVAL_MS: u32 = 33;
const IDLE_CHECK_MS: u32 = 60_000;
/// A paste that takes longer (for example, a hung clipboard owner) no longer blocks the Talk Key.
const PASTE_GUARD_MS: u32 = 5_000;
const NOT_PASTED_MESSAGE: &str = "Not pasted. Push Ctrl+V.";
/// A Dictation that starts this soon after the last paste into the same window is a follow-up:
/// its text gets a space before it, so that it does not touch the last text.
const FOLLOW_UP_WINDOW: Duration = Duration::from_secs(30);

static EVENTS: OnceLock<Sender<UiEvent>> = OnceLock::new();
static UI_HWND: AtomicIsize = AtomicIsize::new(0);
/// Explorer sends this message when it starts again; the tray icon must then be added again.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
/// The last foreground window of another app, for Paste Last Dictation from the tray menu.
static LAST_TARGET: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Sends an event to the UI thread. Safe to call from any thread.
pub fn post(event: UiEvent) {
    if let Some(sender) = EVENTS.get() {
        let _ = sender.send(event);
        let hwnd = UI_HWND.load(Ordering::Acquire);
        if hwnd != 0 {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(hwnd as *mut _)),
                    WM_APP_EVENT,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }
}

/// Runs `f` on the app, unless the app is already in use further up the stack.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

pub fn paint_scratch_pad() {
    with_app(|app| app.pad.paint());
}

struct App {
    hwnd: HWND,
    settings: Settings,
    dictation: Dictation,
    generation: u64,
    hotkeys: Option<Hotkeys>,
    capture: Capture,
    engine: Engine,
    pad: ScratchPad,
    icons: Icons,
    events: Receiver<UiEvent>,
    audio: Option<Arc<AudioBuffer>>,
    key_down_at: Instant,
    released_at: Instant,
    /// When the Talk Key went up for the next Dictation, while the current one finished.
    next_released_at: Option<Instant>,
    view: DraftView,
    final_text: Option<String>,
    stats: Option<Stats>,
    /// For Paste Last Dictation. In memory only.
    last_text: Option<String>,
    /// The window of the last paste, and when the paste started.
    last_paste: Option<(isize, Instant)>,
    last_activity: Instant,
    /// True until the engine reports that the models are ready, and again after an unload.
    models_unloaded: bool,
    /// The Talk Key is up but the models still load: the release deadline starts when they are ready.
    deadline_waits_for_models: bool,
    deadline: Option<Instant>,
}

/// Creates the windows and the worker threads, then runs the message loop until Quit.
pub fn run(settings: Settings, startup_warning: Option<String>) -> Result<()> {
    let (sender, events) = mpsc::channel();
    let _ = EVENTS.set(sender);

    let hwnd = unsafe {
        let instance = GetModuleHandleW(None)?;
        let class_name = w!("LiveTyperTray");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: class_name,
            ..Default::default()
        });
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(w!("TaskbarCreated")),
            Ordering::Relaxed,
        );
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("LiveTyper"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?
    };
    UI_HWND.store(hwnd.0 as isize, Ordering::Release);

    let icons = Icons::new()?;
    tray::add_icon(hwnd, icons.idle, "LiveTyper")?;

    let mut warning = startup_warning;
    let hotkeys = match Hotkeys::new(&settings.talk_key, |key| post(UiEvent::Key(key))) {
        Ok(h) => Some(h),
        Err(message) => {
            log::error!("{message}");
            warning = Some(message);
            None
        }
    };
    if let Some(text) = &warning {
        tray::show_warning(hwnd, text);
    }

    let keep_open = match settings.microphone_open {
        MicrophoneOpen::Always => KeepOpen::Always,
        MicrophoneOpen::AfterUse => KeepOpen::After(Duration::from_secs(
            u64::from(settings.microphone_open_after_use_min) * 60,
        )),
        MicrophoneOpen::Dictation => KeepOpen::Never,
    };
    let capture = Capture::new(settings.microphone.clone(), keep_open, |message| {
        post(UiEvent::MicrophoneError(message));
    });
    let engine = Engine::new(
        EngineConfig {
            models_dir: settings.models_dir(),
            lookahead_ms: settings.lookahead_ms,
            recast_interval: Duration::from_millis(settings.recast_interval_ms.into()),
            recast_window_ms: i64::from(settings.recast_window_s) * 1000,
            live_threads: settings.live_threads,
            recast_threads: settings.recast_threads,
        },
        |event| post(UiEvent::Engine(event)),
    );
    let anchor = settings.scratch_pad_x.zip(settings.scratch_pad_y);
    let pad = ScratchPad::new(anchor, settings.scratch_pad_width)?;

    unsafe {
        SetTimer(Some(hwnd), TIMER_IDLE, IDLE_CHECK_MS, None);
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
    }

    APP.with(|cell| {
        *cell.borrow_mut() = Some(App {
            hwnd,
            dictation: Dictation::new(Duration::from_millis(settings.min_hold_ms.into())),
            settings,
            generation: 0,
            hotkeys,
            capture,
            engine,
            pad,
            icons,
            events,
            audio: None,
            key_down_at: Instant::now(),
            released_at: Instant::now(),
            next_released_at: None,
            view: DraftView::default(),
            final_text: None,
            stats: None,
            last_text: None,
            last_paste: None,
            last_activity: Instant::now(),
            models_unloaded: true,
            deadline_waits_for_models: false,
            deadline: None,
        })
    });

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    UI_HWND.store(0, Ordering::Release);
    APP.with(|cell| cell.borrow_mut().take());
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAY => {
            let event = (lparam.0 & 0xFFFF) as u32;
            if event == WM_RBUTTONUP || event == WM_LBUTTONUP {
                on_tray_click(hwnd);
            }
            LRESULT(0)
        }
        WM_APP_EVENT => {
            if with_app(App::drain_events).is_none() {
                // The app is busy further up the stack: try again from the message loop.
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_APP_EVENT, WPARAM(0), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            with_app(|app| app.on_timer(wparam.0));
            LRESULT(0)
        }
        WM_DESTROY => {
            tray::remove_icon(hwnd);
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            with_app(|app| {
                if let Err(e) = tray::add_icon(hwnd, app.icons.idle, "LiveTyper") {
                    log::error!("tray icon after Explorer restart: {e}");
                }
            });
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// The menu runs its own message loop, so the app must not be borrowed while it shows.
fn on_tray_click(hwnd: HWND) {
    let can_paste_last =
        with_app(|app| app.last_text.is_some() && app.dictation.state() == State::Idle)
            .unwrap_or(false);
    match tray::show_menu(hwnd, can_paste_last) {
        Some(MenuCommand::PasteLast) => {
            let target = HWND(LAST_TARGET.load(Ordering::Relaxed) as *mut _);
            unsafe {
                if IsWindow(Some(target)).as_bool() {
                    let _ = SetForegroundWindow(target);
                }
            }
            with_app(App::paste_last);
        }
        Some(MenuCommand::OpenSettings) => tray::open_file(&paths::settings_file()),
        Some(MenuCommand::OpenLog) => tray::open_file(&paths::log_file()),
        Some(MenuCommand::Quit) => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        None => {}
    }
}

/// Remembers the foreground window of other apps (not the taskbar, which takes the focus on a tray click).
unsafe extern "system" fn on_foreground(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    let mut pid = 0;
    let mut class = [0u16; 64];
    let class = unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let len = GetClassNameW(hwnd, &mut class);
        String::from_utf16_lossy(&class[..len.max(0) as usize])
    };
    let shell = [
        "Shell_TrayWnd",
        "NotifyIconOverflowWindow",
        "TopLevelWindowForOverflowXamlIsland",
    ];
    if pid != unsafe { GetCurrentProcessId() } && !shell.contains(&class.as_str()) {
        LAST_TARGET.store(hwnd.0 as isize, Ordering::Relaxed);
    }
}

impl App {
    fn drain_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.handle(event);
        }
    }

    fn is_current(&self, generation: u64) -> bool {
        generation == self.generation
            && matches!(self.dictation.state(), State::Recording | State::Finishing)
    }

    fn handle(&mut self, event: UiEvent) {
        match event {
            UiEvent::Key(KeyEvent::TalkDown) => {
                self.key_down_at = Instant::now();
                if self.dictation.next() == Next::Recorded {
                    log::warn!("Talk Key push ignored: a next Dictation already waits");
                }
                self.step(Event::TalkKeyDown);
            }
            UiEvent::Key(KeyEvent::TalkUp) => {
                let held = self.key_down_at.elapsed();
                self.step(Event::TalkKeyUp { held });
            }
            UiEvent::Key(KeyEvent::Escape) => self.step(Event::Escape),
            UiEvent::Engine(EngineEvent::Loading { generation }) => {
                if self.is_current(generation) && self.dictation.state() == State::Recording {
                    self.pad.set_status(Status::Loading);
                }
            }
            UiEvent::Engine(EngineEvent::Ready { generation }) => {
                self.models_unloaded = false;
                if self.is_current(generation) && self.dictation.state() == State::Recording {
                    self.pad.set_status(Status::Listening);
                }
                // Also for an older generation: the models are ready for the current Dictation too.
                if self.deadline_waits_for_models && self.dictation.state() == State::Finishing {
                    self.start_deadline();
                }
            }
            UiEvent::Engine(EngineEvent::Draft { generation, view }) => {
                if self.is_current(generation) && self.expire_deadline() {
                    return;
                }
                if self.is_current(generation) {
                    self.view = view.clone();
                    // While a next Dictation records, the Scratch Pad shows that one.
                    if self.dictation.next() == Next::None {
                        self.pad.set_draft(view);
                    }
                }
            }
            UiEvent::Engine(EngineEvent::Final {
                generation,
                text,
                stats,
            }) => {
                if self.is_current(generation) && self.expire_deadline() {
                    return;
                }
                if self.is_current(generation) && self.dictation.state() == State::Finishing {
                    self.final_text = Some(text);
                    self.stats = Some(stats);
                    self.step(Event::DraftFinal);
                }
            }
            UiEvent::Engine(EngineEvent::Error {
                generation,
                message,
            }) => {
                if self.is_current(generation) {
                    let had_next = self.dictation.next() != Next::None;
                    self.step(Event::Failed);
                    if had_next {
                        log::error!("the last Dictation failed; the next one continues: {message}");
                    } else {
                        self.show_message(message, Duration::from_secs(8));
                    }
                }
            }
            UiEvent::MicrophoneError(message) => {
                if self.dictation.next() != Next::None {
                    // The microphone records for the next Dictation: only that one stops.
                    self.step(Event::Escape);
                    self.show_message(message, Duration::from_secs(5));
                } else if matches!(self.dictation.state(), State::Recording | State::Finishing) {
                    self.step(Event::Failed);
                    self.show_message(message, Duration::from_secs(5));
                }
            }
            UiEvent::Pasted { dictation, result } => {
                if let Some(generation) = dictation {
                    if generation == self.generation && self.dictation.state() == State::Pasting {
                        self.kill_timer(TIMER_PASTE);
                        self.log_dictation(result);
                        if result == PasteResult::NotPasted && self.dictation.next() != Next::None {
                            log::warn!("not pasted; the next Dictation shows, so no message");
                        }
                        self.step(Event::PasteDone);
                    } else {
                        log::info!("late paste result {result:?}: the paste guard ended it before");
                    }
                }
                if result == PasteResult::NotPasted && self.dictation.state() == State::Idle {
                    self.show_message(NOT_PASTED_MESSAGE.into(), Duration::from_secs(3));
                }
            }
        }
    }

    fn step(&mut self, event: Event) {
        let action = self.dictation.handle(event);
        self.act(action);
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Start => {
                self.generation += 1;
                let audio = Arc::new(AudioBuffer::default());
                self.capture.start(audio.clone());
                self.engine.start(self.generation, audio.clone());
                self.audio = Some(audio);
                self.view = DraftView::default();
                self.final_text = None;
                self.stats = None;
                self.set_escape(true);
                self.kill_timer(TIMER_HIDE_MESSAGE);
                self.pad.show(if self.models_unloaded {
                    Status::Loading
                } else {
                    Status::Listening
                });
                unsafe { SetTimer(Some(self.hwnd), TIMER_LEVEL, LEVEL_INTERVAL_MS, None) };
                self.set_icon(IconState::Recording);
            }
            Action::Cancel => {
                self.capture.stop();
                self.engine.cancel(self.generation);
                self.end_dictation();
                self.pad.hide();
                log::info!("dictation cancelled");
            }
            Action::Finish => {
                self.capture.stop();
                self.engine.stop(self.generation);
                self.released_at = Instant::now();
                self.pad.set_status(Status::Finishing);
                if self.models_unloaded {
                    // The deadline must not paste an empty Draft while the models load.
                    log::info!("release deadline waits for the models");
                    self.deadline_waits_for_models = true;
                } else {
                    self.start_deadline();
                }
                self.set_icon(IconState::Busy);
            }
            Action::Paste => {
                self.engine.cancel(self.generation);
                let selection = self
                    .stats
                    .as_ref()
                    .map_or("deadline-draft", |s| s.final_selection.as_str());
                let text = insertion_text(self.final_text.take(), &self.view);
                log::info!(
                    "dictation selection: {selection}, {} ms after release, preview cancellation {:?} ms, final decode {:?} ms",
                    self.released_at.elapsed().as_millis(),
                    self.stats.as_ref().and_then(|s| s.preview_cancel_ms),
                    self.stats.as_ref().and_then(|s| s.final_recast_ms)
                );
                if self.dictation.next() == Next::None {
                    self.end_dictation();
                    self.pad.hide();
                } else {
                    // The next Dictation records: keep its microphone, Scratch Pad and Escape key.
                    self.end_finishing();
                }
                if text.trim().is_empty() {
                    match &self.stats {
                        Some(s) => log::info!(
                            "dictation: no text to paste; audio {:.1} s, speech starts at {}",
                            s.audio_ms as f64 / 1000.0,
                            s.speech_onset_ms
                                .map_or("-".to_owned(), |ms| format!("{ms} ms"))
                        ),
                        None => log::info!("dictation: no text to paste"),
                    }
                    self.step(Event::PasteDone);
                    return;
                }
                self.last_text = Some(text.clone());
                let text = self.with_follow_up_space(text);
                let generation = self.generation;
                paste::start(text, move |result| {
                    post(UiEvent::Pasted {
                        dictation: Some(generation),
                        result,
                    })
                });
                unsafe { SetTimer(Some(self.hwnd), TIMER_PASTE, PASTE_GUARD_MS, None) };
            }
            Action::StartNext => {
                // The microphone opens now, so that no word is lost. The engine gets the audio
                // when the last Dictation is pasted, and then decodes it faster than real time.
                log::info!("next Dictation starts while the last one finishes");
                let audio = Arc::new(AudioBuffer::default());
                self.capture.start(audio.clone());
                self.audio = Some(audio);
                self.next_released_at = None;
                self.set_escape(true);
                self.kill_timer(TIMER_HIDE_MESSAGE);
                self.pad.set_draft(DraftView::default());
                self.pad.show(Status::Listening);
                unsafe { SetTimer(Some(self.hwnd), TIMER_LEVEL, LEVEL_INTERVAL_MS, None) };
                self.set_icon(IconState::Recording);
            }
            Action::CancelNext => {
                self.capture.stop();
                self.next_released_at = None;
                log::info!("next Dictation cancelled");
                if self.dictation.state() == State::Finishing {
                    self.audio = None;
                    self.kill_timer(TIMER_LEVEL);
                    self.pad.set_draft(self.view.clone());
                    self.pad.show(Status::Finishing);
                    self.set_icon(IconState::Busy);
                } else {
                    // The last Dictation already pastes: end everything, as a paste without a next
                    // Dictation does.
                    self.end_dictation();
                    self.pad.hide();
                }
            }
            Action::FinishNext => {
                self.capture.stop();
                self.next_released_at = Some(Instant::now());
                self.pad.set_status(Status::Finishing);
                self.set_icon(IconState::Busy);
            }
            Action::Resume { cancel, recorded } => {
                if cancel {
                    self.engine.cancel(self.generation);
                    self.end_finishing();
                }
                self.generation += 1;
                let audio = self.audio.clone().unwrap_or_default();
                self.engine.start(self.generation, audio.clone());
                self.view = DraftView::default();
                self.final_text = None;
                self.stats = None;
                if recorded {
                    self.engine.stop(self.generation);
                    self.released_at = self.next_released_at.take().unwrap_or_else(Instant::now);
                    if self.models_unloaded {
                        self.deadline_waits_for_models = true;
                    } else {
                        // The engine must first decode all of the recorded audio.
                        let recorded_ms = (audio.len() * 1000 / SAMPLE_RATE) as u64;
                        self.start_deadline_after(Duration::from_millis(recorded_ms));
                    }
                } else {
                    self.set_icon(IconState::Recording);
                }
            }
        }
    }

    /// Adds a space before the text if it follows the last paste into the same window.
    fn with_follow_up_space(&mut self, text: String) -> String {
        let target = unsafe { GetForegroundWindow() }.0 as isize;
        let audio =
            Duration::from_millis(self.stats.as_ref().map_or(0, |s| s.audio_ms.max(0) as u64));
        let follow_up = self
            .last_paste
            .is_some_and(|(hwnd, at)| hwnd == target && at.elapsed() < FOLLOW_UP_WINDOW + audio);
        self.last_paste = Some((target, Instant::now()));
        if follow_up && needs_space_before(&text) {
            format!(" {text}")
        } else {
            text
        }
    }

    /// Ends the parts of the Dictation that finishes, but not the next Dictation.
    fn end_finishing(&mut self) {
        self.kill_timer(TIMER_DEADLINE);
        self.deadline = None;
        self.deadline_waits_for_models = false;
        self.last_activity = Instant::now();
    }

    /// Starts the release deadline: after it, the current Draft pastes without the last Recast.
    fn start_deadline(&mut self) {
        self.start_deadline_after(Duration::ZERO);
    }

    /// Starts the release deadline, longer by half of `audio` that the engine has not decoded yet.
    fn start_deadline_after(&mut self, audio: Duration) {
        self.deadline_waits_for_models = false;
        let ms = self
            .settings
            .release_deadline_ms
            .saturating_add((audio.as_millis() / 2).min(u32::MAX as u128) as u32);
        self.deadline = Some(Instant::now() + Duration::from_millis(ms.into()));
        unsafe { SetTimer(Some(self.hwnd), TIMER_DEADLINE, ms, None) };
    }

    fn end_dictation(&mut self) {
        self.set_escape(false);
        self.kill_timer(TIMER_LEVEL);
        self.kill_timer(TIMER_DEADLINE);
        self.deadline = None;
        self.deadline_waits_for_models = false;
        self.audio = None;
        self.set_icon(IconState::Idle);
        self.last_activity = Instant::now();
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

    fn on_timer(&mut self, id: usize) {
        match id {
            TIMER_LEVEL => {
                if let Some(audio) = &self.audio {
                    self.pad.set_level(audio.level());
                }
            }
            TIMER_DEADLINE => {
                self.expire_deadline();
            }
            TIMER_PASTE => {
                self.kill_timer(TIMER_PASTE);
                if self.dictation.state() == State::Pasting {
                    log::error!(
                        "paste did not end in {PASTE_GUARD_MS} ms: the Dictation ends without a paste result"
                    );
                    self.step(Event::PasteDone);
                }
            }
            TIMER_IDLE => {
                let limit = Duration::from_secs(u64::from(self.settings.unload_after_min) * 60);
                if self.settings.unload_after_min > 0
                    && !self.models_unloaded
                    && self.dictation.state() == State::Idle
                    && self.last_activity.elapsed() >= limit
                {
                    self.engine.unload();
                    self.models_unloaded = true;
                }
            }
            TIMER_HIDE_MESSAGE => {
                self.kill_timer(TIMER_HIDE_MESSAGE);
                if self.dictation.state() == State::Idle {
                    self.pad.hide();
                }
            }
            _ => {}
        }
    }

    fn show_message(&mut self, text: String, duration: Duration) {
        self.pad.show(Status::Message(text));
        unsafe {
            SetTimer(
                Some(self.hwnd),
                TIMER_HIDE_MESSAGE,
                duration.as_millis() as u32,
                None,
            )
        };
    }

    fn paste_last(&mut self) {
        // The Talk Key can start a Dictation while the menu shows.
        if self.dictation.state() != State::Idle {
            log::info!("Paste Last Dictation not done: a Dictation runs");
            return;
        }
        let Some(text) = self.last_text.clone() else {
            return;
        };
        // Give the target window a moment to take the focus back from the menu.
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            paste::start(text, |result| {
                post(UiEvent::Pasted {
                    dictation: None,
                    result,
                })
            });
        });
    }

    fn log_dictation(&self, result: PasteResult) {
        let after_release = self.released_at.elapsed().as_millis();
        match &self.stats {
            Some(s) => {
                let optional = |v: Option<u128>| v.map_or("-".to_owned(), |ms| format!("{ms} ms"));
                let mean = if s.recasts > 0 {
                    format!("{} ms", s.recast_total_ms / u128::from(s.recasts))
                } else {
                    "-".into()
                };
                let at = |v: Option<i64>| v.map_or("-".to_owned(), |ms| format!("{ms} ms"));
                log::info!(
                    "dictation: audio {:.1} s, speech starts at {}, first word shown at {} of audio, {} words; load {}, first text {}, recasts {} (mean {}), last recast {}, paste {:?} {} ms after release",
                    s.audio_ms as f64 / 1000.0,
                    at(s.speech_onset_ms),
                    at(s.first_text_audio_ms),
                    s.final_words,
                    optional(s.load_ms),
                    optional(s.first_text_ms),
                    s.recasts,
                    mean,
                    optional(s.final_recast_ms),
                    result,
                    after_release
                );
            }
            None => log::info!(
                "dictation: paste {result:?} {after_release} ms after release (no final Recast)"
            ),
        }
    }

    fn set_escape(&mut self, on: bool) {
        if let Some(hotkeys) = self.hotkeys.as_mut() {
            hotkeys.set_escape(on);
        }
    }

    fn set_icon(&self, state: IconState) {
        let tip = match state {
            IconState::Idle => "LiveTyper",
            IconState::Recording => "LiveTyper: recording",
            IconState::Busy => "LiveTyper: finishing",
        };
        tray::set_icon(self.hwnd, self.icons.get(state), tip);
    }

    fn kill_timer(&self, id: usize) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), id);
        }
    }
}

/// True if a text that follows other text needs a space before it.
fn needs_space_before(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|c| !c.is_whitespace() && !".,;:!?)]}'\"".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_follow_up_gets_a_space_unless_it_starts_with_punctuation() {
        assert!(needs_space_before("This is attempt seven."));
        assert!(!needs_space_before(", and then"));
        assert!(!needs_space_before(" already"));
        assert!(!needs_space_before(""));
    }
}
