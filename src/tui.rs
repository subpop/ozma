use std::io::{self, Stderr};

use color_eyre::eyre::Result;
use crossterm::{
    cursor,
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event as CrosstermEvent, EventStream,
    },
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::{FutureExt, StreamExt};
use ratatui::backend::CrosstermBackend;
use tokio::{
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::event::Event;

/// The TUI manages the terminal state (raw mode, alternate screen) and runs
/// an async event loop that multiplexes terminal events, ticks, and render signals
/// into a unified `Event` channel.
pub struct Tui {
    /// The ratatui terminal, rendering to stderr so stdout stays free for piping.
    pub terminal: ratatui::Terminal<CrosstermBackend<Stderr>>,
    /// Handle to the spawned event loop task.
    task: JoinHandle<()>,
    /// Token to signal the event loop to shut down.
    cancellation_token: CancellationToken,
    /// Receive end of the event channel.
    event_rx: UnboundedReceiver<Event>,
    /// Send end of the event channel (cloneable, can be shared with other tasks).
    pub event_tx: UnboundedSender<Event>,
    /// Ticks per second for background logic.
    tick_rate: f64,
    /// Frames per second for rendering.
    frame_rate: f64,
}

impl Tui {
    /// Create a new Tui with default tick and frame rates.
    pub fn new() -> Result<Self> {
        let backend = CrosstermBackend::new(io::stderr());
        let terminal = ratatui::Terminal::new(backend)?;
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let cancellation_token = CancellationToken::new();
        let task = tokio::spawn(async {});

        Ok(Self {
            terminal,
            task,
            cancellation_token,
            event_rx,
            event_tx,
            tick_rate: 4.0,
            frame_rate: 30.0,
        })
    }

    /// Set the tick rate (ticks per second).
    pub fn tick_rate(mut self, rate: f64) -> Self {
        self.tick_rate = rate;
        self
    }

    /// Set the frame rate (frames per second).
    pub fn frame_rate(mut self, rate: f64) -> Self {
        self.frame_rate = rate;
        self
    }

    /// Enter the TUI: enable raw mode, alternate screen, mouse capture, bracketed paste,
    /// and start the async event loop.
    pub fn enter(&mut self) -> Result<()> {
        terminal::enable_raw_mode()?;
        crossterm::execute!(
            io::stderr(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste,
            cursor::Hide,
        )?;
        self.terminal.clear()?;
        self.start_event_loop();
        Ok(())
    }

    /// Exit the TUI: cancel the event loop, restore terminal state.
    pub fn exit(&mut self) -> Result<()> {
        self.cancellation_token.cancel();
        restore_terminal()?;
        Ok(())
    }

    /// Receive the next event from the event loop.
    pub async fn next(&mut self) -> Option<Event> {
        self.event_rx.recv().await
    }

    /// Spawn the async event loop task that multiplexes crossterm events, ticks, and render signals.
    fn start_event_loop(&mut self) {
        let tick_delay = std::time::Duration::from_secs_f64(1.0 / self.tick_rate);
        let render_delay = std::time::Duration::from_secs_f64(1.0 / self.frame_rate);

        let cancellation_token = self.cancellation_token.clone();
        let event_tx = self.event_tx.clone();

        self.task = tokio::spawn(async move {
            let mut reader = EventStream::new();
            let mut tick_interval = tokio::time::interval(tick_delay);
            let mut render_interval = tokio::time::interval(render_delay);

            // Signal that the application has initialized
            let _ = event_tx.send(Event::Init);

            loop {
                let tick = tick_interval.tick();
                let render = render_interval.tick();
                let crossterm_event = reader.next().fuse();

                tokio::select! {
                    _ = cancellation_token.cancelled() => {
                        break;
                    }
                    _ = tick => {
                        let _ = event_tx.send(Event::Tick);
                    }
                    _ = render => {
                        let _ = event_tx.send(Event::Render);
                    }
                    maybe_event = crossterm_event => {
                        match maybe_event {
                            Some(Ok(evt)) => {
                                let event = match evt {
                                    CrosstermEvent::Key(key) => Event::Key(key),
                                    CrosstermEvent::Mouse(mouse) => Event::Mouse(mouse),
                                    CrosstermEvent::Resize(w, h) => Event::Resize(w, h),
                                    CrosstermEvent::FocusGained => Event::FocusGained,
                                    CrosstermEvent::FocusLost => Event::FocusLost,
                                    CrosstermEvent::Paste(text) => Event::Paste(text),
                                };
                                let _ = event_tx.send(event);
                            }
                            Some(Err(_)) => {
                                let _ = event_tx.send(Event::Error);
                            }
                            None => {}
                        }
                    }
                }
            }
        });
    }
}

/// Restore the terminal to its original state. Called on exit, panic, and error.
/// This is a free function so it can be called from the panic hook in `errors.rs`.
pub fn restore_terminal() -> Result<()> {
    terminal::disable_raw_mode()?;
    crossterm::execute!(
        io::stderr(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
        cursor::Show,
    )?;
    Ok(())
}
