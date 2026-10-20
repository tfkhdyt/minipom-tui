mod app;
mod model;
mod timer;
mod ui;

use std::io::{self, stdout, Stdout, Write};
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
};
use ratatui::Terminal;

use app::App;

fn main() -> io::Result<()> {
    let mut app = App::new();

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;

    let result = run(&mut terminal, &mut app);

    disable_raw_mode()?;
    // Leave the tab named "minipom" no longer than the app itself.
    execute!(terminal.backend_mut(), LeaveAlternateScreen, SetTitle(""))?;
    terminal.show_cursor()?;
    app.quit();

    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> io::Result<()> {
    let tick = Duration::from_millis(200);
    // Only write the title when it changes: once a second at most.
    let mut title = String::new();
    loop {
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(tick)? {
            match event::read()? {
                Event::Key(key) if key.kind != event::KeyEventKind::Release => app.on_key(key),
                _ => {}
            }
        }
        if let Some(finished) = app.tick() {
            alert(
                &mut io::stdout(),
                &format!(
                    "{} finished, {} next",
                    finished.label(),
                    app.timer.phase.label()
                ),
            );
        }
        let wanted = app.title();
        if wanted != title {
            execute!(terminal.backend_mut(), SetTitle(&wanted))?;
            title = wanted;
        }
        if app.should_quit {
            return Ok(());
        }
    }
}

/// Rings the bell and asks the desktop for a notification. Both are best
/// effort: terminals that do not understand OSC 9 ignore it, and tmux swallows
/// it before the outer terminal sees it (the bell still gets through).
fn alert(out: &mut impl Write, message: &str) {
    // Bell, then the OSC 9 notification, which runs until a BEL.
    let _ = write!(out, "\x07\x1b]9;{message}\x07");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alert_rings_the_bell_then_frames_one_osc_9() {
        let mut out = Vec::new();
        alert(&mut out, "WORK finished, SHORT BREAK next");

        assert_eq!(out.first(), Some(&0x07), "the bell comes first");
        assert_eq!(&out[1..5], b"\x1b]9;", "then the OSC 9 introducer");
        assert_eq!(
            out.last(),
            Some(&0x07),
            "and the payload is closed by a BEL"
        );
        // A stray BEL inside the message would cut the notification short.
        assert_eq!(out.iter().filter(|b| **b == 0x07).count(), 2);
    }
}
