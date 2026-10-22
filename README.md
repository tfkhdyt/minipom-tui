# minipom-tui

A pomodoro timer, task list and time tracker that runs in the terminal.

## Features

- **Pomodoro** - 25/5/15 minute work, short break and long break cycle. A long
  break lands after every 4 work phases.
- **Tasks** - add, rename, complete and delete. The list has a heading per
  project (tasks with no project first, then alphabetically), with completed
  tasks last inside their project and tags shown inline.
- **Time tracking** - every work phase is logged against the task you started it
  on. Breaks are never logged. The side panel totals today's time per project.
  The task and Today timers read the same clock as the countdown, so pausing
  the pomodoro pauses the tracker too.
- **Resume** - the clock, the in-progress focus stretch and the day's pomodoro
  count survive a restart.
- **One file of data** - plain JSON, written on every change.

## Install

With a Rust toolchain (1.87 or newer):

```bash
cargo install --git https://github.com/tfkhdyt/minipom-tui
```

Or from a clone:

```bash
git clone https://github.com/tfkhdyt/minipom-tui
cd minipom-tui
cargo install --path .
```

Then run `minipom-tui`.

## Keys

| Key               | Action                                                       |
| ----------------- | ------------------------------------------------------------ |
| `j` / `k`, arrows | move the cursor                                              |
| `a`               | add a task (title, project, tags in one dialog)              |
| `e`               | edit the selected task in the same dialog                    |
| `↓` / `↑`         | in the task dialog, move to the next or previous field       |
| `←` / `→`         | in the task dialog, choose among suggestions, else move the cursor |
| `tab`             | in the task dialog, accept the highlighted suggestion        |
| `d`               | delete the selected task (asks first)                        |
| `D`               | delete every completed task (asks first)                     |
| `space` / `enter` | mark done / undone                                           |
| `s`               | start or pause the timer                                     |
| `x`               | reset the current phase and unpair the task                  |
| `n`               | skip to the next phase (earns no pomodoro, logs no time)     |
| `?`               | show the key list                                            |
| `q`, `ctrl-c`     | quit                                                         |

Starting the timer binds it to the task under the cursor. To switch tasks
mid-session, move the cursor first and press `s` again.

Add and edit open one dialog with the title, project (leave it empty for none)
and tags (comma separated) on screen together. `↓`/`↑` move between fields,
`←`/`→` highlight a suggestion, `tab` accepts it, `enter` saves and `esc`
discards everything. When no value matches, the arrows move the text cursor
instead and `tab` does nothing.

The key list sits in the bottom row and stays there. Status messages appear on
the row below it for 4 seconds, so a toast never hides the keys.

## Data

Everything lives in one JSON file:

```
$XDG_DATA_HOME/minipom-tui/data.json     # ~/.local/share/minipom-tui/data.json
```

It is written on every change. If it is ever unreadable it is moved aside
instead of being overwritten, and the reason shows up in the status bar.

Durations are fixed at 25/5/15 with a long break every 4 work phases.

<!-- shortcut: durations are hardcoded, add a settings block to data.json when someone asks -->

## Notifications

When a phase runs out the app rings the terminal bell and sends a desktop
notification (OSC 9), then waits for you to start the next phase:

```
WORK finished, SHORT BREAK next
```

Both are best effort. Terminals that do not understand OSC 9 ignore it, and
**tmux swallows it** before the outer terminal sees it, so inside tmux you get
the bell only. Turn the bell on in your terminal if you muted it.

## Development

```bash
cargo run --release   # run
cargo test            # tests
cargo fmt             # format
cargo clippy          # lints
```

The code is split by responsibility:

| File           | Role                                          |
| -------------- | --------------------------------------------- |
| `src/main.rs`  | terminal setup, event loop, notifications     |
| `src/app.rs`   | application state, key handling, timer wiring |
| `src/timer.rs` | pure pomodoro phase machine                   |
| `src/model.rs` | tasks, time entries, JSON persistence         |
| `src/ui.rs`    | rendering, layout, palette                    |

Keep `timer.rs` pure: it takes an explicit `now`, so its tests never sleep.

## Contributing

Bug reports and pull requests are welcome. Please run `cargo test` and
`cargo clippy` before opening a PR, and keep changes focused on one thing.

## License

[MIT](LICENSE) © Taufik Hidayat
