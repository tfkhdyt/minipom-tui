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
    pub confirm_delete: Option<Confirm>,
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

/// What a pending `d`/`D` is waiting for a yes on.
#[derive(Clone, Copy)]
pub enum Confirm {
    One(u64),
    Completed,
}

/// One field of the add/edit form. The form shows all three at once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Title,
    Project,
    Tags,
}

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Field::Title => "title",
            Field::Project => "project",
            Field::Tags => "tags",
        }
    }

    /// Next field for a tab, wrapping at both ends.
    fn shift(self, delta: isize) -> Self {
        const FIELDS: [Field; 3] = [Field::Title, Field::Project, Field::Tags];
        let i = FIELDS.iter().position(|f| *f == self).unwrap_or(0) as isize;
        FIELDS[(i + delta).rem_euclid(FIELDS.len() as isize) as usize]
    }
}

/// The text of the whole form. Nothing is written to the store until the form
/// is saved, so escaping out leaves the task as it was.
#[derive(Default)]
pub struct Draft {
    /// `None` adds a task; `Some` edits that one.
    id: Option<u64>,
    title: String,
    project: String,
    tags: String,
}

pub struct Input {
    pub field: Field,
    draft: Draft,
    /// Existing values, drawn from the stored tasks, per suggestion field.
    projects: Vec<String>,
    tags: Vec<String>,
    /// Which match `→` will take.
    pub choice: usize,
}

impl Input {
    pub fn value(&self, field: Field) -> &str {
        match field {
            Field::Title => &self.draft.title,
            Field::Project => &self.draft.project,
            Field::Tags => &self.draft.tags,
        }
    }

    fn value_mut(&mut self) -> &mut String {
        match self.field {
            Field::Title => &mut self.draft.title,
            Field::Project => &mut self.draft.project,
            Field::Tags => &mut self.draft.tags,
        }
    }

    pub fn heading(&self) -> &'static str {
        if self.draft.id.is_some() {
            "Edit task"
        } else {
            "Add task"
        }
    }

    /// The piece being typed: the last comma-separated part for tags.
    fn token(&self) -> &str {
        let value = self.value(self.field).trim();
        match self.field {
            Field::Tags => value.rsplit(',').next().unwrap_or("").trim(),
            _ => value,
        }
    }

    /// Existing values that extend what is typed, minus tags already listed.
    pub fn matches(&self) -> Vec<&str> {
        let suggestions = match self.field {
            Field::Title => return Vec::new(),
            Field::Project => &self.projects,
            Field::Tags => &self.tags,
        };
        let token = self.token().to_lowercase();
        let listed: Vec<&str> = if self.field == Field::Tags {
            self.value(self.field).split(',').map(str::trim).collect()
        } else {
            Vec::new()
        };
        suggestions
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
        if self.field == Field::Tags {
            let mut parts: Vec<String> = self
                .value(self.field)
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if !self.token().is_empty() {
                parts.pop();
            }
            parts.push(value.to_string());
            *self.value_mut() = parts.join(", ");
        } else {
            *self.value_mut() = value.to_string();
        }
        self.choice = 0;
    }

    /// Moves the highlighted suggestion, wrapping at both ends.
    fn cycle_choice(&mut self, delta: isize) {
        let len = self.matches().len();
        if len == 0 {
            self.choice = 0;
            return;
        }
        self.choice = (self.choice as isize + delta).rem_euclid(len as isize) as usize;
    }

    /// Takes the highlighted suggestion, if there is one.
    fn apply_choice(&mut self) {
        if let Some(value) = self.matches().get(self.choice).map(|s| s.to_string()) {
            self.apply(&value);
        }
    }
}

pub const HELP: &str =
    "a add  e edit  d del  D clear done  space done  s start/pause  x reset  n skip  q quit";

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
        self.persist();
        self.set_status("timer reset");
    }

    fn skip_phase(&mut self) {
        self.close_entry();
        self.timer.skip();
        self.active = None;
        self.set_status(format!("skipped, now {}", self.timer.phase.label()));
    }

    // ---- inputs --------------------------------------------------------

    fn open_form(&mut self, draft: Draft) {
        self.input = Some(Input {
            field: Field::Title,
            draft,
            projects: self.data.projects(),
            tags: self.data.tags(),
            choice: 0,
        });
    }

    fn focus_field(&mut self, delta: isize) {
        if let Some(input) = self.input.as_mut() {
            input.field = input.field.shift(delta);
            input.choice = 0;
        }
    }

    fn submit_input(&mut self) {
        // Keep the form open so the title can be fixed in place.
        let Some(input) = self.input.as_ref() else {
            return;
        };
        if input.draft.title.trim().is_empty() {
            self.set_status("title cannot be empty");
            return;
        }
        let Some(input) = self.input.take() else {
            return;
        };
        self.commit(input.draft);
    }

    /// Writes the finished form: adds a task, or replaces the edited one.
    fn commit(&mut self, draft: Draft) {
        let title = draft.title.trim().to_string();
        let project = draft.project.trim();
        let project = (!project.is_empty()).then(|| project.to_string());
        let mut tags: Vec<String> = Vec::new();
        for tag in draft
            .tags
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if !tags.iter().any(|t| t == tag) {
                tags.push(tag.to_string());
            }
        }
        let id = match draft.id {
            Some(id) => id,
            None => {
                let id = self.data.add_task(title.clone());
                self.selected = Some(id);
                self.set_status(format!("added {title}"));
                id
            }
        };
        if let Some(t) = self.data.task_mut(id) {
            t.title = title;
            t.project = project;
            t.tags = tags;
        }
        self.persist();
    }

    fn delete_task(&mut self, id: u64) {
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

    fn delete_completed(&mut self) {
        let index = self.selected_index().unwrap_or(0);
        if self
            .active
            .is_some_and(|id| self.data.task(id).is_some_and(|t| t.done))
        {
            self.close_entry();
            self.active = None;
        }
        let removed = self.data.remove_done();
        let ordered = self.data.ordered();
        if self.selected.is_none_or(|id| !ordered.contains(&id)) {
            self.selected = ordered
                .get(index.min(ordered.len().saturating_sub(1)))
                .copied();
        }
        self.persist();
        match removed {
            0 => self.set_status("no completed tasks"),
            n => self.set_status(format!("deleted {n} completed task(s)")),
        }
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
        let Some(confirm) = self.confirm_delete.take() else {
            return;
        };
        match (key.code, confirm) {
            (KeyCode::Char('y') | KeyCode::Char('Y'), Confirm::One(id)) => self.delete_task(id),
            (KeyCode::Char('y') | KeyCode::Char('Y'), Confirm::Completed) => {
                self.delete_completed()
            }
            _ => self.set_status("delete cancelled"),
        }
    }

    fn on_key_input(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.input = None,
            KeyCode::Enter => self.submit_input(),
            KeyCode::Tab => self.focus_field(1),
            KeyCode::BackTab => self.focus_field(-1),
            KeyCode::Down => {
                if let Some(input) = self.input.as_mut() {
                    input.cycle_choice(1);
                }
            }
            KeyCode::Up => {
                if let Some(input) = self.input.as_mut() {
                    input.cycle_choice(-1);
                }
            }
            KeyCode::Right => {
                if let Some(input) = self.input.as_mut() {
                    input.apply_choice();
                }
            }
            KeyCode::Backspace => {
                if let Some(input) = self.input.as_mut() {
                    input.value_mut().pop();
                    input.choice = 0;
                }
            }
            KeyCode::Char(c) => {
                if let Some(input) = self.input.as_mut() {
                    input.value_mut().push(c);
                    input.choice = 0;
                }
            }
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
            KeyCode::Char('a') => self.open_form(Draft::default()),
            KeyCode::Char('e') => {
                let draft = self
                    .selected_task_id()
                    .and_then(|id| self.data.task(id))
                    .map(|t| Draft {
                        id: Some(t.id),
                        title: t.title.clone(),
                        project: t.project.clone().unwrap_or_default(),
                        tags: t.tags.join(", "),
                    });
                if let Some(draft) = draft {
                    self.open_form(draft);
                }
            }
            KeyCode::Char('d') => {
                if let Some(id) = self.selected_task_id() {
                    self.confirm_delete = Some(Confirm::One(id));
                }
            }
            KeyCode::Char('D') => {
                if self.data.tasks.iter().any(|t| t.done) {
                    self.confirm_delete = Some(Confirm::Completed);
                } else {
                    self.set_status("no completed tasks");
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
                        self.active = None;
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

    /// Terminal title: run state, time left and the task (or the phase when no
    /// task is bound). Control characters are dropped so a stored title cannot
    /// inject escape sequences.
    pub fn title(&self) -> String {
        let secs =
            self.timer.remaining.as_secs() + u64::from(self.timer.remaining.subsec_millis() > 0);
        let work = self.timer.phase.is_work();
        let glyph = match (work, self.timer.running) {
            (true, true) => "▶",
            (false, true) => "☕",
            (_, false) => "⏸",
        };
        let label = if work {
            self.active_title()
                .unwrap_or_else(|| self.timer.phase.label().to_lowercase())
        } else {
            self.timer.phase.label().to_lowercase()
        };
        let label: String = label.chars().filter(|c| !c.is_control()).collect();
        format!("{glyph} {:02}:{:02} · {label}", secs / 60, secs % 60)
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
    fn title_shows_state_time_and_task() {
        let (mut app, _) = app_with_task();
        app.timer.start(Instant::now());
        assert_eq!(app.title(), "▶ 25:00 · write docs");

        app.timer.pause();
        app.timer.remaining = Duration::from_secs(65);
        assert_eq!(app.title(), "⏸ 01:05 · write docs");

        // A break has no task bound, so the phase stands in.
        app.active = None;
        app.timer.phase = Phase::ShortBreak;
        app.timer.remaining = Duration::from_secs(300);
        assert_eq!(app.title(), "⏸ 05:00 · short break");
        app.timer.start(Instant::now());
        assert_eq!(app.title(), "☕ 05:00 · short break");
    }

    #[test]
    fn title_drops_control_characters() {
        let (mut app, id) = app_with_task();
        app.data.task_mut(id).unwrap().title = "evil\x1b]0;pwned\x07".into();
        assert_eq!(app.title(), "⏸ 25:00 · evil]0;pwned");
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
        std::env::set_var(
            "XDG_DATA_HOME",
            std::env::temp_dir().join("minipom-tui-test"),
        );

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
            confirm_delete: None,
            status: String::new(),
            today: Local::now().date_naive(),
            should_quit: false,
            open: None,
            status_expiry: None,
        };

        app.delete_task(b);
        assert_eq!(app.selected, Some(c), "the row below takes its place");

        app.selected = Some(c);
        app.delete_task(c);
        assert_eq!(app.selected, Some(a), "the row above is used at the tail");

        app.selected = Some(a);
        app.delete_task(a);
        assert_eq!(app.selected, None, "an empty list has nothing to select");
    }

    #[test]
    fn clearing_completed_tasks_keeps_the_selection_and_drops_the_clock() {
        std::env::set_var(
            "XDG_DATA_HOME",
            std::env::temp_dir().join("minipom-tui-test"),
        );

        let (mut app, id) = app_with_task();
        app.data.task_mut(id).unwrap().done = true;
        let open = app.data.add_task("still open".into());
        app.confirm_delete = Some(Confirm::Completed);

        app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.data.task(id).is_none(), "the done task is gone");
        assert!(app.active.is_none(), "its clock is unbound");
        assert_eq!(app.selected, Some(open), "selection lands on a live task");
        assert_eq!(app.data.remove_done(), 0);
    }

    #[test]
    fn add_and_edit_collect_title_project_and_tags() {
        std::env::set_var(
            "XDG_DATA_HOME",
            std::env::temp_dir().join("minipom-tui-test"),
        );

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

        // Add: all three fields are on screen, tab moves between them.
        app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        // An empty title is refused without closing the form.
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.input.is_some(), "the form stays open to fix the title");
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().field, Field::Title);
        for c in "new".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().field, Field::Project);
        app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().matches(), vec!["work"]);
        // Right accepts the suggestion; tab moves on.
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().value(Field::Project), "work");
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().field, Field::Tags);

        // Tags: accepting a suggestion appends to what is already listed.
        for c in "urgent, de".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.input.as_ref().unwrap().matches(), vec!["deep"]);
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(
            app.input.as_ref().unwrap().value(Field::Tags),
            "urgent, deep"
        );
        // A listed tag is not offered again.
        for c in ", z".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert!(app.input.as_ref().unwrap().matches().is_empty());
        for _ in 0..3 {
            app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.input.is_none(), "saving closes the form");

        let added = app
            .data
            .ordered()
            .into_iter()
            .find(|id| *id != a && *id != b);
        let added = app.data.task(added.unwrap()).unwrap();
        assert_eq!(added.title, "new");
        assert_eq!(added.project.as_deref(), Some("work"));
        assert_eq!(added.tags, vec!["urgent", "deep"]);

        // Edit prefills every field; escaping keeps the stored task untouched.
        app.selected = Some(a);
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        let input = app.input.as_ref().unwrap();
        assert_eq!(input.value(Field::Title), "a");
        assert_eq!(input.value(Field::Project), "work");
        assert_eq!(input.value(Field::Tags), "deep, urgent");
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().field, Field::Project);
        app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().field, Field::Title);
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.data.task(a).unwrap().title, "a");
        assert_eq!(app.data.task(a).unwrap().project.as_deref(), Some("work"));
        assert_eq!(app.data.task(a).unwrap().tags, vec!["deep", "urgent"]);
    }

    #[test]
    fn suggestion_choice_wraps_and_accepts_the_highlighted_one() {
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
        let seed = app.data.add_task("seed".into());
        app.data.task_mut(seed).unwrap().tags = vec!["deep".into(), "design".into(), "docs".into()];

        app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(
            app.input.as_ref().unwrap().matches(),
            vec!["deep", "design", "docs"]
        );
        assert_eq!(app.input.as_ref().unwrap().choice, 0);

        // Down walks the list and wraps; Up walks back.
        for expected in [1, 2, 0] {
            app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
            assert_eq!(app.input.as_ref().unwrap().choice, expected);
        }
        app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().choice, 2);

        // Typing restarts from the first match.
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().choice, 0);
        assert_eq!(
            app.input.as_ref().unwrap().matches(),
            vec!["deep", "design"]
        );

        // The right arrow takes the highlighted one, not always the first.
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().value(Field::Tags), "design");
        assert_eq!(app.input.as_ref().unwrap().choice, 0);
    }

    #[test]
    fn done_pauses_only_the_clocked_task() {
        // One test, so the redirected store cannot race with a sibling.
        std::env::set_var(
            "XDG_DATA_HOME",
            std::env::temp_dir().join("minipom-tui-test"),
        );

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
        assert!(app.active.is_none(), "a finished task is no longer on");
    }
}
