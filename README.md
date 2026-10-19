# minipom-tui

A pomodoro timer, task list and time tracker that runs in the terminal.

## Features

- **Pomodoro** - 25/5/15 minute work, short break and long break cycle. A long
  break lands after every 4 work phases.
- **Tasks** - add, rename, complete and delete. The list is grouped by project
  (tasks with no project first, then alphabetically by project and title) with
  completed tasks last, and tags are shown inline.
- **Time tracking** - every work phase is logged against the task you started it
  on. Breaks are never logged. The side panel totals today's time per project.
- **Resume** - the clock and the day's pomodoro count survive a restart.
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
| `a`               | add a task                                                   |
| `e`               | rename the selected task                                     |
| `p`               | set the selected task's project (empty clears it)            |
| `t`               | set tags, comma separated                                    |
| `tab`             | in the project/tag editor, complete from values already used |
| `d`               | delete the selected task (asks first)                        |
| `space` / `enter` | mark done / undone                                           |
| `s`               | start or pause the timer                                     |
| `x`               | reset the current phase and unpair the task                  |
| `n`               | skip to the next phase (earns no pomodoro, logs no time)     |
| `?`               | show the key list                                            |
| `q`, `ctrl-c`     | quit                                                         |

Starting the timer binds it to the task under the cursor. To switch tasks
mid-session, move the cursor first and press `s` again.

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
