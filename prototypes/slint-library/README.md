# Slint prototype of the Songs view

**Decision (2026-10-27): the app stays on egui.** Slint used about a fifth of
the memory, but the port would rebuild the whole working UI, adds a second
language and a licence condition, and costs more CPU when scrolling on large
screens. This prototype is kept only as a record of the comparison.

A throwaway prototype for deciding whether to port the desktop UI from egui to
[Slint](https://slint.dev) with its software renderer. It loads the real
library through `yerg-core`, shows the Songs view laid out like the egui app
(top bar, sidebar, sortable table, empty Now Playing panel, transport bar) and
starts the audio engine, which opens the output device as the real app does.

It is read-only: sorting, search, selection and rating clicks work on screen,
but ratings are never written back, and double-click does not play.

It has its own `[workspace]`, so the main build never compiles Slint.

```bash
cd prototypes/slint-library
cargo run --release
```

`YERG_DEBUG_AUTOSCROLL=<px per second>` scrolls the table continuously and
logs the rate every 5 seconds. The egui app has the same hook, and
`YERG_DEBUG_VIEW=songs` opens it on the Songs view; neither saves UI state
when these are set.

## Results (2026-10-27)

Windows 10, AMD Radeon RX 6600, 16 logical cores, 2560×1440 monitor, real
library of 6,973 tracks, nothing playing. Normal window is 1240×780. Scrolling
is 2,400 px/s at 60 updates a second. Memory is private working set.

| | egui (OpenGL) | Slint (software) |
|---|---|---|
| Idle memory, normal window | 81.6 MB | 17.0 MB |
| Idle memory, maximized | 84.5 MB | 28.5 MB |
| Memory while scrolling, maximized | 121 MB | 29.4 MB |
| Idle CPU | 0% | 0 to 0.2% |
| Scrolling CPU, normal window | 16.2% of a core | 20.2% of a core |
| Scrolling CPU, maximized | 23 to 26% of a core, plus 8% GPU | 41.5 to 42.6% of a core, no GPU |

Slint's software renderer cannot report frames, so its rate is scroll updates
(at most one frame each); egui drew a steady 60 fps.
