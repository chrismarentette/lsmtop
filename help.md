# lsmtop

A terminal monitor for llama.cpp and llama-swap inference services: live
token-throughput graphs, per-model lanes, GPU vitals, and the engine
journal. lsmtop only observes — it never starts, stops, or configures
your inference servers. Run `lsmtop --help` any time for the option
list; this file is the full guide.

## Layout

The dashboard shows, top to bottom:

- **Title bar** — the model/target name, loaded lanes (`2/11`), service
  state and PID, with a live clock at the right edge.
- **Status line** — GPU/APU utilization and VRAM (or unified RAM on an
  APU), plus the device name when the system reports one.
- **Graphs** — PROMPT t/s, GEN t/s, KV≈ %, and GPU/APU utilization.
- **Lanes table** — one row per model lane: id, active marker, prompt
  t/s, TOTAL, gen t/s, KV≈%, cache, context use, accept rate.
- **Journal** — engine log lines, color-coded per service.
- **Strip** — `Journal  Lanes  Settings  Quit`: click a word or press
  its key (`j`, `L`, `s`, `q`).

## Keys

| Key | Action |
|-----|--------|
| `q` / `Esc` | leave Settings or the full-screen journal; on the dashboard they quit. `Ctrl+C` always quits |
| `s` | open Settings |
| `j` | journal size: off → four lines → ten lines → full screen → closes back to off |
| `J` | journal on/off — returns to whatever size it was last on |
| `p` `g` `v` `u` | cycle PROMPT / GEN / KV / GPU graph size: full → half → one-line |
| `P` `G` `V` `U` | show/hide that graph |
| `l` | lanes table: active models ↔ every model |
| `L` | lanes table on/off |
| `↑` `↓` (or `k`) | journal: read back / scroll down (reaching the bottom resumes live following) |
| `PageUp` `PageDown` | journal: jump ten lines |
| `End` | journal: jump back to the live tail |
| `c` | journal: clear the view |

In Settings: `↑`/`↓` (or `Tab`) move between rows, `←`/`→` choose the
`[ + ]` / `[ - ]` buttons, `Enter`/`space` presses, `q`/`Esc` go back.
Every change saves and applies immediately.

## Mouse

- Click any strip or footer word to press it.
- In Settings, click a chip or `[ + ]` / `[ - ]` to use it.
- Scroll wheel over the journal reads back through history; scrolling
  down to the bottom resumes live following.
- Scroll wheel over a graph pans history when the graph is squeezed
  narrower than its stored range.

## Display options (Settings)

All choices persist across runs.

- **tick rate** — sampling interval, 500–5000 ms in 50 ms steps
  (default 2000). Applies at once.
- **chars** — how plots are drawn: `braille` (dots), `block` (bar
  columns), `line` (plain box-drawing stair-steps).
- **fill** — area under the curve: `bar`, `area`, `line`. With
  `chars = line` this row offers `off`, `vert`, `horiz` instead.
- **scale** — where the value ruler sits: `right` (default), `left`,
  `both`, `off`. The colored number riding the ruler is the current
  value; `off` removes the ruler and the number together.
- **colors** — how lines are colored: `gradient` (along the line's
  position), `flow` (pulses travel right-to-left with time), `solid`
  (each line takes its value's heat color).
- **ramp** — the palette behind colors: `heat`, `spectrum`, `ice`.
- **timebase** — `scroll` slides one column per tick; `squish` keeps the
  last ~30 seconds exactly real-time and compresses older history.
- **journal** — panel height: `off`, `four`, `ten` (full screen is the
  `j` key).
- **lanes** — `on` shows active models, `all` shows every loaded model,
  `off` hides the table.
- Per graph: size `full` / `half` / `single` and visibility `on` / `off`.

## What the numbers mean

- **GEN t/s** — decode speed from the engine's own counters. Prompt
  processing is never counted as generation.
- **PROMPT t/s** — prompt-eval speed, averaged over the current busy
  run: the same number the engine prints when the prompt completes.
- **KV≈ %** — how much of the model's context window is occupied.
- **hit%** — prefix-cache reuse: how much of the work was skipped
  because the engine already had it.
- **TOTAL** (lane table) — the running token count read straight from
  the engine's own journal lines, not estimated.
- Graphs plot one engine sample per tick, newest at the right edge.
  Nothing is smoothed: real dips and spikes appear exactly as the
  engine had them. `squish` timebase keeps the last ~30 seconds exact
  while still showing your full stored history.

## Reading the graphs

- Each panel's label row explains what it plots: PROMPT — average and
  peak prompt t/s; GEN — average, peak, and `gen%` (share of the
  model's output room in use); KV — `hit%` cache hits and `proc%` of
  time spent prompting; GPU/APU — utilization, plus `clk` (core clock)
  and `pwr` (power) where your hardware reports them.
- The ruler gives the value scale; the colored number on it is the live
  value. Values above 999 shorten (`1k`, `2½k`).
- A panel shrunk to a single line becomes a sparkline of the same
  series: dots, growing bars, or a stroke ladder, colored by the same
  `colors` setting.
- Titles adapt automatically to your hardware (`GPU %` or `APU %`).
  Missing hardware readings are left blank rather than guessed.

## The journal

Lines are colored per service and follow live by default. Scrolling up
parks the view so you can read; the moment you reach the bottom again
(or press `End`), following resumes on its own. The full-screen journal
is plain log rows; `c` clears the view.

## Configuration file

`~/.lsmtop/config.json` is created on first run and updated whenever you
change anything in Settings — endpoints, layout, colors, tick rate all
live there. Delete it to return to defaults. Point the `LSMTOP_CONFIG`
environment variable somewhere else to keep a second setup.
