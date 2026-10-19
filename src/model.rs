//! Persisted state: tasks, time entries, and the JSON file they live in.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::PathBuf;

use chrono::{DateTime, Local, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::timer::Phase;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Task {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub done: bool,
}

/// One continuous stretch of focused time on a task, in unix seconds.
/// Entries are only written once they are closed, so a crash mid-session
/// loses the in-flight stretch (see App::open_entry).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    pub task_id: u64,
    pub start: i64,
    pub end: i64,
}

/// The clock's position at the last save. `None` on a fresh store.
#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct TimerState {
    pub phase: Phase,
    pub remaining_secs: u64,
    /// Work phases finished so far, which decides when a long break is due.
    pub work_done: u32,
    pub running: bool,
    /// Unix seconds when the snapshot was taken, so closed time can be counted.
    pub saved_at: i64,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Data {
    #[serde(default)]
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub next_id: u64,
    /// Completed work phases per local date, keyed by ISO `yyyy-mm-dd`.
    #[serde(default)]
    pub pomodoros: BTreeMap<String, u32>,
    /// Where the clock stood at the last save, so the app resumes there.
    #[serde(default)]
    pub timer: Option<TimerState>,
}

pub fn now_epoch() -> i64 {
    Local::now().timestamp()
}

/// `secs` as a date in the machine's local timezone.
fn local_date(secs: i64) -> Option<NaiveDate> {
    DateTime::from_timestamp(secs, 0).map(|dt| dt.with_timezone(&Local).date_naive())
}

impl Data {
    pub fn data_path() -> PathBuf {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("minipom-2").join("data.json")
    }

    /// Loads the store. A missing file is normal; an unreadable file is moved
    /// aside rather than overwritten, and the reason is returned for display.
    pub fn load() -> (Self, Option<String>) {
        let path = Self::data_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => {
                return (
                    Self::default(),
                    Some(format!("cannot read {}: {e}", path.display())),
                )
            }
        };
        match serde_json::from_str(&text) {
            Ok(data) => (data, None),
            Err(e) => {
                let backup = path.with_extension("json.corrupt");
                let moved = std::fs::rename(&path, &backup).is_ok();
                let note = if moved {
                    format!(
                        "data.json was unreadable ({e}); kept as {}",
                        backup.display()
                    )
                } else {
                    format!("data.json was unreadable ({e})")
                };
                (Self::default(), Some(note))
            }
        }
    }

    pub fn save(&self) -> io::Result<()> {
        let path = Self::data_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)
    }

    pub fn add_task(&mut self, title: String) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.tasks.push(Task {
            id,
            title,
            project: None,
            tags: Vec::new(),
            done: false,
        });
        id
    }

    pub fn task(&self, id: u64) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn task_mut(&mut self, id: u64) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    pub fn remove_task(&mut self, id: u64) {
        self.tasks.retain(|t| t.id != id);
        self.entries.retain(|e| e.task_id != id);
    }

    pub fn pomodoros_on(&self, day: NaiveDate) -> u32 {
        self.pomodoros.get(&day.to_string()).copied().unwrap_or(0)
    }

    /// Records a finished work phase and returns the day's new total.
    pub fn add_pomodoro(&mut self, day: NaiveDate) -> u32 {
        let count = self.pomodoros.entry(day.to_string()).or_default();
        *count += 1;
        *count
    }

    pub fn total_seconds(&self, task_id: u64) -> u64 {
        self.entries
            .iter()
            .filter(|e| e.task_id == task_id)
            .map(|e| (e.end - e.start).max(0) as u64)
            .sum()
    }

    /// Every project already used, sorted, for input suggestions.
    pub fn projects(&self) -> Vec<String> {
        let set: BTreeSet<&str> = self
            .tasks
            .iter()
            .filter_map(|t| t.project.as_deref())
            .filter(|p| !p.is_empty())
            .collect();
        set.into_iter().map(str::to_string).collect()
    }

    /// Every tag already used, sorted, for input suggestions.
    pub fn tags(&self) -> Vec<String> {
        let set: BTreeSet<&str> = self
            .tasks
            .iter()
            .flat_map(|t| t.tags.iter().map(String::as_str))
            .filter(|t| !t.is_empty())
            .collect();
        set.into_iter().map(str::to_string).collect()
    }

    /// Tracked time on `day`, grouped by project name, sorted by project.
    pub fn totals_by_project(&self, day: NaiveDate) -> Vec<(String, u64)> {
        let mut totals: BTreeMap<String, u64> = BTreeMap::new();
        for e in &self.entries {
            if local_date(e.start) != Some(day) {
                continue;
            }
            let name = self
                .task(e.task_id)
                .and_then(|t| t.project.clone())
                .unwrap_or_else(|| "(no project)".to_string());
            *totals.entry(name).or_default() += (e.end - e.start).max(0) as u64;
        }
        totals.into_iter().collect()
    }

    /// Tasks in display order: open before done, then grouped by project
    /// (tasks with no project first), then by title.
    pub fn ordered(&self) -> Vec<u64> {
        let mut tasks: Vec<&Task> = self.tasks.iter().collect();
        tasks.sort_by_key(|t| (t.done, t.project.clone(), t.title.to_lowercase()));
        tasks.iter().map(|t| t.id).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_with(entries: Vec<Entry>) -> Data {
        let mut d = Data::default();
        let id = d.add_task("write docs".into());
        d.task_mut(id).unwrap().project = Some("work".into());
        d.entries = entries
            .into_iter()
            .map(|e| Entry { task_id: id, ..e })
            .collect();
        d
    }

    #[test]
    fn total_seconds_sums_only_that_task() {
        let mut d = data_with(vec![
            Entry {
                task_id: 0,
                start: 0,
                end: 60,
            },
            Entry {
                task_id: 0,
                start: 100,
                end: 130,
            },
        ]);
        let other = d.add_task("other".into());
        d.entries.push(Entry {
            task_id: other,
            start: 0,
            end: 999,
        });
        assert_eq!(d.total_seconds(1), 90);
        assert_eq!(d.total_seconds(other), 999);
    }

    #[test]
    fn totals_by_project_ignores_other_days() {
        let today = Local::now().date_naive();
        let start = Local::now().date_naive().and_hms_opt(9, 0, 0).unwrap();
        let secs = start
            .and_local_timezone(Local)
            .earliest()
            .unwrap()
            .timestamp();

        let d = data_with(vec![
            Entry {
                task_id: 0,
                start: secs,
                end: secs + 3600,
            },
            Entry {
                task_id: 0,
                start: secs - 86400,
                end: secs - 86400 + 120,
            },
        ]);
        assert_eq!(d.totals_by_project(today), vec![("work".to_string(), 3600)]);
    }

    #[test]
    fn pomodoros_are_counted_per_day() {
        let mut d = Data::default();
        let day = NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
        assert_eq!(d.pomodoros_on(day), 0);
        assert_eq!(d.add_pomodoro(day), 1);
        assert_eq!(d.add_pomodoro(day), 2);
        assert_eq!(d.pomodoros_on(day), 2);
        // A new day starts empty; yesterday is kept.
        assert_eq!(d.pomodoros_on(day.succ_opt().unwrap()), 0);
        assert_eq!(d.pomodoros_on(day), 2);
    }

    #[test]
    fn timer_state_round_trips_and_old_files_still_load() {
        let d = Data {
            timer: Some(TimerState {
                phase: Phase::ShortBreak,
                remaining_secs: 123,
                work_done: 4,
                running: true,
                saved_at: 42,
            }),
            ..Data::default()
        };
        let back: Data = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        let back = back.timer.unwrap();
        assert_eq!(back.phase, Phase::ShortBreak);
        assert_eq!(back.remaining_secs, 123);
        assert_eq!(back.work_done, 4);

        let old: Data = serde_json::from_str("{\"tasks\":[]}").unwrap();
        assert!(old.timer.is_none());
    }

    #[test]
    fn ordered_puts_open_tasks_first_grouped_by_project() {
        let mut d = Data::default();
        let bbb = d.add_task("bbb".into());
        let aaa = d.add_task("aaa".into());
        let ccc = d.add_task("ccc".into());
        // Tasks with no project sort first; gaining a project moves a task after them.
        assert_eq!(d.ordered(), vec![aaa, bbb, ccc]);
        d.task_mut(aaa).unwrap().project = Some("z".into());
        d.task_mut(ccc).unwrap().done = true;
        assert_eq!(d.ordered(), vec![bbb, aaa, ccc]);
    }
}
