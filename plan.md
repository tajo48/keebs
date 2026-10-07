# plan.md — Wayland text-input-aware auto-show for the keebs OSK

## Goal

Show the on-screen keyboard automatically when the user focuses a text field
(on Wayland), and hide it when the field loses focus — without making SDL3
responsible for any protocol work. Manual toggling (Steam + X, mouse) keeps
working unchanged.

**Non-goal (explicitly):** detecting *physical keyboard typing* globally.
A normal Wayland client cannot snoop key events — that's a security property
of the protocol, not a limitation we can code around. The trigger is
**text-field focus**, not keystrokes.

## Architecture

SDL3 stays a pure renderer/input surface. A separate thread owns a Wayland
connection and drives the existing `OskState.visible` flag:

```
        Wayland compositor
        │ zwp_input_method_manager_v2 (KWin, sway, Hyprland, …)
        │   or compositor-specific bridge (GNOME)
        ▼
  src/wl_im.rs  — IM monitor thread
        │ activate / deactivate events
        ▼
  OskState.show() / close()   (already AtomicBool — thread-safe today)
        ▼
  src/osk_main.rs render loop — already reacts to visibility flips
        ▼
  Text injection: existing uinput.rs (default) or IM commit_string (later)
```

No changes to the SDL event loop's role: it keeps rendering, handling
controller/mouse, and flipping `visible` via `screen.show()/hide()`.

## State machine

```rust
enum KeyboardState {
    Hidden,     // no active text input
    Visible,    // text-input activated → auto-show
    Composing,  // later: shift/emoji/hold states, IM-side composition
}
```

Rules:

- `activate` → `Visible` (auto-show), unless the user manually closed the OSK
  for this activation.
- `deactivate` → `Hidden` (auto-hide) after a short grace period (~250 ms),
  since focus flaps happen during window switches.
- Manual close (B / L4/L5 / Steam released) while auto-shown sets a
  suppress flag valid until the next `deactivate` — so the OSK doesn't
  immediately pop back up while the field is still focused.
- Manual open (Steam + X) wins over everything and disables auto-hide until
  the next manual close.

## Compositor support matrix (decides what we build)

| Compositor | Mechanism | Auto-show feasible? |
|---|---|---|
| KWin (KDE Plasma) | `input-method-unstable-v2` | ✅ native |
| sway / Hyprland / wlroots | `input-method-unstable-v2` | ✅ native |
| Mutter (GNOME) | no input-method-v2; uses IBus internally | ⚠ needs a GNOME Shell extension bridge or stay manual |
| X11 sessions | no Wayland protocols | ❌ manual toggle only (current behavior) |

Strategy: implement the input-method-v2 path first (covers KDE + wlroots
family), treat GNOME and X11 as fallbacks where the OSK stays manually
toggled. Do not try to be clever with GNOME from a plain Wayland client —
Mutter simply won't tell us.

## Phases

### Phase 0 — deps + module skeleton

- `cargo add wayland-client wayland-protocols wayland-protocols-wlr`
  (pure-Rust stacks; `wayland-scanner` comes in transitively — no Cargo.toml
  hand-editing).
- New `src/wl_im.rs` exposing:

  ```rust
  pub fn spawn(state: Arc<OskState>, stop: Arc<StopFlag>) -> Option<JoinHandle<()>>
  ```

  Returns `None` when there is no usable Wayland display (X11 session →
  caller just doesn't get auto-show).
- Wire the spawn call into `main.rs` next to the existing controller-thread
  spawn. The thread exits on `stop` / `ctrlc` (already plumbed via `StopFlag`).

**Validation:** `cargo check` + `cargo clippy`; run under a wlroots
compositor and confirm the thread connects and reports the
`zwp_input_method_manager_v2` global present/absent in a log line.

### Phase 1 — input-method-v2 monitor (activate/deactivate only)

- Connect with `Connection::connect_to_env()`; bind
  `zwp_input_method_manager_v2`; `get_input_method` on a fresh seat.
- Handle `activate` / `deactivate` events → map to `OskState.show()` /
  delayed `close()` with the grace period and suppress flag above.
- Run the event queue on this dedicated thread; blocking dispatch is fine
  (no render loop involvement).

**Important detail:** many compositors expose the input-method socket as a
*separate* Wayland display (e.g. `wayland-1` next to `wayland-0`, or
`$WAYLAND_DISPLAY` pointing at the IM socket). Probe both: first try the
ambient `WAYLAND_DISPLAY`, then the common sibling socket names. Log which
one worked — this will be the #1 support question.

**Validation:** on sway/Hyprland: focus a terminal input field → OSK
appears; click desktop → OSK hides. On KDE: same, after enabling virtual
keyboard support in systemsettings if needed.

### Phase 2 — manual-override + debounce logic in the state machine

- Add to `OskState` (or a small sibling struct): `auto_shown: AtomicBool`,
  `suppress_until_deactivate: AtomicBool`, `manual_hold: AtomicBool`.
- Centralize show/hide decisions in one `fn decide(state, event)` so the
  controller mapper (`manager.rs` / `events.rs`) and `wl_im.rs` can't fight
  each other. The current `was_visible` diffing in `osk_main.rs` already
  tolerates external flips — keep it that way.
- Grace-period hide runs on the wl thread with `stop`-aware waits (no
  polling loops in the render thread).

**Validation:** full manual/auto interplay matrix: manual-open then focus
blur → stays open; auto-show then manual-close → stays closed while focused;
rapid focus flapping → single clean show/hide cycle.

### Phase 3 — text injection correctness

- Keep `uinput.rs` as the default injector: kernel-level evdev injection
  works on Wayland (the compositor reads `/dev/input` like any other input
  device) and matches the Windows-app architecture.
- Optional follow-up (only if Unicode/composition issues appear): when the
  IM grab is active, send text via `zwp_input_method_v2.commit_string`
  instead of synthetic keystrokes. This is the "proper IME" path and makes
  emoji/Unicode entry reliable, but it only works while our IM is the active
  input method — hence it can never fully replace uinput.

**Validation:** type into Firefox (GTK, uses text-input-v3), a Qt app, and a
terminal; check shifted keys, Unicode, and backspace behavior.

### Phase 4 — GNOME fallback (stretch)

Options, in preference order:

1. **Do nothing** — GNOME users use Steam + X (honest, zero maintenance).
2. **Tiny GNOME Shell extension** that listens to Mutter's OSK
   (`keyboard.get_state()` / text-input activation in the shell process) and
   forwards show/hide over a private D-Bus signal; `wl_im.rs` (or a
   `gnome_bridge.rs`) subscribes and drives the same `decide()` path.
   - Requires users to install an unsigned extension — document it like the
     Windows lock-screen add-on is documented.
3. **IBus engine** — technically the "real" GNOME path, but it means
  becoming an IBus daemon plugin; way out of scope for this port.

### Phase 5 — polish

- `-osk-follow-cursor`-style positioning stays out of scope; keep the current
  XWayland always-on-top window (the `SDL_VIDEODRIVER=x11` forcing in
  `osk_main.rs` stays — layer-shell would mean dropping SDL3, not worth it).
- README table row: "text field focused → OSK appears automatically (Wayland:
  KDE/wlroots; GNOME: manual or extension)".
- flake: nothing to change — `wayland-client` is pure Rust; dev shell already
  has what's needed.

## Risk register

| Risk | Mitigation |
|---|---|
| IM socket discovery varies per compositor | probe ambient + sibling sockets, log clearly, fail soft to manual-only |
| User runs a non-IM compositor (e.g. plain GNOME) | feature degrades to current manual behavior, never breaks |
| wl thread vs controller thread visibility races | `decide()` is the single writer of hide/show decisions; `OskState` already uses atomics |
| Duplicate keystrokes when both uinput and IM path fire | Phase 3 IM path is opt-in/behind a flag until proven; never both simultaneously |

## Definition of done

- On KWin, sway, Hyprland: focusing any text-input-v3 app auto-shows the OSK;
  blur auto-hides; manual toggles behave per the state machine above.
- On X11 and GNOME (without extension): behavior identical to today.
- `cargo check`, `cargo clippy`, `cargo build --release` clean; manual test
  matrix above walked through on at least one wlroots compositor and KWin.
