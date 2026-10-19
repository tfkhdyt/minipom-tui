# minipom-2

A pomodoro timer, task list and time tracker that runs in the terminal.

Inspired by [super-productivity](https://github.com/super-productivity/super-productivity),
rebuilt as a native Rust TUI. It is the terminal counterpart to
[minipom](https://github.com/tfkhdyt/minipom), which covers the timer only.

## What it does

- **Pomodoro** - 25/5/15 minute work, short break and long break cycle. A long
  break lands after every 4 work phases.
- **Tasks** - add, rename, complete and delete. The list is grouped by project
  (tasks with no project first, then alphabetically by project and title) with
  completed tasks last, and tags are shown inline.
- **Time tracking** - every work phase is logged against the task you started it
  on. Breaks are never logged. The side panel totals today's time per project.

## Build and run

```bash
cargo run --release
```

## Keys

| Key | Action |
| --- | --- |
| `j` / `k`, arrows | move the cursor |
| `a` | add a task |
| `e` | rename the selected task |
| `p` | set the selected task's project (empty clears it) |
| `t` | set tags, comma separated |
| `tab` | in the project/tag editor, complete from values already used |
| `d` | delete the selected task (asks first) |
| `space` / `enter` | mark done / undone |
| `s` | start or pause the timer |
| `x` | reset the current phase and unpair the task |
| `n` | skip to the next phase (earns no pomodoro, logs no time) |
| `?` | show the key list |
| `q`, `ctrl-c` | quit |

Starting the timer binds it to the task under the cursor. To switch tasks
mid-session, move the cursor first and press `s` again.

The key list sits in the bottom row and stays there. Status messages appear on
the row below it for 4 seconds, so a toast never hides the keys.

## Look

Every colour is an explicit entry from the 256-colour cube, never a base ANSI
colour, because base colours are theme-controlled: bright black (8) is a *light*
grey in many themes, so using it for a surface paints white-on-light and hides
the task status. The palette is a handful of constants at the top of `src/ui.rs`:

| Constant | Value | Used for |
| --- | --- | --- |
| `INK` | 234 `#1c1c1c` | text on the cyan status toast |
| `SURFACE` | 236 `#303030` | the selected row |
| `BORDER` | 245 `#8a8a8a` | panel frames, which carry no information |
| `TRACK` | 238 `#444444` | the unfilled part of the timer bar |
| `TEXT` | 252 `#d0d0d0` | all text, including status glyphs: 8.6:1 or better on both surfaces |

The focus timer draws its countdown in a 3x5 block font, coloured by phase: 203
`#ff5f5f` for work, 114 `#87d787` for a short break, 81 `#5fd7ff` for a long one.

No background is painted, so the terminal's own background shows through: a
translucent terminal stays translucent, and an opaque one simply looks normal.
That is also why the colours are palette entries rather than truecolor `Rgb`:
terminals and tmux without RGB support drop `48;2;...` silently.

## Notifications

When a phase runs out the app rings the terminal bell and sends a desktop
notification (OSC 9), then waits for you to start the next phase:

```
WORK finished, SHORT BREAK next
```

Both are best effort. Terminals that do not understand OSC 9 ignore it, and
**tmux swallows it** before the outer terminal sees it, so inside tmux you get
the bell only. Turn the bell on in your terminal if you muted it.

## Data

Everything lives in one JSON file:

```
$XDG_DATA_HOME/minipom-2/data.json     # ~/.local/share/minipom-2/data.json
```

It is written on every change. If it is ever unreadable it is moved aside
instead of being overwritten, and the reason shows up in the status bar.

Durations are fixed at 25/5/15 with a long break every 4 work phases.

<!-- shortcut: durations are hardcoded, add a settings block to data.json when someone asks -->
