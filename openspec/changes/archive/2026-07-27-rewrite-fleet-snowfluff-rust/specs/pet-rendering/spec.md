# Spec: pet-rendering

## ADDED Requirements

### Requirement: Native alpha-transparent pet windows

Each pet SHALL be a borderless, taskbar-hidden, plain native window (no webview) whose non-sprite area is fully transparent using real per-pixel alpha on all supported platforms. No chroma-key color SHALL be used.

#### Scenario: Transparent background over any wallpaper

- **WHEN** a pet is displayed over any desktop background
- **THEN** only the sprite pixels are visible, with no colored halo or solid backing rectangle

### Requirement: GIF frame animation

The renderer SHALL decode each animation GIF once at load into RGBA frames (honoring the GIF's transparency index and per-frame delays) and blit the current frame on the animation cadence; animation selection (move, idle variants, drag, special) follows the behavior state.

#### Scenario: Animation follows state change

- **WHEN** a pet transitions from idle to moving
- **THEN** the window displays the move animation's frames at that GIF's own frame delays

### Requirement: Directional flip

The renderer SHALL horizontally mirror the sprite when the pet's horizontal velocity direction reverses, matching the legacy flip behavior.

#### Scenario: Pet turns around

- **WHEN** the pet's movement direction changes from rightward to leftward
- **THEN** the sprite faces left

### Requirement: Scale steps

The renderer SHALL support the 20 scale steps (0.1× through 2.0×), applied to sprite and window size at runtime and persisted in config.

#### Scenario: Scale change applies immediately

- **WHEN** the user selects a different scale step in settings
- **THEN** all pet windows resize to the new scale without restart

### Requirement: Opacity steps

The renderer SHALL support the 10 whole-window opacity steps (10% through 100%), applied at runtime and persisted in config.

#### Scenario: Opacity change applies immediately

- **WHEN** the user selects 50% opacity
- **THEN** all pet windows render at half opacity without restart
