# Developing Fleet Snowfluff

This document covers local setup, everyday commands, and the details of
building for each platform. For what the app _is_, see [`README.md`](README.md).

## Prerequisites

- **Rust** — via [`rust-toolchain.toml`](rust-toolchain.toml) (rustup will pick
  it up automatically), or through the Nix devshell below.
- **Node.js 22+** — for the settings-window frontend (`ui/`).
- **[Nix](https://nixos.org/download)** (optional but recommended) — provides
  a devshell with the exact Rust toolchain, formatters, linters, and (on
  Linux) Tauri's system libraries already wired up. Run `nix develop`, or let
  [`direnv`](https://direnv.net/) load it automatically via [`.envrc`](.envrc).
- **Docker** — only needed for `just build-linux` (see below).

Without Nix, install Node and a matching Rust toolchain yourself and the
`just` recipes that don't shell out to `nix develop` (`dev`, `build`, `test`,
`lint`) work the same way.

## Repository layout

```
crates/
  fleet-snowfluff-core/   # pure behavior engine: state machine, motion
                          # physics, config schema + migration, locale
                          # resolution -- zero window/Tauri dependencies,
                          # compiles and tests headless on any platform
  fleet-snowfluff/        # the Tauri app: pet windows, rendering,
                          # platform-specific layering (platform/), tray,
                          # native quick menu, audio, updater wiring
ui/                       # settings window frontend (vanilla TS + Vite)
assets/                   # GIFs, voice clips, fonts -- embedded into the
                          # binary at build time (rust-embed)
locales/                  # one flat JSON dictionary per UI language,
                          # shared source of truth for Rust and the webview
legacy/                   # the original Python/tkinter app, kept
                          # permanently as a behavior reference
scripts/                  # maintenance scripts (see inline docs in each)
docker/                   # the Linux build container definition
devshell/                 # Nix devshell package list and formatter config
openspec/                 # active change proposals, design docs, specs
docs/                     # the manual per-platform smoke-test checklist
```

## Everyday commands

All of these are `just` recipes ([`justfile`](justfile)); run `just --list`
for the short version.

| Command              | What it does                                                          |
| -------------------- | --------------------------------------------------------------------- |
| `just dev`           | Run the app in dev mode (hot-reloads the settings window's frontend)  |
| `just build`         | Produce a release build for the host platform                         |
| `just test`          | Run the whole workspace's test suite                                  |
| `just lint`          | Run `cargo clippy` with warnings denied, matching CI                  |
| `just fmt`           | Run `treefmt` (Rust, TS, Nix, shell, TOML, etc.) across the repo      |
| `just build-windows` | Cross-compile a portable Windows `.exe` from macOS/Linux (see below)  |
| `just build-linux`   | Build for Linux inside a real container (see below)                   |
| `just macos-signing` | (Re)generate/install/sync the self-signed macOS code-signing identity |
| `just legacy-dev`    | Run the original Python app (`legacy/`) for behavior comparison       |
| `just legacy-build`  | Build the original Python app's PyInstaller executable                |

**Always use `just dev`/`just build`** (or `cargo tauri dev`/`cargo tauri
build` directly) rather than a plain `cargo run`/`cargo build`. The settings
window is the app's only webview, and a raw `cargo` invocation doesn't enable
the Tauri CLI's `custom-protocol` feature — it tries to load the Vite dev
server URL instead of the bundled frontend even when nothing is serving it,
leaving that window silently blank.

## Cross-platform builds

Fleet Snowfluff ships on Windows, macOS, and Linux (X11), each with a
different rendering backend and platform-layering implementation (see
`crates/fleet-snowfluff/src/platform/`). Below is how to build for each one
from a single macOS/Linux dev machine.

### Windows

`just build-windows` cross-compiles a portable `.exe` via
[`cargo-xwin`](https://github.com/rust-cross/cargo-xwin) (downloads the MSVC
headers/libs and links with `lld`), through `cargo tauri build --target
x86_64-pc-windows-msvc --runner cargo-xwin --no-bundle`. GIFs and voice clips
are embedded into the binary itself, so the resulting
`target/x86_64-pc-windows-msvc/release/fleet-snowfluff.exe` is the entire
artifact — copy that one file to a Windows machine and run it, no installer
needed for local testing.

Pet-window rendering on Windows does **not** use wgpu at all (it reported
`Opaque`-only alpha compositing under both DX12 and Vulkan on real hardware);
it goes through classic GDI (`UpdateLayeredWindow` painting a `CreateDIBSection`
memory DC) instead. See `platform/windows.rs`'s module doc for the full story.

### macOS

`just build` on macOS produces a native `.app` directly. Two things worth
knowing:

- **Code signing.** Without a paid Apple Developer ID, every build defaults
  to an ad-hoc signature that changes on every rebuild — which breaks
  Accessibility permission (needed for drag/follow-mouse/the right-click quick
  menu) persisting across rebuilds, since macOS's TCC ties that grant to the
  exact signing identity. `just macos-signing generate` creates a free
  self-signed certificate reused for every build (local and CI), fixing that;
  it does **not** satisfy Gatekeeper for other users downloading a release —
  that still needs a real Apple Developer ID + notarization. See
  `scripts/setup-macos-signing.sh`'s header comment for the full rundown and
  exact commands.
- **Nix-linked libraries.** If you use direnv (this repo's `.envrc` loads the
  Nix devshell automatically for every command run in the directory,
  including a plain `cargo build`), the linker can pick up libraries like
  `libiconv` from the Nix store by their absolute `/nix/store/...` path
  instead of the system copy. That's non-portable and, once the app is
  actually signed (see above), gets rejected outright by macOS's Library
  Validation. `scripts/fix-macos-dylib-paths.sh` (wired in as `tauri.conf.
json`'s `beforeBundleCommand`) repoints known offenders at their system
  equivalents automatically before bundling — you shouldn't need to think
  about this, but it's worth knowing if a build ever crashes at launch with
  "Library not loaded".

### Linux (X11)

Tauri's Linux backend links directly against `webkit2gtk`/`GTK` at build
time, so cross-compiling from macOS isn't practical. `just build-linux`
instead builds natively inside a real Debian container
([`docker/linux-build.Dockerfile`](docker/linux-build.Dockerfile)) — this is
also how local `cargo clippy`/`cargo test` should be verified for
Linux-specific code, since `fleet-snowfluff-core` (no GTK/Tauri dependencies)
type-checks fine cross-platform, but the full `fleet-snowfluff` app crate
needs the real system libraries the container provides.

Deliberately does **not** pass `--platform linux/amd64` to Docker: on Apple
Silicon that forces QEMU emulation, which OOM-killed the `gtk` crate under
Docker Desktop's default memory limit. Building natively for the host's own
architecture (aarch64 on Apple Silicon) avoids emulation entirely and
produces an aarch64 Linux binary — fine for local build/lint/test
verification, but not what CI's release workflow ships (that runs on real
x86_64 GitHub-hosted runners). Named Docker volumes cache the Cargo registry,
the frontend's `node_modules`, and `target/` across runs so repeat builds
don't start from scratch.

The Nix devshell also carries an `x86_64-unknown-linux-gnu` Rust std target
(see [`flake.nix`](flake.nix)), so `cargo check`/`cargo clippy --target
x86_64-unknown-linux-gnu -p fleet-snowfluff-core` works directly from this
devshell without the container — useful for a fast type-check of
platform-agnostic changes, but it can't build the GTK-linked `fleet-snowfluff`
crate itself (no system libraries on a macOS host to link against), so
anything touching `platform/linux.rs` still needs `just build-linux` for real
verification.

### Nix / NixOS

`.deb` and AppImage (the two Linux artifacts `release.yaml` ships) both
assume a standard FHS layout, which NixOS deliberately doesn't have —
AppImage in particular is known to need extra shims (`nix-alien`,
`appimage-run`) to run on NixOS at all. `flake.nix` exposes a native
`packages.default` (built from [`devshell/package.nix`](devshell/package.nix)
via [crane](https://github.com/ipetkov/crane), with the frontend prebuilt
separately by [`devshell/ui.nix`](devshell/ui.nix)) as a NixOS/Nix-native
alternative install path, **not a replacement** for the `.deb`/AppImage —
those still matter for everyone else on Linux, and a `nix build` output only
runs on a machine that has Nix itself (resolving the exact `/nix/store` paths
it was linked against), not on an arbitrary Linux system.

```sh
nix build          # produces ./result/bin/fleet-snowfluff
nix run             # build + run in one step
```

**Consuming it from your own NixOS/home-manager config** — add this repo as a
flake input and reference its `packages.<system>.default`:

```nix
# flake.nix
inputs.fleet-snowfluff.url = "github:kagetsuki1997/fleet-snowfluff";

# then, wherever you build your system/home-manager config, with
# `system` resolved to your host's (e.g. "x86_64-linux"):
environment.systemPackages = [
  inputs.fleet-snowfluff.packages.${system}.default
];
# or, for home-manager:
home.packages = [ inputs.fleet-snowfluff.packages.${system}.default ];
```

Or try it without installing anything: `nix run github:kagetsuki1997/fleet-snowfluff`.

**Known limitations, currently unverified on real hardware:**

- `packages.default` is Linux-only in practice (built via GTK-specific
  tooling — `wrapGAppsHook3`, `webkitgtk_4_1`) even though `flake-utils`'
  `eachDefaultSystem` technically evaluates it for Darwin systems too; macOS
  building/running through this path isn't a supported or tested
  configuration. Use `just build`/`just macos-signing` on macOS instead.
- **Pet-window transparency works inside a VirtualBox VM guest**, confirmed
  on a NixOS guest: VirtualBox's Guest Additions only pass through OpenGL
  (as `SVGA3D`/llvmpipe), not Vulkan, so no real Vulkan ICD is present —
  `vulkaninfo` only ever finds Mesa's Lavapipe (software Vulkan). wgpu's
  default backend selection (`Backends::all()`) enumerated both GL and
  Vulkan adapters and picked GLES/EGL, whose X11 surface only ever
  advertises the `Opaque` composite alpha mode, so pets rendered fully
  opaque regardless of what was actually behind it. `PetManager`'s instance
  now explicitly prefers Vulkan (`select_wgpu_backends` in `manager.rs`,
  falling back to every backend only when no Vulkan ICD exists at all) —
  Lavapipe's X11 surface correctly advertises `PreMultiplied`/`PostMultiplied`
  alpha, so software Vulkan alone is sufficient. Real hardware, with either
  real GPU passthrough or a proper Vulkan ICD, should work at least as well;
  treat this as confirmed-in-VM, not yet confirmed on real (non-virtualized)
  hardware.
- The settings webview can still render fully blank inside a VirtualBox VM
  guest — a known WebKitGTK-on-NixOS DMABUF-compositing issue, unrelated to
  the transparency fix above. `devshell/package.nix` wires in
  `WEBKIT_DISABLE_COMPOSITING_MODE=1` as a best-effort fix, not confirmed
  fixed yet.
- **Drag, follow-mouse, and the right-click quick menu now work on Linux**,
  confirmed on a NixOS guest. These all ride the same global mouse-position
  poll, which used to be unconditionally disabled on Linux because
  `device_query`'s X11 backend wraps `Rc<Display>` (not `Send`), which can't
  live inside Tauri's `Mutex<PetManager>` managed state. `platform::linux::
MousePoller` replaces it with a dedicated `x11rb::RustConnection` (a
  pure-Rust, thread-safe XCB client) polling `XQueryPointer` directly —
  `mouse_available()` no longer hardcodes `false` on Linux.
  Real-hardware/other-WM verification is still open, matching this
  project's own "verified on GNOME/KDE" tier for the Linux platform
  generally (design.md D5).

## CI

- **Quality** ([`.github/workflows/quality.yaml`](.github/workflows/quality.yaml)) —
  runs on every push to `main`/`develop` and every PR: formatting
  (`treefmt`), commit-message linting, a spellcheck, `cargo clippy -D
warnings`, and the full test suite. The `clippy`/`test` jobs run through
  the Nix devshell on a real Linux runner, so Linux-specific code is
  compiled for real there too.
- **Release** ([`.github/workflows/release.yaml`](.github/workflows/release.yaml)) —
  triggered by pushing a `v*` tag. Matrix-builds Windows (NSIS `.exe`), Linux
  (`.deb` + AppImage), and macOS (universal `.dmg` + `.app.tar.gz`), signs
  artifacts, generates the updater manifest, and publishes everything to a
  **draft** GitHub Release. Releases stay drafts until manually published
  after the [manual smoke-test checklist](docs/smoke-test-checklist.md)
  passes on each platform — publishing is the moment running updaters
  actually see the new version.

## Design docs

The active rewrite's proposal, design decisions, and per-area specs live
under [`openspec/changes/`](openspec/changes/) — that's the place to look for
_why_ something is built the way it is, not just what it does.
