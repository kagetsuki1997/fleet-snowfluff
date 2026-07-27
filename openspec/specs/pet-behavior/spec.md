# Spec: pet-behavior

## Purpose

TBD

## Requirements

### Requirement: Motion state machine

The behavior engine SHALL drive each pet through the states wander, follow, curious, and rest, with transitions matching the legacy implementation: follow engages when follow-mouse is enabled and cursor distance exceeds the follow-start threshold; curious engages within the follow-stop threshold; rest engages probabilistically on reaching a wander target and expires after a randomized duration.

#### Scenario: Follow engages at distance

- **WHEN** follow-mouse is enabled and the cursor is farther than the follow-start distance (200 px)
- **THEN** the pet enters the follow state and moves toward the cursor at the follow speed multiplier

#### Scenario: Curious near cursor

- **WHEN** the pet is in follow state and closes within the follow-stop distance (60 px)
- **THEN** the pet enters the curious state and moves at the reduced curious speed

#### Scenario: Rest after reaching target

- **WHEN** a wandering pet arrives within the rest-arrival distance of its target
- **THEN** with the configured rest probability it enters rest for a randomized duration between the rest minimum and maximum, then resumes wandering

### Requirement: Inertia-based movement

Pet motion SHALL use the legacy inertia model — velocity blended from the previous velocity (inertia factor 0.95) and the intent vector toward the target (intent factor 0.05), plus periodic random jitter — updated on a ~30 ms tick.

#### Scenario: Smooth direction change

- **WHEN** the pet's target position changes abruptly
- **THEN** the pet's velocity turns gradually over multiple ticks rather than snapping to the new heading

### Requirement: Edge escape and respawn

When a pet reaches a screen edge, it SHALL either bounce back or, with the configured escape probability, exit the screen and respawn from beyond an opposite edge at the configured respawn margin.

#### Scenario: Respawn outside visible area

- **WHEN** a pet escapes through a screen edge
- **THEN** it reappears positioned outside the opposite edge and wanders back into view

### Requirement: Wander stay modes

The engine SHALL support the three wander stay modes: always moving, probabilistic stopping, and stationary idle, selectable at runtime and persisted in config.

#### Scenario: Stationary mode holds position

- **WHEN** the wander stay mode is set to stationary
- **THEN** the pet plays idle animations in place and does not select new movement targets

### Requirement: Drag interaction

A pet SHALL be draggable with the pointer: drag begins on press (switching to the drag animation and triggering a voice line), the window follows the cursor during drag, and normal behavior resumes on release, snapping to screen bounds when window snap is enabled.

#### Scenario: Drag and release

- **WHEN** the user presses on a pet, moves the cursor, and releases
- **THEN** the pet follows the cursor while pressed, and on release resumes its previous behavior from the drop position

### Requirement: Pause mode

When paused, pets SHALL stop moving, rest in the idle pose for a fixed 10 s, then continuously switch to a different randomly selected special animation at a randomized interval between the configured minimum (30 s) and maximum (120 s) for as long as the pet stays paused, without returning to the idle pose in between (deviates from the legacy Ameath's 30-120s-idle/4-8s-animation cycle by deliberate product decision — see design.md D18).

#### Scenario: Paused pet plays random animation

- **WHEN** a pet has been paused for the fixed 10 s idle delay
- **THEN** it plays one random special animation and schedules the next different one at a new randomized 30-120s interval, repeating for as long as it stays paused

### Requirement: Multi-instance management

The application SHALL run between 1 and 80 pet instances simultaneously, with the count adjustable at runtime; shared settings (pause, follow, click-through, display priority, visibility, voice) SHALL apply to all instances.

#### Scenario: Increase instance count

- **WHEN** the user raises the instance count from 1 to 5
- **THEN** four additional pets appear, each inheriting the current shared settings

#### Scenario: Global toggle reaches all instances

- **WHEN** the user toggles pause while 5 pets are running
- **THEN** all 5 pets pause together
