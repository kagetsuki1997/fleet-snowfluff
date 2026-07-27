# Design: Fleet Snowfluff Rust Rewrite

## Context

Ameath (~4,500 lines Python) is a tkinter desktop pet: `pet.py` mixes a behavior state machine with rendering and drag handling; `window_manager.py` isolates Win32 layering (WorkerW desktop attachment, click-through, topmost); `voice.py` hand-parses WAV and plays via sounddevice; `music_player.py` (removed in this change) is a full MP3 player; settings/tray/quick-menu are tkinter with hard-coded zh-CN strings; updates download from Gitee and self-replace via a generated `.bat`. Transparency uses a pink chroma-key because tkinter cannot do per-pixel alpha — but the GIF assets themselves already carry real 1-bit transparency, so no asset re-export is needed.

The repo already lives at `github.com/kagetsuki1997/fleet-snowfluff` with Rust scaffolding (`rust-toolchain.toml`, nix devshell). Current version v1.1.9; the rewrite restarts at 0.1.0.

## Goals / Non-Goals

**Goals:**

- Feature parity with the Python app minus the music player, on Windows, macOS, and Ubuntu (X11/XWayland)
- Real per-pixel alpha pet rendering; 80-instance cap preserved
- UI in zh-Hant/zh-Hans/en/ja/ko; voice packs switchable zh/ja/en/ko (zh assets only at launch)
- Signed auto-update from GitHub Releases; CI quality gates and tag-triggered multi-platform release builds
- One-shot config migration from the legacy app; full rebrand with proper attribution

**Non-Goals:**

- Native Wayland support (X11/XWayland only on Linux)
- Real OS code signing (Apple Developer ID + notarization, proper Authenticode) and notarization specifically (deferred; caveats documented) — macOS does now sign with a free self-signed identity (D19), but only to fix Accessibility-permission persistence, not to satisfy Gatekeeper/SmartScreen
- Recording ja/en/ko voice assets (UI supports them; assets arrive later)
- New pet behaviors or animations beyond the existing set
- E2E/UI test automation (manual per-platform checklist instead)
- Deleting the Python implementation (`legacy/` is kept permanently as reference)

## Decisions

### D1 — Tauri v2 as the application shell

Tauri provides tray, webview windows for settings, the updater plugin, bundlers for all three targets, and raw window handles for platform layering code. Alternatives: pure winit + egui (hand-rolled settings UI, CJK font juggling, no updater); Electron-class stacks (contradict the lightweight-pet goal).

### D2 — Pet windows are plain native windows (no webview), rendered by Rust

Tauri v2 decouples windows from webviews. Each pet is a `tauri::window::Window` with pre-decoded GIF frames (`image`/`gif` crates) blitted on the same ~30 ms tick that runs physics — no IPC on the hot path. This keeps per-pet cost at a few MB so the 80-instance cap survives. Alternatives considered: webview-per-pet (15–40 MB each → unacceptable at high counts; would force cap ~10), overlay canvas per monitor (scales furthest but replaces per-pet drag/layering with a cursor hit-test dance). macOS and X11-with-compositor blit via wgpu, calling `set_position` on the same tick. **Windows does not** — D17 covers why wgpu's DirectComposition-backed transparent surface (this decision's original plan) was abandoned post-spike for GDI's `UpdateLayeredWindow` instead, which also folds position into the same call rather than a separate `set_position`.

### D3 — Cargo workspace with a pure core crate

```
crates/
  fleet-snowfluff-core/   # state machine, motion physics, config schema +
                          # migration, locale resolution — zero window/Tauri deps
  fleet-snowfluff/        # Tauri app: pet windows, wgpu, platform modules,
                          # tray, native quick menu, audio, updater wiring
ui/                       # settings webview (vanilla TS + Vite)
legacy/                   # the Python app, kept permanently as reference
```

All dependency versions declared once in root `[workspace.dependencies]`. The core crate compiles headless, so `cargo test` runs on free CI runners with no display server, and the compiler enforces the behavior/window seam. Alternative: single crate (rejected — loses both properties for the cost of one `Cargo.toml`).

### D4 — Behavior engine is a 1:1 port

The state machine (wander/follow/curious/rest), inertia model (`INERTIA_FACTOR` 0.95, intent 0.05, jitter 0.15), edge escape/respawn, stop/rest timers, and all tuning constants port verbatim from `legacy/ameath/pet.py` and `constants.py`. The pet's "feel" is the product; `legacy/` stays runnable for side-by-side comparison. Rust owns window movement natively; the sprite framebuffer is the only rendering concern.

### D5 — Platform layering behind one trait, tiered acceptance

`platform/{win,macos,linux}.rs` implement a common trait: topmost, desktop-only, fullscreen-hide probing, click-through.

| Mode            | Windows (strict parity)              | macOS (verified)             | Linux X11 (verified GNOME/KDE)              |
| --------------- | ------------------------------------ | ---------------------------- | ------------------------------------------- |
| Topmost         | SetWindowPos                         | NSWindow level               | `_NET_WM_STATE_ABOVE`                       |
| Fullscreen-hide | foreground-window rect (direct port) | `CGWindowListCopyWindowInfo` | `_NET_WM_STATE_FULLSCREEN` on active window |
| Desktop-only    | WorkerW attach (direct port)         | `kCGDesktopWindowLevel`      | `_NET_WM_WINDOW_TYPE_DESKTOP` hint          |

Exotic Linux WMs are documented best-effort, not release blockers.

### D6 — i18n via plain JSON, single source of truth

`locales/{zh-Hant,zh-Hans,en,ja,ko}.json`, flat namespaced keys, `{placeholder}` interpolation only (no concatenation). Rust embeds via `include_str!` for tray/menu strings and serves the active dictionary to webviews through one command. Detection: `sys-locale` → `zh-TW|HK|MO → zh-Hant`, `zh-CN|SG → zh-Hans`, prefix match `en|ja|ko`, fallback `zh-Hant`; `ui_language` config overrides. Alternatives: Fluent / rust-i18n (rejected — ~60 strings, CJK barely uses plurals; JSON is easiest for future translators).

### D7 — Voice: rodio + per-language manifest

`assets/voice/{zh,ja,en,ko}/` each with a `manifest.json` naming its clips. rodio replaces the ~100-line WAV parser; 0–150% volume via `amplify()`. Anti-repeat rule (no clip 3× consecutively) ports as-is; voice triggers on drag start, unchanged. Invariant: a selectable `voice_language` always has assets — the settings picker disables empty languages, and config load snaps invalid values back to `zh`.

### D8 — Settings = vanilla TS webview; quick menu = native; tray = Tauri

Settings is ~15 controls in 3 tabs (personalization, update, about) mapping 1:1 to Tauri `invoke()` — no frontend framework. The 628-line custom tkinter quick menu becomes a native context menu (~80 lines; OS handles focus/dismiss/DPI). About tab carries credits and the Kuro Games asset disclaimer. zpix font with CSS fallback chain to system Noto Sans JP/KR pending a Hangul/kana coverage check.

### D9 — App identity, config, migration

Identifier `fleet-snowfluff`, product "Fleet Snowfluff", binary `fleet-snowfluff`. Config is a serde struct at Tauri `app_config_dir/config.json` (validation = types + range clamps mirroring `_sanitize_config`). First run without a new config: read `%APPDATA%/ameath_config.json` if present, carry all keys except `music_*`, add `ui_language` (detected) and `voice_language` (`"zh"`), remove the old `DesktopPet` autostart registry value, register the new autostart (`tauri-plugin-autostart`), write the new file; the old file is left untouched. Migration logic lives in the core crate and is unit-tested.

### D10 — Updater: GitHub Releases + tauri-plugin-updater

GitHub Releases hosts artifacts + `latest.json`; the plugin verifies a minisign signature on every package (keypair generated once; private key in Actions secrets, public key in `tauri.conf.json`). Skip-this-version / skip-all-updates gate the startup check exactly as today. Replaces the Gitee + `.bat` flow, which cannot work cross-platform.

Implemented (14.1-14.3): keypair generated via `cargo tauri signer generate`; public key is embedded in `crates/fleet-snowfluff/tauri.conf.json`'s `plugins.updater.pubkey`. The private key was written straight to a location outside the repo and was never committed. Before task 16's release workflow can sign anything, it needs to exist as the `TAURI_SIGNING_PRIVATE_KEY` GitHub Actions secret (repo Settings → Secrets and variables → Actions) — that's a one-time manual step for whoever holds the key, not something automated here. The key has no password (`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` unset), matching the plugin's non-interactive-CI-friendly default; if the key is ever regenerated, the new public half must be re-embedded in `tauri.conf.json` and the old signed releases become unverifiable (expected — that's what rotation means).

All update logic goes through app-defined commands (`commands.rs`: `check_for_update`, `install_update`, `set_skip_version`) calling the plugin's Rust API directly, rather than exposing the plugin's own JS-invokable commands to the webview — keeps the settings window's command surface uniform with every other tab, and needs no capability-file entry (custom commands bypass the ACL; only plugin-exposed JS commands are ACL-gated).

### D11 — CI and release pipeline

- **Quality workflow** (PRs + pushes to `develop`/`main`, free hosted runners): `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` (core tests run headless).
- **Release workflow** (`v*` tag): matrix builds — NSIS `.exe` (Windows), `.deb` + AppImage (Linux), universal-binary `.dmg` + `.app.tar.gz` (single Apple-Silicon runner, `universal-apple-darwin`) — uploads all artifacts plus `latest.json` to a **draft** GitHub Release; publishing is manual after the smoke checklist (publishing is the moment live updaters see it).
- No rc tags; first tag is `v0.1.0` when the parity checklist passes on all platforms.

### D12 — Testing bar

Core crate unit tests: state-machine transition edges and timers, motion invariants (inertia convergence, respawn bounds), config sanitization + migration, locale resolution chain, voice-manifest validation. Shell verification: a versioned manual smoke checklist (markdown in-repo) executed per platform before publishing. No E2E automation (transparent native windows across 3 OSes on GPU-less runners = high cost, flaky signal).

### D13 — Attribution and branding

LICENSE keeps `Copyright (c) 2026 sinlatansen`, adds the new holder line. About tab + README credit the original Ameath project and `-fugu-` (bilibili link) for the app and fan assets, note the Rust rewrite authorship, and state: all GIF/voice asset copyrights belong to Wuthering Waves / Kuro Games; assets removed promptly on request. (Disclaimer documents good faith; it is not a license.)

### D14 — Transparency spike findings (tasks 3.1-3.4)

`crates/fleet-snowfluff/examples/transparent_gif.rs` decodes a real asset GIF and renders it via wgpu into a genuinely windowless (`tauri::window::WindowBuilder`, not `WebviewWindowBuilder`) transparent window. **Verified visually on macOS (Metal)**: screenshots during the run show the sprite compositing directly over arbitrary desktop/terminal content with no backing box of any color — confirmed pixel-for-pixel that no chroma-key is involved, and the wgpu surface negotiated `CompositeAlphaMode::PostMultiplied` (real per-pixel alpha) rather than falling back to `Opaque`. **The Windows/DirectComposition path was tried on real hardware and confirmed unworkable**: `wgpu`'s alpha-mode negotiation (`pick_alpha_mode`) reported `Opaque`-only under both the DX12 and Vulkan backends — the documented signal (task 3.5) to fall back to a manual blit path instead of a wgpu swapchain on Windows, which is what happened; see D17 for the GDI/`UpdateLayeredWindow` path that replaced it and its own real-hardware verification history.

Findings that affect the production implementation (task 6.1), not just the spike:

- **macOS transparency requires two extra opt-ins**, or `.transparent()` doesn't even compile: the `tauri` crate's `macos-private-api` Cargo feature, and `"macOSPrivateApi": true` under `app` in `tauri.conf.json` (Tauri cross-checks the two and errors at build time if they disagree). Both are now set at the workspace level.
- **A truly windowless window requires the `unstable` Cargo feature** — `tauri::window::WindowBuilder` (which implements `raw_window_handle`'s `HasWindowHandle`/`HasDisplayHandle` directly) is gated behind it; `WebviewWindowBuilder` does not need it but always carries a webview, which D2 rules out for pet windows.
- **`tauri.conf.json`'s `app.windows: []` (no default window) works correctly** and does not stop programmatically-created windows from appearing — confirmed by isolating it from an unrelated bug (below). This is the config the production app needs anyway, since neither the settings window nor any pet window should auto-open before app logic decides to create them.
- **The animation/render loop can't rely on `RunEvent::MainEventsCleared` alone**: Tauri's underlying event loop defaults to `ControlFlow::Wait` and has no public API to change that, so `MainEventsCleared` only fires on real OS events, not continuously. The working pattern — and the one the production ~30ms pet tick (D4) should reuse — is a background `std::thread` that sleeps on the desired cadence and calls `AppHandle::run_on_main_thread(...)` to schedule each frame's work onto the main thread.
- **`WindowBuilder::center()` produced an invisible/off-screen window** in this environment; an explicit `.position(x, y)` is the verified-working alternative. Root cause unconfirmed (plausibly monitor geometry not yet resolved for a windowless window at build time) — worth a focused re-test when task 6.1 implements real multi-monitor placement, since pets do need reliable, monitor-aware positioning.

### D15 — Window-snap docks to the current foreground window, not a named allowlist

Legacy matched a hardcoded set of app names/classes (Notepad, WeChat, a Wuthering Waves title fragment) to decide whether to dock. This rewrite drops the allowlist: docking is decided purely by whether the current foreground window is eligible (normal state, not desktop/shell, not the pet's own window) — any application qualifies. Split across the core/shell boundary (D3): the shell's per-platform foreground-window query (task 7/8/9's existing fullscreen-hide detection already fetches the active window; docking reuses that same primitive to also fetch its rect) hands a plain rect to a pure, testable core function that computes the dock position (top-right corner offset, matching legacy) or `None` if undocking is warranted. No platform code is needed inside `fleet-snowfluff-core`.

### D16 — Voice playback: rodio on its own thread

Same shape as the render loop: manifest loading and clip-selection state live on the main thread inside `PetManager` (task 10.1/10.2), but the actual `rodio::Player`/output device handle run on a dedicated background thread, communicated with via an `mpsc::Sender<Command>`. This sidesteps rodio's `OutputStream`/device-handle types not being reliably `Send`, which matters because `PetManager` lives inside a `Mutex` Tauri manages across threads. `ClipSelector` (core, D4-style pure logic) picks the next clip avoiding a 4th consecutive repeat; `VoicePlayer` (shell) resolves manifests once at startup via `resolve_voice_language`, so an invalid/empty configured language snaps to `zh` before any playback is attempted.

### D17 — Windows pet rendering: GDI's `UpdateLayeredWindow`, not wgpu

D14 confirmed on real Windows hardware that `wgpu` negotiates `Opaque`-only alpha compositing under both DX12 and Vulkan — no per-pixel-alpha swapchain path exists on Windows the way D2 assumed. `platform/windows.rs`'s `LayeredSurface` replaces it with the classic pre-DWM-composition API: a `WS_EX_LAYERED` window painted via `UpdateLayeredWindow` from a `CreateDIBSection` memory DC, premultiplied-alpha `BLENDFUNCTION`. This is what legacy Ameath was functionally reaching for with tkinter's `-transparentcolor` (also `UpdateLayeredWindow`-based under the hood), except real per-pixel alpha from the GIFs' own alpha channel instead of a binary chroma-key. `gfx.rs` (the wgpu module) is `#[cfg(not(target_os = "windows"))]`-gated; Windows pet windows never reference it.

A `Gpu` type alias (`pub type Gpu = GpuContext` non-Windows, `pub type Gpu = ()` Windows) keeps shared call sites (`PetWindow::tick`/`render`/`apply_window_size`/`set_scale`/`set_opacity`, `PetManager::tick`) at one identical signature across platforms; only the method **bodies** branch by `#[cfg(...)]`.

Real-hardware debugging (post-switch) found and fixed, in order: a DPI-oscillation bug from mixing a stale `GetDpiForWindow` value for size with a fresh per-frame monitor lookup for position (unified into one `resolve_monitor_scale_and_position` call feeding both); a real GDI bug where a bitmap was `DeleteObject`ed while still selected into its DC; and a persistent box/content size mismatch that survived every DPI/monitor-math fix (all independently confirmed correct against real log data) before the actual mechanism was found: the sprite is drawn at its true size pinned to the DIB's top-left corner, with the DIB/window itself sized `MARGIN_FACTOR` (3.0×, empirically necessary — 1.1× visibly wasn't enough) larger than the content and the extra area left fully transparent. The exact Windows-side compositing behavior that needs this much slack was never identified despite ruling out DPI oscillation, DWM's resize-transition animation, and per-thread DPI-awareness resets in turn; the fix is empirical, not fully explained. Net result: memory dropped from the wgpu-era 500-1000MB to ~37-45MB per instance — the entire reason UpdateLayeredWindow was worth pursuing over wgpu in the first place, alongside actually having per-pixel alpha.

### D18 — Window-snap pause animation: fixed idle delay, then continuous different-random cycling

Changed from D4's legacy-matching model (rest 30-120s, play one random special animation for 4-8s, repeat) to a deliberate product decision, not a porting bug: rest a fixed 10s after pausing, then keep switching to a _different_ random screen-reaction gif every 30-120s for as long as the pet stays paused, never returning to the idle pose in between. `fleet-snowfluff-core/src/pause.rs`'s `PauseAnimationScheduler` simplified accordingly — the `Phase`/`ReturnToIdle` state it used to need is gone, since post-idle-delay behavior is now uniform (always fire `PlayRandomAnimation` on a 30-120s timer). `pet-behavior/spec.md`'s "Pause mode" requirement updated to match.

### D19 — macOS code signing: self-signed, reused everywhere, not deferred to "unsigned"

The original Non-Goals scoped "OS code signing / notarization" out entirely as deferred. In practice, zero signing (Tauri's ad-hoc default with no `signingIdentity` configured) turned out to actively break a real feature: macOS's TCC ties an Accessibility-permission grant (needed for `device_query`'s global mouse polling — drag, follow-mouse, right-click) to the exact signing identity, and ad-hoc signatures are derived from the binary's own content hash, which changes on essentially every rebuild. That made the grant impossible to keep past a single rebuild, locally or across release updates.

A free self-signed certificate (`scripts/setup-macos-signing.sh`, Common Name `Fleet Snowfluff Self-Signed`, referenced by `tauri.conf.json`'s `bundle.macOS.signingIdentity`), reused for _every_ build — local dev and CI release alike — gives the app a stable identity, so the grant survives rebuilds and app updates. This does **not** satisfy Gatekeeper for other users downloading a release (self-signed certs aren't in Apple's trust chain — same one-time "unidentified developer" click-through as before, not worse); real Apple Developer ID + notarization remains deferred exactly as the original Non-Goals said, now scoped specifically to the Gatekeeper problem rather than signing in general.

**CI wiring is deliberately not `tauri-action`'s built-in `APPLE_CERTIFICATE`/`APPLE_CERTIFICATE_PASSWORD` env vars.** When those are set, `tauri-bundler` (`tauri-macos-sign`'s `Keychain::with_certificate`) always tries to auto-discover the signing identity from the imported cert by matching Apple's own official certificate-name prefixes ("Developer ID Application:", "Apple Development:", etc.) — a self-signed cert named "Fleet Snowfluff Self-Signed" can never match any of those, regardless of trust settings, and the build fails with "failed to resolve signing identity" before the configured `signingIdentity` is ever consulted. `release.yaml`'s macOS leg instead imports the cert into a keychain itself (a separate step, `security create-keychain`/`import`/`set-key-partition-list`/`list-keychain`) and leaves those two env vars unset for the actual `tauri-action` build step, so `tauri-bundler` takes its other path (`Keychain::with_signing_identity`) that just trusts `tauri.conf.json`'s configured identity directly, no discovery involved. Confirmed working via a real tag-triggered release run.

Turning on real signing also surfaced an unrelated, previously-latent bug: the app crashed at launch ("Library not loaded") because it linked `libiconv.2.dylib` by an absolute `/nix/store/...` path — this repo's `.envrc` uses direnv's `use flake`, which injects the Nix devshell into every command run in the directory, including a plain `cargo build`. That was always non-portable, but harmless under ad-hoc signing (no Library Validation enforcement); a real identity makes macOS enforce Team-ID matching on loaded libraries, which Nix's copy fails. Fixed by `scripts/fix-macos-dylib-paths.sh`, wired in as `tauri.conf.json`'s `beforeBundleCommand`, repointing the load command at `/usr/lib/libiconv.2.dylib` (macOS has shipped an ABI-compatible one as part of the OS forever) before the binary is bundled and signed. The `beforeBundleCommand` wrapper itself also needed a Windows fix: it checks `uname -s = Darwin` and exits immediately otherwise, since the `git rev-parse --show-toplevel`/`cd` dance it used to reach the script broke on the Windows runner's bash/path handling before the script's own no-op guard ever ran.

### D20 — Linux pet rendering: keep wgpu, just prefer Vulkan over GL

Unlike Windows (D17), Linux X11 didn't need abandoning wgpu — it needed wgpu to stop silently choosing a backend that can't do alpha. Diagnosed against a NixOS-in-VirtualBox guest: pet windows rendered fully opaque, and the log showed wgpu had picked the GLES/EGL backend. VirtualBox's Guest Additions only pass real GPU acceleration through as OpenGL, never Vulkan (`vulkaninfo` on that guest only ever finds Mesa's Lavapipe, the CPU/software Vulkan implementation) — and wgpu's default `Backends::all()` enumerates every available backend and picks one without any awareness that, on X11 specifically, the GLES/EGL surface only ever advertises the `Opaque` composite alpha mode, the same fundamental limitation D17 hit on Windows' non-Vulkan backends. The difference here: Vulkan itself — including Lavapipe, pure software, no real GPU involved — correctly advertises `PreMultiplied`/`PostMultiplied` alpha on X11. So the fix is just making sure wgpu picks Vulkan when it's available at all, not replacing the renderer.

`manager.rs`'s `select_wgpu_backends()` runs a cheap adapter-enumeration probe restricted to `Backends::VULKAN` before constructing the real `wgpu::Instance`; if that probe finds anything (a real GPU's Vulkan driver, or just Lavapipe), the instance is constructed with `Backends::VULKAN` only, otherwise it falls back to `Backends::all()` (rendering opaque, same as before, rather than failing outright on genuinely Vulkan-less systems). `WGPU_BACKEND` still overrides both when set, matching wgpu's own documented behavior. Confirmed fixed on the NixOS-in-VirtualBox guest; not yet confirmed on real (non-virtualized) Linux hardware, where a real GPU's own Vulkan driver should if anything make this more reliable, not less — VirtualBox's GL-only passthrough was the whole reason GL got picked over Vulkan by wgpu's own default heuristics in the first place.

Also silenced, same investigation: Mesa's lavapipe/llvmpipe X11 WSI logs a "suboptimal present" warning on essentially every frame (a known Mesa software-rasterizer quirk, not an actual problem for a tiny ~200×200 pet sprite) — one `.level_for("wgpu_hal::vulkan", log::LevelFilter::Error)` call on the log plugin builder in `lib.rs` silences it without touching the workspace-wide `Info` level.

### D21 — Linux global mouse polling: a dedicated `x11rb` connection, not `device_query`

`device_query`'s Linux/X11 backend wraps `Rc<Display>` internally, which isn't `Send` — since `PetManager` lives inside a `Mutex` Tauri manages across threads, storing a `device_query::DeviceState` there at all is a compile error, not just a missing permission (unlike the macOS Accessibility-permission case, D19). This is why `mouse_available()` hardcoded `false` on Linux unconditionally from task 18.2 onward, silently disabling drag, follow-mouse, and the right-click quick menu there — not a missing feature so much as a `Send` bound `device_query` itself can't satisfy on this one platform.

`platform::linux::MousePoller` sidesteps this with its own dedicated `x11rb::RustConnection` — `x11rb` is a pure-Rust XCB client built to be thread-safe, so a single long-lived connection can live inside `PetManager` directly. `MousePoller::poll()` calls `XQueryPointer` on the root window for cursor position (root-relative physical pixels, same semantics as `GetCursorPos` on Windows — same `physical_to_logical_cursor` conversion now applies to both) and button-1/button-3 state, and `PetManager::tick`'s existing drag/follow-mouse/quick-menu logic (already platform-generic, driven by whatever `(cursor, left_down, right_down)` the platform branch produces) needed no changes at all beyond wiring in the new source. This is the same X11 connection _pattern_ `platform::linux`'s EWMH client (D5/task 9) already established — a separate, independent connection rather than trying to share GTK's own — just used for a different purpose (polling, not property queries).

Confirmed working (drag, follow-mouse, and the quick menu all functional) on the same NixOS-in-VirtualBox KDE Plasma X11 session D20 was diagnosed against; real-hardware and other-window-manager verification remains open (task 9.4).

### D22 — Nix flake packaging as a NixOS-native install path, not a `release.yaml` replacement

`.deb` and AppImage (`release.yaml`'s Linux artifacts) both assume a standard FHS layout, which NixOS deliberately doesn't have — AppImage in particular needs extra shims (`nix-alien`, `appimage-run`) to run on NixOS at all. `flake.nix` gained a `packages.default` output (`devshell/package.nix`, built via crane; the frontend prebuilt separately by `devshell/ui.nix` since the actual Cargo build runs inside Nix's network-sandboxed build environment, where `npm run build` isn't possible) as a NixOS/Nix-native alternative — `nix build`/`nix run`, or consumable as a flake input by another NixOS/home-manager config (`packages.<system>.default`). This is a complement, not a replacement: it only runs on machines that have Nix itself (resolving the exact `/nix/store` paths it's linked against), so the `.deb`/AppImage remain what everyone else on Linux needs, and it isn't wired into `release.yaml`'s automated builds — same "local dev-convenience tooling, not part of the original spec phases" category as task group 18's Windows/Linux cross-build recipes.

Building this against a real NixOS-in-VirtualBox guest is what surfaced D20 and D21 in the first place, plus one more issue: the settings webview (the app's only webview) rendered fully blank in that same VM session — a separate, well-documented WebKitGTK-on-NixOS symptom where WebKitGTK's own DMABUF/GPU-accelerated compositing negotiation fails silently instead of falling back to software rendering. `devshell/package.nix` sets `WEBKIT_DISABLE_COMPOSITING_MODE=1` in the wrapper as the standard workaround (a settings form has no real need for hardware compositing) — confirmed fixed on the same session, same as D20/D21.

`packages.default` is Linux-only in practice — built via GTK-specific tooling (`wrapGAppsHook3`, `webkitgtk_4_1`) — even though `flake-utils.eachDefaultSystem` technically evaluates it for Darwin systems too. Making that a real, working macOS path would need: (a) a Darwin-specific `buildInputs`/framework list (straightforward, `devshell/default.nix`'s existing `apple-sdk` usage is most of the way there), and (b) accepting that `nix build` can't produce a properly _signed_ `.app` at all — `codesign` needs real keychain access, and Nix's sandboxed build model (no network, a separate unprivileged build user, no access to the invoking user's login keychain) is structurally at odds with that, the same way it's at odds with anything needing interactive/CI-secret-gated credentials. The realistic version of a macOS Nix package would stop at an unsigned raw binary (`cargo tauri build --no-bundle`, mirroring how `just build-windows`'s cross-compiled output is also unsigned/unbundled) and leave signing to `just build`/`just macos-signing` outside Nix, same as today. Not pursued: the motivating problem (FHS incompatibility) is Linux/NixOS-specific and doesn't exist on macOS at all — any Mac can already run the normal `.dmg`, no Nix required — so the only real audience would be nix-darwin/home-manager users wanting `home.packages` consistency, a nice-to-have rather than a gap.

## Risks / Trade-offs

- [Windows GDI rendering (D17) needed extensive empirical, not fully-explained tuning] → `MARGIN_FACTOR = 3.0` is confirmed necessary and cheap (~37-45MB measured full-app memory with it) but the exact Windows-side mechanism forcing that much slack was never identified; revisit if a future macOS/DPI/scale combination reintroduces visible cropping
- [Windows right-click quick menu doesn't open on real hardware (task 19.3)] → click detection confirmed working via the same hit-test path as drag; diagnostic logging is in place at every step of the popup path for the next real-hardware test rather than further blind guessing
- [80 animated wgpu windows may stress weak GPUs] → frames are tiny (≈200×200); throttle blits to GIF frame-delay cadence rather than tick rate; measure at the spike
- [Wayland-native sessions break follow-mouse/positioning] → run under XWayland; document; out of scope otherwise
- [Linux desktop-only/fullscreen-hide varies by WM] → tiered acceptance (D5); GNOME/KDE verified, others best-effort
- [Linux pet transparency and mouse polling (D20/D21) confirmed only inside a NixOS-in-VirtualBox VM, not real hardware] → the specific bug (wgpu picking GL over Vulkan) was caused by VirtualBox's GL-only guest passthrough, so real hardware with a real GPU's own Vulkan driver should if anything be more reliable, not less; still worth a real-hardware pass before trusting this fully (task 9.4)
- [Systems with no Vulkan ICD at all (not even software Lavapipe) still render pet windows fully opaque] → `select_wgpu_backends()` (D20) falls back to `Backends::all()` rather than failing outright; accepted as a graceful degradation, not pursued further since Lavapipe ships in any reasonably current Mesa
- [zpix may lack Hangul/kana glyphs] → verify early; CSS fallback chain regardless; tray/native menus use OS font stacks anyway
- [Unsigned binaries trigger SmartScreen/Gatekeeper] → README caveats; signing deferred deliberately
- [Migration runs once on real user machines] → logic in core crate with exhaustive unit tests over legacy config corpus (defaults, partial, corrupt, out-of-range)
- [Shipping Kuro Games-derived assets] → prominent disclaimer + removal commitment (D13); accepted as standard fan-project posture

## Migration Plan

1. Move Python app to `legacy/` (kept permanently); delete `sound/music/`; retarget `justfile`
2. Scaffold workspace + CI quality workflow first (gates every subsequent PR)
3. Spike: one native window + wgpu transparent blit on Windows (highest-risk item)
4. Port core engine with tests → pet windows → platform layering per tier → voice/i18n/settings/tray → updater → release workflow
5. User-facing migration is automatic on first launch (D9); rollback = keep running the legacy Python app, whose config file is never modified

## Open Questions

- zpix Hangul/kana coverage (verify during settings-UI work; fallback chain already designed)
- Exact wgpu surface configuration for transparency: resolved on macOS (D14). Windows doesn't use wgpu at all (D17) — moot for Windows now, not "still needs verification"
- Whether `.ico`/asset filenames get renamed from `ameath.*` during the asset reorg (cosmetic; decide at implementation)
- Root cause of `WindowBuilder::center()` producing an invisible window (D14) — worked around with explicit `.position()`, not fully explained
- Exact Windows-side mechanism requiring `MARGIN_FACTOR = 3.0` in the GDI renderer (D17) — empirically necessary and cheap, but not explained; four specific theories (DPI oscillation, DWM transition animation, thread DPI-awareness resets, position-vs-monitor-origin math) were each individually ruled out via real hardware logs
- Why the right-click quick menu doesn't open on Windows (task 19.3) — click detection works, `menu.popup()`'s result isn't yet known from a real-hardware run with the new diagnostic logging in place
- Why the `cpal`/`_AudioHardwareDestroyProcessTap` link failure (D16's devshell SDK note) recurred intermittently on the same dev machine after being marked resolved — not investigated
- Whether D20's Vulkan-preference fix and D21's mouse polling behave the same on real (non-virtualized) Linux hardware and other window managers as they did on the one NixOS-in-VirtualBox KDE Plasma X11 session they were diagnosed and confirmed against (task 9.4)
