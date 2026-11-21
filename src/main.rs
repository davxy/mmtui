mod config;
mod drives;
mod mountpoints;
mod tui;

use crate::config::Config;
use crossterm::{
    event::{Event, EventStream, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{prelude::CrosstermBackend, Terminal};
use std::{io::stderr, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tui::{InputResult, Tui};

#[tokio::main]
async fn main() -> udisks2::Result<()> {
    let config = Config::load_or_default();
    enable_raw_mode().unwrap();
    execute!(stderr(), EnterAlternateScreen).unwrap();
    let mut terminal = Terminal::new(CrosstermBackend::new(stderr())).unwrap();

    let period = Duration::from_secs_f32(1.0 / 10.0);
    let mut interval = tokio::time::interval(period);
    let mut events = EventStream::new();

    let state: Arc<Mutex<Result<Vec<drives::Drive>, String>>> =
        Arc::new(Mutex::new(Ok(Vec::new())));
    let s = state.clone();
    tokio::spawn(async move {
        loop {
            let scan = drives::collect_all(&config).await;
            *s.lock().await = scan.map_err(|error| format!("udisks: {error}"));
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });

    let mut output_change_path = String::new();
    let mut tui = Tui::default();
    loop {
        tui.update(&*state.lock().await);
        tui.finish_operation().await;
        terminal
            .draw(|f| tui.draw(f))
            .expect("failed to draw frame");

        let key = tokio::select! {
            _ = interval.tick() => continue,
            Some(Ok(Event::Key(key))) = events.next() => key,
        };

        if key.kind == KeyEventKind::Press {
            match tui.input(key) {
                InputResult::None => {}
                InputResult::Quit => break,
                InputResult::QuitChangeDirectory(p) => {
                    output_change_path = p;
                    break;
                }
            }
        }
    }

    disable_raw_mode().unwrap();
    execute!(stderr(), LeaveAlternateScreen).unwrap();
    println!("{output_change_path}");
    Ok(())
}
