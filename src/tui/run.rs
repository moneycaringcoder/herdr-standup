//! Crossterm runtime and terminal lifecycle for the digest pane.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::config::{Config, Format, DEFAULT_SINCE};
use crate::Result;

use super::state::{adopt, apply, fail, DigestPane, Intent, Key, WindowKind};
use super::view;

type DigestTerminal = Terminal<CrosstermBackend<io::Stdout>>;

/// Runs the interactive digest, initially showing today's window.
pub fn run_digest(base: &Config) -> Result<()> {
    let digest = crate::standup::build(&window_config(base, WindowKind::Today, None))?;
    let stop = Arc::new(AtomicBool::new(false));
    register_stop_signals(&stop)?;

    let guard = terminal::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    let result = event_loop(base, DigestPane::new(digest), &stop, &mut terminal);
    drop(terminal);
    drop(guard);
    result
}

fn event_loop(
    base: &Config,
    mut pane: DigestPane,
    stop: &AtomicBool,
    terminal: &mut DigestTerminal,
) -> Result<()> {
    let mut mouse = view::MouseMap::default();
    let mut dirty = true;

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if dirty {
            terminal.draw(|frame| mouse = view::render(frame, &pane))?;
            dirty = false;
        }

        let Some(event) = next_event()? else {
            continue;
        };
        match event {
            Event::Key(event) => {
                if let Some(key) = map_key_event(event) {
                    pane = apply(pane, key);
                    dirty = true;
                }
            }
            Event::Mouse(event) => match event.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(cursor) = mouse.cursor_at(event.column, event.row) {
                        pane = apply(pane, Key::Focus(cursor));
                        dirty = true;
                    }
                }
                MouseEventKind::ScrollUp => {
                    pane = apply(pane, Key::Up);
                    dirty = true;
                }
                MouseEventKind::ScrollDown => {
                    pane = apply(pane, Key::Down);
                    dirty = true;
                }
                _ => {}
            },
            Event::Resize(_, _) => dirty = true,
            Event::FocusGained | Event::FocusLost | Event::Paste(_) => {}
        }

        match pane.intent {
            Intent::None => {}
            Intent::Quit => return Ok(()),
            Intent::Load(window) => {
                pane = load_window(pane, base, window);
                dirty = true;
            }
            Intent::Refresh => {
                pane = refresh(pane, base);
                dirty = true;
            }
        }
    }
}

fn load_window(pane: DigestPane, base: &Config, window: WindowKind) -> DigestPane {
    match crate::standup::build(&window_config(base, window, None)) {
        Ok(digest) => {
            let generated_at = digest.generated_at.clone();
            let pane = adopt(pane, digest, window);
            if window == WindowKind::SinceLast {
                match crate::window::record_run(&generated_at) {
                    Ok(()) => pane,
                    Err(err) => fail(pane, format!("could not advance the since-last marker: {err}")),
                }
            } else {
                pane
            }
        }
        Err(err) => fail(pane, format!("could not load {}: {err}", window.label())),
    }
}

fn refresh(pane: DigestPane, base: &Config) -> DigestPane {
    // The marker has already advanced when a since-last window is entered.
    // Refreshing must therefore pin the window's original start rather than
    // resolving the newly advanced marker into an almost-empty digest.
    let active = pane.active;
    let fixed_start = (active == WindowKind::SinceLast).then_some(pane.digest.window.since.epoch);
    let source = pane.digest.window.source.clone();
    match crate::standup::build(&window_config(base, active, fixed_start)) {
        Ok(mut digest) => {
            if active == WindowKind::SinceLast {
                digest.window.source = source;
            }
            adopt(pane, digest, active)
        }
        Err(err) => fail(
            pane,
            format!("could not refresh {}: {err}", active.label()),
        ),
    }
}

fn window_config(base: &Config, window: WindowKind, fixed_start: Option<i64>) -> Config {
    let mut config = base.clone();
    config.format = Format::Text;
    config.record_run = false;
    config.until = None;
    config.rollup = None;
    match window {
        WindowKind::Today => {
            config.since = DEFAULT_SINCE.to_string();
            config.since_is_explicit = false;
            config.since_last = false;
        }
        WindowKind::Yesterday => {
            config.since = "yesterday".to_string();
            config.since_is_explicit = true;
            config.since_last = false;
        }
        WindowKind::SinceLast => match fixed_start {
            Some(epoch) => {
                config.since = format!("@{epoch}");
                config.since_is_explicit = true;
                config.since_last = false;
            }
            None => {
                // First use follows --since-last's documented fallback to today,
                // independent of a custom default in the config file.
                config.since = DEFAULT_SINCE.to_string();
                config.since_is_explicit = false;
                config.since_last = true;
            }
        },
    }
    config
}

fn next_event() -> Result<Option<Event>> {
    match event::poll(Duration::from_millis(50)) {
        Ok(false) => Ok(None),
        Ok(true) => match event::read() {
            Ok(event) => Ok(Some(event)),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => Ok(None),
            Err(err) => Err(err.into()),
        },
        Err(err) if err.kind() == io::ErrorKind::Interrupted => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub fn map_key_event(event: KeyEvent) -> Option<Key> {
    if !matches!(event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return None;
    }
    Some(match event.code {
        KeyCode::Up | KeyCode::Char('k') => Key::Up,
        KeyCode::Down | KeyCode::Char('j') => Key::Down,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Escape,
        KeyCode::Char('q') => Key::Quit,
        KeyCode::Char('t') => Key::Today,
        KeyCode::Char('y') => Key::Yesterday,
        KeyCode::Char('l') => Key::SinceLast,
        KeyCode::Char('R') => Key::Refresh,
        KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => Key::Quit,
        _ => Key::Other,
    })
}

#[cfg(unix)]
fn register_stop_signals(stop: &Arc<AtomicBool>) -> Result<()> {
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, Arc::clone(stop))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn register_stop_signals(_stop: &Arc<AtomicBool>) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
mod terminal {
    use std::io::{self, IsTerminal};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Once;

    use crossterm::cursor::{Hide, Show};
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    use crossterm::execute;
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };

    static ACTIVE: AtomicBool = AtomicBool::new(false);
    static HOOKS: Once = Once::new();

    pub struct Guard(());

    impl Drop for Guard {
        fn drop(&mut self) {
            restore();
        }
    }

    pub fn enter() -> crate::Result<Guard> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(
                "the digest pane needs a terminal on stdin and stdout; use --report or --json when there is not one"
                    .into(),
            );
        }
        enable_raw_mode()?;
        ACTIVE.store(true, Ordering::SeqCst);
        install_hooks();
        if let Err(err) = execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture, Hide) {
            restore();
            return Err(err.into());
        }
        Ok(Guard(()))
    }

    pub fn restore() {
        if !ACTIVE.swap(false, Ordering::SeqCst) {
            return;
        }
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            LeaveAlternateScreen,
            Show
        );
    }

    fn install_hooks() {
        HOOKS.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore();
                previous(info);
            }));
            for signal in [
                signal_hook::consts::SIGINT,
                signal_hook::consts::SIGTERM,
                signal_hook::consts::SIGHUP,
            ] {
                let _ = unsafe { signal_hook::low_level::register(signal, restore) };
            }
        });
    }
}

#[cfg(not(unix))]
mod terminal {
    pub struct Guard(());

    pub fn enter() -> crate::Result<Guard> {
        Err("the digest pane is unix-only; use --report or --json".into())
    }
}
