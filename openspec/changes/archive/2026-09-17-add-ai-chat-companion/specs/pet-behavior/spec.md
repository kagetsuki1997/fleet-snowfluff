## MODIFIED Requirements

### Requirement: Pause mode

When paused, pets SHALL stop moving, rest in the idle pose for a fixed 10 s, then continuously switch to a different randomly selected special animation at a randomized interval between the configured minimum (30 s) and maximum (120 s) for as long as the pet stays paused, without returning to the idle pose in between (deviates from the legacy Ameath's 30-120s-idle/4-8s-animation cycle by deliberate product decision — see design.md D18). Manual drag interaction SHALL remain available while a pet is paused and SHALL take precedence over the paused animation cycle for that pet while the drag is active.

#### Scenario: Paused pet plays random animation

- **WHEN** a pet has been paused for the fixed 10 s idle delay
- **THEN** it plays one random special animation and schedules the next different one at a new randomized 30-120s interval, repeating for as long as it stays paused

#### Scenario: Dragging a paused pet

- **WHEN** the user presses and drags a pet that is currently paused
- **THEN** the pet follows the cursor exactly as an unpaused pet would, and resumes the paused animation cycle from its idle pose once released
