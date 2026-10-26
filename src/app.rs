//! Application state, key handling and the pomodoro/time-tracking wiring.

use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

use crate::model::{now_epoch, Data, Entry, OpenEntry, TimerState};
use crate::timer::{Phase, Timer};

/// Cells the task name may use in the terminal title. The state, clock and
/// separator sit outside this budget, so the whole title stays compact.
const TITLE_LABEL_WIDTH: usize = 40;

/// Cut `label` to `max` cells, marking the cut with an ellipsis. Wide glyphs
/// are never split, so the result can come in a cell short of the limit.
fn truncate_label(label: &str, max: usize) -> String {
    if label.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>() <= max {
        return label.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for c in label.chars() {
        let w = c.width().unwrap_or(0);
        if width + w > max.saturating_sub(1) {
            break;
        }
        out.push(c);
        width += w;
    }
    out.push('…');
    out
}

pub struct App {
    pub data: Data,
    pub timer: Timer,
    /// Selected task id. Kept by id, not index, so re-sorting never moves it.
    pub selected: Option<u64>,
    /// Task the running pomodoro is credited to. It stays bound through short
    /// and long breaks, so the next work phase keeps the same focus.
    pub active: Option<u64>,
    pub input: Option<Input>,
    pub confirm_delete: Option<Confirm>,
    pub status: String,
    pub today: NaiveDate,
    pub should_quit: bool,
    /// A focus stretch not yet written to the log. Its length is read from the
    /// clock, so the tracker and the countdown can never disagree.
    open: Option<OpenEntry>,
    /// When the status message gives way back to the key list.
    status_expiry: Option<Instant>,
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

    /// Next field for the down/up keys, wrapping at both ends.
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
    /// Which match `tab` will take.
    pub choice: usize,
    /// Byte offset of the text cursor within the focused field.
    pub cursor: usize,
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

    /// Whether left/right can pick a suggestion right now.
    fn suggestible(&self) -> bool {
        self.field != Field::Title && !self.matches().is_empty()
    }

    /// Moves the text cursor by one character, staying inside the value.
    fn move_cursor(&mut self, delta: isize) {
        let next = {
            let text = self.value(self.field);
            if delta < 0 {
                text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map(|(i, _)| i)
            } else {
                text[self.cursor..]
                    .chars()
                    .next()
                    .map(|c| self.cursor + c.len_utf8())
            }
        };
        if let Some(next) = next {
            self.cursor = next;
        }
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
        self.cursor = self.value(self.field).len();
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
    "a add  e edit  d del  D clear done  space done  s start/pause task  x reset  n skip  q quit";

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
        let mut active = None;
        let mut open = None;
        if let Some(state) = data.timer {
            timer.phase = state.phase;
            timer.work_done = state.work_done;
            timer.running = state.running;
            let gap = (now_epoch() - state.saved_at).max(0) as u64;
            timer.remaining = resume_remaining(state, gap, timer.total());
            // Keep the focused task across a restart, including one taken
            // during a break.
            active = state.active.filter(|id| data.task(*id).is_some());
            // A work stretch that was running at the last save stays bound to
            // its task, so the tracker counts the same time the clock did.
            if state.running && state.phase.is_work() {
                if let Some(entry) = state.open.filter(|e| data.task(e.task_id).is_some()) {
                    active = Some(entry.task_id);
                    open = Some(entry);
                }
            }
        }
        let mut app = Self {
            data,
            timer,
            selected: None,
            active,
            input: None,
            confirm_delete: None,
            status: warning.unwrap_or_else(|| HELP.to_string()),
            today,
            should_quit: false,
            open,
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
            open: self.open,
            active: self.active,
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
        if self.open.is_some() || !self.timer.running || !self.timer.phase.is_work() {
            return;
        }
        if let Some(id) = self.active {
            self.open = Some(OpenEntry {
                task_id: id,
                start: now_epoch(),
                base_secs: self.timer.elapsed().as_secs(),
            });
        }
    }

    fn close_entry(&mut self) {
        let Some(entry) = self.open.take() else {
            return;
        };
        // The clock decides how long the stretch was. Using the wall clock
        // would log time the timer never counted, for example while the
        // machine was suspended.
        let secs = self
            .timer
            .elapsed()
            .as_secs()
            .saturating_sub(entry.base_secs);
        if secs == 0 {
            return;
        }
        self.data.entries.push(Entry {
            task_id: entry.task_id,
            start: entry.start,
            end: entry.start + secs as i64,
        });
        self.persist();
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

    /// Binds the clock to `id` and runs it. Switching away from another task
    /// banks the old stretch first, so no time is credited twice.
    fn start_task(&mut self, id: u64) {
        if self.active != Some(id) {
            self.close_entry();
            self.active = Some(id);
        }
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

    fn toggle_timer(&mut self) {
        if self.timer.running {
            self.pause_timer();
            return;
        }
        if !self.timer.phase.is_work() {
            // Breaks are not credited to a task, so starting one must not move
            // the focus away from the task it is preparing for.
            self.timer.start(Instant::now());
            self.persist();
            self.set_status("break started");
            return;
        }
        if let Some(id) = self.selected_task_id().or(self.active) {
            self.start_task(id);
        } else {
            self.set_status("add a task first, then start the timer");
        }
    }

    /// `s` toggles start/pause only when the cursor is on the task the clock is
    /// credited to. On any other task it starts that task right away, so
    /// moving the cursor and pressing `s` switches tasks in one step.
    fn start_or_toggle(&mut self) {
        if self.timer.phase.is_work() {
            if let Some(id) = self.selected_task_id() {
                if self.active != Some(id) {
                    self.start_task(id);
                    return;
                }
            }
        }
        self.toggle_timer();
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
        // Skipping is not the same as resetting: the focused task survives so
        // a skipped break still leads back to it.
        self.persist();
        self.set_status(format!("skipped, now {}", self.timer.phase.label()));
    }

    /// Flips a task's done flag. Finishing the task the clock is credited to
    /// ends the session, so no more time is logged against it.
    fn toggle_done(&mut self, id: u64) {
        let done = self.data.task_mut(id).map(|t| {
            t.done = !t.done;
            t.done
        });
        if done == Some(true) && self.active == Some(id) {
            self.pause_timer();
            self.active = None;
        }
        self.persist();
    }

    // ---- inputs --------------------------------------------------------

    fn open_form(&mut self, draft: Draft) {
        let cursor = draft.title.len();
        self.input = Some(Input {
            field: Field::Title,
            draft,
            projects: self.data.projects(),
            tags: self.data.tags(),
            choice: 0,
            cursor,
        });
    }

    fn focus_field(&mut self, delta: isize) {
        if let Some(input) = self.input.as_mut() {
            input.field = input.field.shift(delta);
            input.choice = 0;
            input.cursor = input.value(input.field).len();
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
        if self.active == Some(id) {
            // The time goes with the task; dropping the stretch here avoids
            // writing an entry for an id that is about to disappear.
            self.open = None;
            self.active = None;
        }
        self.data.remove_task(id);
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

    /// A left click on the task list. The clicked row becomes the cursor; a
    /// click on its checkbox also marks it done or undone, like `space`.
    /// Dialogs own the screen, so clicks behind one are ignored.
    pub fn click_task(&mut self, id: u64, checkbox: bool) {
        if self.input.is_some() || self.confirm_delete.is_some() {
            return;
        }
        self.selected = Some(id);
        if checkbox {
            self.toggle_done(id);
        }
    }

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
            KeyCode::Tab => {
                if let Some(input) = self.input.as_mut() {
                    input.apply_choice();
                }
            }
            KeyCode::BackTab => self.focus_field(-1),
            KeyCode::Down => self.focus_field(1),
            KeyCode::Up => self.focus_field(-1),
            KeyCode::Left => {
                if let Some(input) = self.input.as_mut() {
                    if input.suggestible() {
                        input.cycle_choice(-1);
                    } else {
                        input.move_cursor(-1);
                    }
                }
            }
            KeyCode::Right => {
                if let Some(input) = self.input.as_mut() {
                    if input.suggestible() {
                        input.cycle_choice(1);
                    } else {
                        input.move_cursor(1);
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(input) = self.input.as_mut() {
                    let at = input.cursor;
                    let prev = input.value(input.field)[..at]
                        .char_indices()
                        .next_back()
                        .map(|(i, _)| i);
                    if let Some(prev) = prev {
                        input.value_mut().remove(prev);
                        input.cursor = prev;
                        input.choice = 0;
                    }
                }
            }
            KeyCode::Char(c) => {
                if let Some(input) = self.input.as_mut() {
                    let at = input.cursor;
                    input.value_mut().insert(at, c);
                    input.cursor = at + c.len_utf8();
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
                    self.toggle_done(id);
                }
            }
            KeyCode::Char('s') => self.start_or_toggle(),
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
            // Bank the work stretch but keep the task focused for the break.
            self.close_entry();
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
        // The clock keeps running while the app is closed, so leave a fresh
        // stretch open. On the next launch it picks up the away time, keeping
        // the task and Today totals in step with the countdown.
        self.open_entry();
        self.persist();
    }

    pub fn active_title(&self) -> Option<String> {
        self.active
            .and_then(|id| self.data.task(id))
            .map(|t| t.title.clone())
    }

    /// Terminal title: run state, time left and the task (or the phase when no
    /// task is bound). Control characters are dropped so a stored title cannot
    /// inject escape sequences, and a long task name is cut to one line's worth
    /// of cells so the title bar stays readable.
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
        let label = truncate_label(&label, TITLE_LABEL_WIDTH);
        format!("{glyph} {:02}:{:02} · {label}", secs / 60, secs % 60)
    }

    /// The focus stretch in progress, as (task id, seconds so far). It is not
    /// in the log yet, so callers add it in to what they display. The seconds
    /// come from the pomodoro clock, so every panel shows the same number.
    pub fn live(&self) -> Option<(u64, u64)> {
        let open = self.open.as_ref()?;
        let secs = self
            .timer
            .elapsed()
            .as_secs()
            .saturating_sub(open.base_secs);
        Some((open.task_id, secs))
    }
}

#[cfg(test)]
impl App {
    /// A bare app over `data`, for tests in other modules that need the state
    /// but not the store on disk.
    pub(crate) fn for_test(data: Data) -> Self {
        Self {
            data,
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
        }
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

        // With no task bound, the phase stands in.
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
    fn title_truncates_long_task_names() {
        let (mut app, id) = app_with_task();
        app.data.task_mut(id).unwrap().title = "x".repeat(80);
        assert_eq!(
            app.title(),
            format!("⏸ 25:00 · {}…", "x".repeat(TITLE_LABEL_WIDTH - 1))
        );
    }

    #[test]
    fn truncate_label_keeps_wide_glyphs_whole() {
        let label = "漢".repeat(30);
        let out = truncate_label(&label, 40);
        assert_eq!(out, format!("{}…", "漢".repeat(19)));
    }

    #[test]
    fn resume_counts_time_while_the_app_was_closed() {
        let state = |remaining_secs, running| TimerState {
            phase: Phase::Work,
            remaining_secs,
            work_done: 0,
            running,
            saved_at: 0,
            open: None,
            active: None,
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
    fn live_time_is_the_phase_clock_not_the_wall_clock() {
        let (mut app, id) = app_with_task();
        let total = app.timer.total();
        app.timer.running = true;
        app.open = Some(OpenEntry {
            task_id: id,
            start: 100,
            base_secs: 0,
        });

        app.timer.remaining = total - Duration::from_secs(90);
        assert_eq!(app.live(), Some((id, 90)));

        // A paused clock leaves the tracked number where it was.
        app.timer.pause();
        assert_eq!(app.live(), Some((id, 90)));

        // A stretch that opened midway through the phase only counts its own
        // seconds, so switching tasks cannot credit time twice.
        app.open = Some(OpenEntry {
            task_id: id,
            start: 200,
            base_secs: 60,
        });
        app.timer.remaining = total - Duration::from_secs(120);
        assert_eq!(app.live(), Some((id, 60)));
    }

    #[test]
    fn closing_an_entry_logs_exactly_the_seconds_the_clock_ran() {
        std::env::set_var(
            "XDG_DATA_HOME",
            std::env::temp_dir().join("minipom-tui-test-clock"),
        );

        let (mut app, id) = app_with_task();
        let total = app.timer.total();
        app.timer.running = true;
        app.open = Some(OpenEntry {
            task_id: id,
            start: 1_000,
            base_secs: 0,
        });

        // The phase ran out. Logging the wall clock here could add time the
        // timer never counted, for example after a suspend.
        app.timer.remaining = Duration::ZERO;
        app.close_entry();

        let entry = app.data.entries.last().unwrap();
        assert_eq!(entry.start, 1_000);
        assert_eq!(entry.end, 1_000 + total.as_secs() as i64);
        assert_eq!(app.data.total_seconds(id), total.as_secs());
        assert_eq!(app.live(), None);
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
    fn s_toggles_the_clocked_task_but_starts_any_other() {
        std::env::set_var(
            "XDG_DATA_HOME",
            std::env::temp_dir().join("minipom-tui-test-s"),
        );

        let (mut app, id) = app_with_task();
        let other = app.data.add_task("other".into());
        let s = || KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);

        // On the task the clock is credited to, s starts and then pauses.
        app.selected = Some(id);
        app.on_key(s());
        assert!(app.timer.running, "first press starts the focused task");
        app.on_key(s());
        assert!(
            !app.timer.running,
            "second press pauses instead of restarting"
        );
        assert_eq!(app.active, Some(id), "pausing keeps the task bound");

        // On another task, s starts that task right away instead of pausing.
        app.selected = Some(other);
        let remaining = app.timer.remaining;
        app.on_key(s());
        assert!(app.timer.running, "a different task starts immediately");
        assert_eq!(app.active, Some(other));
        assert_eq!(app.timer.remaining, remaining, "the clock is not restarted");
        assert!(
            app.open.is_some_and(|e| e.task_id == other),
            "the stretch is credited to the new task"
        );

        // Switching again while running keeps the clock going and moves the
        // open stretch, without pausing.
        app.selected = Some(id);
        app.on_key(s());
        assert!(app.timer.running);
        assert_eq!(app.active, Some(id));
        assert!(app.open.is_some_and(|e| e.task_id == id));
    }

    #[test]
    fn breaks_keep_the_focused_task() {
        std::env::set_var(
            "XDG_DATA_HOME",
            std::env::temp_dir().join("minipom-tui-test-break-focus"),
        );

        let (mut app, id) = app_with_task();
        // The first work phase earns a long break.
        app.timer.settings.long_every = 1;
        app.timer.start(Instant::now());
        app.timer.remaining = Duration::ZERO;

        assert_eq!(app.tick(), Some(Phase::Work));
        assert_eq!(app.timer.phase, Phase::LongBreak);
        assert_eq!(app.active, Some(id), "the focus survives the long break");
        assert_eq!(app.active_title().as_deref(), Some("write docs"));

        // The same holds for a short break.
        app.timer.settings.long_every = 4;
        app.timer.phase = Phase::Work;
        app.timer.work_done = 0;
        app.timer.running = true;
        app.timer.remaining = Duration::ZERO;

        assert_eq!(app.tick(), Some(Phase::Work));
        assert_eq!(app.timer.phase, Phase::ShortBreak);
        assert_eq!(app.active, Some(id), "the focus survives the short break");
    }

    #[test]
    fn starting_or_skipping_a_break_keeps_the_focused_task() {
        std::env::set_var(
            "XDG_DATA_HOME",
            std::env::temp_dir().join("minipom-tui-test-break-start"),
        );

        let (mut app, id) = app_with_task();
        let other = app.data.add_task("other".into());
        app.active = Some(id);
        app.timer.phase = Phase::ShortBreak;
        app.timer.remaining = app.timer.total();
        app.selected = Some(other);

        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert!(app.timer.running, "s starts the break");
        assert_eq!(
            app.active,
            Some(id),
            "starting a break does not move the focus to the cursor"
        );

        app.timer.pause();
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(app.timer.phase, Phase::Work, "skipping the break moves on");
        assert_eq!(app.active, Some(id), "skipping keeps the focused task");
    }

    #[test]
    fn clicking_selects_a_task_and_the_checkbox_toggles_it() {
        std::env::set_var(
            "XDG_DATA_HOME",
            std::env::temp_dir().join("minipom-tui-test-click"),
        );

        let (mut app, first) = app_with_task();
        let second = app.data.add_task("second".into());
        app.selected = Some(first);

        // A click anywhere on the row just moves the cursor.
        app.click_task(second, false);
        assert_eq!(app.selected, Some(second));
        assert!(!app.data.task(second).unwrap().done);

        // The checkbox behaves like space.
        app.click_task(second, true);
        assert!(app.data.task(second).unwrap().done);
        app.click_task(second, true);
        assert!(!app.data.task(second).unwrap().done);

        // Finishing the task the clock is credited to unbinds it.
        app.click_task(first, true);
        assert!(app.data.task(first).unwrap().done);
        assert!(app.active.is_none());

        // A dialog owns the screen, so clicks behind it are ignored.
        app.open_form(Draft::default());
        app.click_task(second, true);
        assert_eq!(app.selected, Some(first));
        assert!(!app.data.task(second).unwrap().done);
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

        // Add: all three fields are on screen, up/down moves between them.
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
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().field, Field::Project);
        app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().matches(), vec!["work"]);
        // Tab accepts the suggestion; down moves on.
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().value(Field::Project), "work");
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().field, Field::Tags);

        // Tags: accepting a suggestion appends to what is already listed.
        for c in "urgent, de".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.input.as_ref().unwrap().matches(), vec!["deep"]);
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
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
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().field, Field::Project);
        app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
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
        // The title has no suggestions, so the arrows move the text cursor
        // and tab has nothing to accept.
        for c in "ac".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().value(Field::Title), "abc");
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().value(Field::Title), "abcd");
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().value(Field::Title), "abcd");
        for _ in 0..4 {
            app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
        assert_eq!(app.input.as_ref().unwrap().value(Field::Title), "");
        assert_eq!(app.input.as_ref().unwrap().choice, 0);
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(
            app.input.as_ref().unwrap().matches(),
            vec!["deep", "design", "docs"]
        );
        assert_eq!(app.input.as_ref().unwrap().choice, 0);

        // Right walks the list and wraps; Left walks back.
        for expected in [1, 2, 0] {
            app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
            assert_eq!(app.input.as_ref().unwrap().choice, expected);
        }
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().choice, 2);

        // Typing restarts from the first match.
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().choice, 0);
        assert_eq!(
            app.input.as_ref().unwrap().matches(),
            vec!["deep", "design"]
        );

        // Tab takes the highlighted one, not always the first.
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().value(Field::Tags), "design");
        assert_eq!(app.input.as_ref().unwrap().choice, 0);

        // Nothing matches, so the arrows move the cursor and tab accepts nothing.
        app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().cursor, 6);
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input.as_ref().unwrap().value(Field::Tags), "designz");
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
