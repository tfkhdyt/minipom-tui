//! Application state, key handling and the pomodoro/time-tracking wiring.

use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::model::{now_epoch, Data, Entry, TimerState};
use crate::timer::{Phase, Timer};

pub struct App {
    pub data: Data,
    pub timer: Timer,
    /// Selected task id. Kept by id, not index, so re-sorting never moves it.
    pub selected: Option<u64>,
    /// Task the running pomodoro is credited to.
    pub active: Option<u64>,
    pub input: Option<Input>,
    pub confirm_delete: Option<u64>,
    pub status: String,
    pub today: NaiveDate,
    pub should_quit: bool,
    /// An hour of focus is written to the log only when it closes.
    open: Option<OpenEntry>,
    /// When the status message gives way back to the key list.
    status_expiry: Option<Instant>,
}

struct OpenEntry {
    task_id: u64,
    start: i64,
}

#[derive(Clone, Copy)]
pub enum Prompt {
    NewTask,
    Title(u64),
    Project(u64),
    Tags(u64),
}

pub struct Input {
    pub prompt: String,
    pub target: Prompt,
    pub buf: String,
    /// Existing values for this field, drawn from the stored tasks.
    pub suggestions: Vec<String>,
}

impl Input {
    /// The piece being typed: the last comma-separated part for tags.
    fn token(&self) -> &str {
        match self.target {
            Prompt::Tags(_) => self.buf.rsplit(',').next().unwrap_or("").trim(),
            _ => self.buf.trim(),
        }
    }

    /// Existing values that extend what is typed, minus tags already listed.
    pub fn matches(&self) -> Vec<&str> {
        let token = self.token().to_lowercase();
        let listed: Vec<&str> = if matches!(self.target, Prompt::Tags(_)) {
            self.buf.split(',').map(str::trim).collect()
        } else {
            Vec::new()
        };
        self.suggestions
            .iter()
            .filter(|s| {
                let lower = s.to_lowercase();
                lower.starts_with(&token) && lower != token && !listed.contains(&s.as_str())
            })
            .map(String::as_str)
            .collect()
    }

    /// Replaces the token being typed with `value`, appending for a new tag.
    fn apply(&mut self, value: &str) {
        if matches!(self.target, Prompt::Tags(_)) {
            let mut parts: Vec<String> = self
                .buf
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if !self.token().is_empty() {
                parts.pop();
            }
            parts.push(value.to_string());
            self.buf = parts.join(", ");
        } else {
            self.buf = value.to_string();
        }
    }
}

pub const HELP: &str =
    "a add  e edit  p project  t tags  d del  space done  s start/pause  x reset  n skip  q quit";

/// How long a status message stays before the key list comes back.
const STATUS_LINGER: Duration = Duration::from_secs(4);

/// The clock's remaining time after `gap` seconds closed. A running clock that
/// ran out while the app was away comes back at zero, so the next tick finishes
/// the phase instead of dropping the save.
fn resume_remaining(state: TimerState, gap: u64, total: Duration) -> Duration {
    let base = Duration::from_secs(state.remaining_secs);
    let left = if state.running {
        base.saturating_sub(Duration::from_secs(gap))
    } else {
        base
    };
    if left.is_zero() && !state.running {
        total
    } else {
        left.min(total)
    }
}

impl App {
    pub fn new() -> Self {
        let (data, warning) = Data::load();
        let today = Local::now().date_naive();
        let mut timer = Timer::new(Default::default());
        // The day's pomodoro count survives a restart.
        timer.pomodoros = data.pomodoros_on(today);
        // Pick the clock back up where the last save left it.
        if let Some(state) = data.timer {
            timer.phase = state.phase;
            timer.work_done = state.work_done;
            timer.running = state.running;
            let gap = (now_epoch() - state.saved_at).max(0) as u64;
            timer.remaining = resume_remaining(state, gap, timer.total());
        }
        let mut app = Self {
            data,
            timer,
            selected: None,
            active: None,
            input: None,
            confirm_delete: None,
            status: warning.unwrap_or_else(|| HELP.to_string()),
            today,
            should_quit: false,
            open: None,
            status_expiry: None,
        };
        // Select the first row of the list as the user sees it, not insertion order.
        app.selected = app.data.ordered().first().copied();
        app
    }

    // ---- helpers -------------------------------------------------------

    pub fn selected_index(&self) -> Option<usize> {
        self.selected
            .and_then(|id| self.data.ordered().iter().position(|&t| t == id))
    }

    pub fn selected_task_id(&self) -> Option<u64> {
        let ordered = self.data.ordered();
        self.selected_index().and_then(|i| ordered.get(i).copied())
    }

    fn move_selection(&mut self, delta: isize) {
        let ordered = self.data.ordered();
        if ordered.is_empty() {
            self.selected = None;
            return;
        }
        let current = self.selected_index().unwrap_or(0);
        let next = (current as isize + delta).clamp(0, ordered.len() as isize - 1) as usize;
        self.selected = Some(ordered[next]);
    }

    fn persist(&mut self) {
        self.data.timer = Some(TimerState {
            phase: self.timer.phase,
            remaining_secs: self.timer.remaining.as_secs(),
            work_done: self.timer.work_done,
            running: self.timer.running,
            saved_at: now_epoch(),
        });
        if let Err(e) = self.data.save() {
            self.status = format!("save failed: {e}");
        }
    }
    fn set_status(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_expiry = Some(Instant::now() + STATUS_LINGER);
    }

    // ---- time tracking -------------------------------------------------

    fn open_entry(&mut self) {
        if self.open.is_some() || !self.timer.phase.is_work() {
            return;
        }
        if let Some(id) = self.active {
            self.open = Some(OpenEntry {
                task_id: id,
                start: now_epoch(),
            });
        }
    }

    fn close_entry(&mut self) {
        if let Some(entry) = self.open.take() {
            let end = now_epoch();
            if end > entry.start {
                self.data.entries.push(Entry {
                    task_id: entry.task_id,
                    start: entry.start,
                    end,
                });
                self.persist();
            }
        }
    }

    // ---- timer controls ------------------------------------------------

    /// Stops the clock and banks whatever the open entry logged.
    fn pause_timer(&mut self) {
        if !self.timer.running {
            return;
        }
        self.timer.pause();
        let logged = self
            .live()
            .map(|(_, secs)| crate::ui::fmt_duration(secs as i64));
        self.close_entry();
        match logged {
            Some(logged) => self.set_status(format!("paused, {logged} logged")),
            None => self.set_status("paused"),
        }
    }

    fn toggle_timer(&mut self) {
        if self.timer.running {
            self.pause_timer();
            return;
        }
        // Starting always binds to the highlighted task, so moving the cursor
        // before pressing s is how you switch tasks mid-session.
        self.active = self.selected_task_id().or(self.active);
        match self.active {
            Some(id) => {
                // Starting a task that was finished reopens it.
                if let Some(t) = self.data.task_mut(id) {
                    t.done = false;
                }
                let title = self
                    .data
                    .task(id)
                    .map(|t| t.title.clone())
                    .unwrap_or_default();
                self.timer.start(Instant::now());
                self.open_entry();
                self.persist();
                if self.timer.phase.is_work() {
                    self.set_status(format!("focusing on {title}"));
                } else {
                    self.set_status(format!("{} break", self.timer.phase.label().to_lowercase()));
                }
            }
            None if !self.timer.phase.is_work() => {
                self.timer.start(Instant::now());
                self.set_status("break started");
            }
            None => self.set_status("add a task first, then start the timer"),
        }
    }

    fn reset_timer(&mut self) {
        self.close_entry();
        self.timer.reset();
        self.active = None;
        self.set_status("timer reset");
    }

    fn skip_phase(&mut self) {
        self.close_entry();
        self.timer.skip();
        self.active = None;
        self.set_status(format!("skipped, now {}", self.timer.phase.label()));
    }

    // ---- inputs --------------------------------------------------------

    fn open_input(&mut self, prompt: &str, target: Prompt, prefill: String) {
        let suggestions = match target {
            Prompt::Project(_) => self.data.projects(),
            Prompt::Tags(_) => self.data.tags(),
            _ => Vec::new(),
        };
        self.input = Some(Input {
            prompt: prompt.into(),
            target,
            buf: prefill,
            suggestions,
        });
    }

    fn submit_input(&mut self) {
        let Some(input) = self.input.take() else {
            return;
        };
        let text = input.buf.trim().to_string();
        match input.target {
            Prompt::NewTask => {
                if text.is_empty() {
                    self.set_status("empty task ignored");
                    return;
                }
                let id = self.data.add_task(text.clone());
                self.selected = Some(id);
                self.set_status(format!("added {text}"));
            }
            Prompt::Title(id) => {
                if text.is_empty() {
                    self.set_status("title cannot be empty");
                    return;
                }
                if let Some(t) = self.data.task_mut(id) {
                    t.title = text;
                }
            }
            Prompt::Project(id) => {
                if let Some(t) = self.data.task_mut(id) {
                    // An empty answer detaches the task from its project.
                    t.project = Some(text).filter(|s| !s.is_empty());
                }
            }
            Prompt::Tags(id) => {
                let mut tags: Vec<String> = Vec::new();
                for tag in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    if !tags.iter().any(|t| t == tag) {
                        tags.push(tag.to_string());
                    }
                }
                if let Some(t) = self.data.task_mut(id) {
                    t.tags = tags;
                }
            }
        }
        self.persist();
    }

    fn delete_selected(&mut self) {
        let Some(id) = self.confirm_delete.take() else {
            return;
        };
        let index = self.selected_index();
        self.data.remove_task(id);
        if self.active == Some(id) {
            self.close_entry();
            self.active = None;
        }
        if self.selected == Some(id) {
            let ordered = self.data.ordered();
            // The row that took its place, or the last one when the tail went.
            self.selected = index.and_then(|i| ordered.get(i).or(ordered.last()).copied());
        }
        self.persist();
        self.set_status("task deleted");
    }

    // ---- events --------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        if self.input.is_some() {
            self.on_key_input(key);
        } else if self.confirm_delete.is_some() {
            self.on_key_confirm(key);
        } else {
            self.on_key_normal(key);
        }
    }

    fn on_key_confirm(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => self.delete_selected(),
            _ => {
                self.confirm_delete = None;
                self.set_status("delete cancelled");
            }
        }
    }

    fn on_key_input(&mut self, key: KeyEvent) {
        let Some(input) = self.input.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.input = None,
            KeyCode::Enter => self.submit_input(),
            KeyCode::Tab => {
                if let Some(value) = input.matches().first().map(|s| s.to_string()) {
                    input.apply(&value);
                }
            }
            KeyCode::Backspace => {
                input.buf.pop();
            }
            KeyCode::Char(c) => input.buf.push(c),
            _ => {}
        }
    }

    fn on_key_normal(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => self.should_quit = true,
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('a') => self.open_input("new task", Prompt::NewTask, String::new()),
            KeyCode::Char('e') => self.edit_selected(Prompt::Title, "title", |t| t.title.clone()),
            KeyCode::Char('p') => self.edit_selected(Prompt::Project, "project", |t| {
                t.project.clone().unwrap_or_default()
            }),
            KeyCode::Char('t') => self.edit_selected(Prompt::Tags, "tags (comma separated)", |t| {
                t.tags.join(", ")
            }),
            KeyCode::Char('d') => {
                if let Some(id) = self.selected_task_id() {
                    self.confirm_delete = Some(id);
                }
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                if let Some(id) = self.selected_task_id() {
                    let done = self.data.task_mut(id).map(|t| {
                        t.done = !t.done;
                        t.done
                    });
                    // Finishing the task the clock is credited to ends the session.
                    if done == Some(true) && self.active == Some(id) {
                        self.pause_timer();
                    }
                    self.persist();
                }
            }
            KeyCode::Char('s') => self.toggle_timer(),
            KeyCode::Char('x') => self.reset_timer(),
            KeyCode::Char('n') => self.skip_phase(),
            KeyCode::Char('?') => self.set_status(HELP),
            _ => {}
        }
    }

    /// Opens the one-line editor for the selected task, prefilled from `get`.
    fn edit_selected(
        &mut self,
        target: fn(u64) -> Prompt,
        prompt: &str,
        get: fn(&crate::model::Task) -> String,
    ) {
        let Some(id) = self.selected_task_id() else {
            return;
        };
        let Some(task) = self.data.task(id) else {
            return;
        };
        let prefill = get(task);
        self.open_input(prompt, target(id), prefill);
    }

    // ---- periodic ------------------------------------------------------

    /// Advances the clock. Returns the phase that just ran out, if any, so the
    /// caller can decide how to get attention.
    pub fn tick(&mut self) -> Option<Phase> {
        self.today = Local::now().date_naive();
        if self.status_expiry.is_some_and(|at| Instant::now() >= at) {
            self.status = HELP.to_string();
            self.status_expiry = None;
        }
        if !self.timer.tick(Instant::now()) {
            return None;
        }
        let finished = self.timer.phase;
        if finished.is_work() {
            self.close_entry();
            self.active = None;
        }
        self.timer.complete();
        if finished.is_work() {
            self.timer.pomodoros = self.data.add_pomodoro(self.today);
        }
        self.set_status(format!(
            "{} done, {} next, press s to start",
            finished.label(),
            self.timer.phase.label()
        ));
        self.persist();
        Some(finished)
    }

    pub fn quit(&mut self) {
        self.close_entry();
        self.persist();
    }

    pub fn active_title(&self) -> Option<String> {
        self.active
            .and_then(|id| self.data.task(id))
            .map(|t| t.title.clone())
    }

    /// The focus stretch in progress, as (task id, seconds so far). It is not
    /// in the log yet, so callers add it in to what they display.
    pub fn live(&self) -> Option<(u64, u64)> {
        let open = self.open.as_ref()?;
        Some((open.task_id, (now_epoch() - open.start).max(0) as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyModifiers;

    fn app_with_task() -> (App, u64) {
        let mut data = Data::default();
        let id = data.add_task("write docs".into());
        let app = App {
            data,
            timer: Timer::new(Default::default()),
            selected: Some(id),
            active: Some(id),
            input: None,
            confirm_delete: None,
            status: String::new(),
            today: Local::now().date_naive(),
            should_quit: false,
            open: None,
            status_expiry: None,
        };
        (app, id)
    }

    #[test]
    fn resume_counts_time_while_the_app_was_closed() {
        let state = |remaining_secs, running| TimerState {
            phase: Phase::Work,
            remaining_secs,
            work_done: 0,
            running,
            saved_at: 0,
        };
        let total = Duration::from_secs(1500);

        // Paused: the gap is not counted.
        assert_eq!(
            resume_remaining(state(600, false), 300, total),
            Duration::from_secs(600)
        );
        // Running: closed time comes off the clock.
        assert_eq!(
            resume_remaining(state(600, true), 300, total),
            Duration::from_secs(300)
        );
        // Ran out while away: zero, so the next tick completes it.
        assert_eq!(
            resume_remaining(state(60, true), 300, total),
            Duration::ZERO
        );
        // A paused clock at zero is repaired to a full phase.
        assert_eq!(
            resume_remaining(state(0, false), 0, total),
            Duration::from_secs(1500)
        );
    }

    #[test]
    fn starting_a_done_task_clears_done() {
        std::env::set_var("XDG_DATA_HOME", std::env::temp_dir().join("minipom-2-test"));

        let (mut app, id) = app_with_task();
        app.data.task_mut(id).unwrap().done = true;

        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert!(
            !app.data.task(id).unwrap().done,
            "starting reopens the task"
        );
        assert!(app.timer.running);
    }

    #[test]
    fn deleting_a_task_selects_the_nearest_row() {
        let mut data = Data::default();
        let a = data.add_task("a".into());
        let b = data.add_task("b".into());
        let c = data.add_task("c".into());
        let mut app = App {
            data,
            timer: Timer::new(Default::default()),
            selected: Some(b),
            active: None,
            input: None,
            confirm_delete: Some(b),
            status: String::new(),
            today: Local::now().date_naive(),
            should_quit: false,
            open: None,
            status_expiry: None,
        };

        app.delete_selected();
        assert_eq!(app.selected, Some(c), "the row below takes its place");

        app.selected = Some(c);
        app.confirm_delete = Some(c);
        app.delete_selected();
        assert_eq!(app.selected, Some(a), "the row above is used at the tail");

        app.selected = Some(a);
        app.confirm_delete = Some(a);
        app.delete_selected();
        assert_eq!(app.selected, None, "an empty list has nothing to select");
    }

    #[test]
    fn suggestions_complete_projects_and_append_tags() {
        let mut app = App {
            data: Data::default(),
            timer: Timer::new(Default::default()),
            selected: None,
            active: None,
            input: None,
            confirm_delete: None,
            status: String::new(),
            today: Local::now().date_naive(),
            should_quit: false,
            open: None,
            status_expiry: None,
        };
        let a = app.data.add_task("a".into());
        let b = app.data.add_task("b".into());
        app.data.task_mut(a).unwrap().project = Some("work".into());
        app.data.task_mut(a).unwrap().tags = vec!["deep".into(), "urgent".into()];
        app.data.task_mut(b).unwrap().project = Some("home".into());
        app.selected = Some(b);

        // Projects: typing narrows, Tab fills the whole value.
        app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        app.input.as_mut().unwrap().buf.clear();
        app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().matches(), vec!["work"]);
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().buf, "work");

        // Tags: Tab appends to what is already listed, and drops used tags.
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        let input = app.input.as_mut().unwrap();
        input.buf = "urgent, de".into();
        assert_eq!(input.matches(), vec!["deep"]);
        let value = input.matches()[0].to_string();
        input.apply(&value);
        assert_eq!(input.buf, "urgent, deep");
        input.buf.push_str(", z");
        assert!(
            input.matches().is_empty(),
            "used tags are not offered again"
        );
    }

    #[test]
    fn done_pauses_only_the_clocked_task() {
        // One test, so the redirected store cannot race with a sibling.
        std::env::set_var("XDG_DATA_HOME", std::env::temp_dir().join("minipom-2-test"));

        let (mut app, id) = app_with_task();
        let other = app.data.add_task("other".into());
        app.active = Some(other);
        app.timer.start(Instant::now());
        app.on_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(app.timer.running, "another task's clock must keep running");
        assert!(app.data.task(id).unwrap().done);

        app.active = Some(id);
        app.timer.start(Instant::now());
        app.selected = Some(id);
        app.data.task_mut(id).unwrap().done = false;
        app.on_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(!app.timer.running, "finishing the clocked task pauses it");
    }
}
