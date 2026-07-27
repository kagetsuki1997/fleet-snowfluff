# Spec: desktop-integration

## ADDED Requirements

### Requirement: Display priority modes

The application SHALL support three display priority modes applied to all pet windows: (1) always-on-top; (2) normal stacking with automatic hide while a fullscreen application is in the foreground; (3) desktop-only, layered behind all normal windows. Acceptance is tiered: strict parity on Windows (WorkerW attachment for desktop-only, foreground-rect fullscreen detection); verified behavior on macOS (desktop window level, CGWindowList fullscreen detection); verified on GNOME and KDE X11 sessions via EWMH hints, best-effort documented elsewhere.

#### Scenario: Fullscreen hide in mode 2

- **WHEN** display priority is mode 2 and a fullscreen application becomes foreground
- **THEN** pets hide, and reappear when the fullscreen application is no longer foreground

#### Scenario: Desktop-only stays behind windows

- **WHEN** display priority is mode 3 and the user opens a normal application window over a pet
- **THEN** the pet renders behind that window, remaining visible only on the desktop layer

### Requirement: Click-through

The application SHALL support a global click-through toggle: when enabled, pet windows ignore all pointer events (drag becomes unavailable); the state applies to all instances, persists in config, and is toggleable from tray and quick menu.

#### Scenario: Clicks pass through pets

- **WHEN** click-through is enabled and the user clicks on a pet
- **THEN** the click reaches the window or desktop beneath the pet

### Requirement: Multi-monitor placement

The application SHALL support roaming across all monitors combined, or confinement to one selected monitor, per the legacy total-screen and screen-index settings.

#### Scenario: Confined to selected monitor

- **WHEN** the user disables all-screens mode and selects monitor 2
- **THEN** pets wander, target, and respawn only within monitor 2's bounds

### Requirement: Foreground window docking (pause mode)

When paused and window-snap is enabled, the pet SHALL dock against the current foreground window if that window is in normal (not minimized, maximized, or fullscreen) state and is not the desktop/shell itself or the pet's own window; docking position matches legacy (pet anchored to the target window's top-right corner). Any application's window is eligible — there is no per-application allowlist. If no eligible foreground window exists, or the foreground window changes to an ineligible one, the pet SHALL return to its pre-dock position.

#### Scenario: Docks to whichever window is focused

- **WHEN** the pet is paused, window-snap is enabled, and the user focuses a normal window belonging to any application
- **THEN** the pet moves to and holds at that window's top-right corner

#### Scenario: Undocks when focus moves to an ineligible window

- **WHEN** a docked pet's target window is minimized, or focus moves to the desktop
- **THEN** the pet returns to its position from before docking

### Requirement: System tray

The application SHALL provide a system tray icon with localized menu items: show/hide, pause/resume, follow-mouse (checked), click-through (checked), settings, and quit; labels and check states SHALL reflect current state. Follow-mouse and click-through have no settings-window control of their own — tray (and the quick menu) is the only place either is ever changed — so both SHALL persist to config on toggle, the same as every settings-window control.

#### Scenario: Tray toggles visibility

- **WHEN** the user clicks the tray hide item while pets are visible
- **THEN** all pets hide and the item label changes to show

#### Scenario: Follow-mouse and click-through survive a restart

- **WHEN** the user toggles follow-mouse or click-through from the tray or quick menu
- **THEN** the new value is written to config immediately, and the app launches with that same value next time

### Requirement: Quick context menu

Right-clicking a pet SHALL open a native OS context menu mirroring the tray items (follow, pause, click-through, hide, settings, quit) with localized labels and check marks; the menu SHALL dismiss on outside click per OS convention.

#### Scenario: Right-click opens menu

- **WHEN** the user right-clicks a pet (click-through disabled)
- **THEN** a native context menu appears at the cursor with the current toggle states
