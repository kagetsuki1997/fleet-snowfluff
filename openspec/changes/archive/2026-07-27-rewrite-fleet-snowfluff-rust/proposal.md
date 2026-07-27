# Proposal: Rewrite Ameath as Fleet Snowfluff in Rust

## Why

Ameath is a Windows-only Python/tkinter desktop pet whose architecture caps its future: tkinter forces a pink chroma-key transparency hack, all window layering is hard-wired to Win32, the UI is hard-coded Simplified Chinese, and distribution relies on a hand-written `.bat` self-updater. A Rust rewrite (Tauri v2) delivers the same pet on Windows, macOS, and Ubuntu with real alpha rendering, five UI languages, switchable voice-pack languages, signed cross-platform auto-updates, and a testable behavior core — under the new identity **Fleet Snowfluff**, version restarting at 0.1.0.

## What Changes

- **BREAKING**: Full rewrite in Rust (Tauri v2); Python implementation moves to `legacy/` and is kept permanently as the behavior reference (no longer the shipped app)
- **BREAKING**: Music player removed entirely (player module, settings tab, and bundled MP3s)
- **BREAKING**: Full rebrand — product "Fleet Snowfluff", binary `fleet-snowfluff`, identifier `fleet-snowfluff`; config relocates to Tauri `app_config_dir` with one-shot migration from `%APPDATA%/ameath_config.json`
- All existing pet features are ported: behavior state machine (wander/follow/curious/rest), inertia motion, drag with voice, pause-mode animations, multi-instance (up to 80), scale/transparency steps, click-through, follow-mouse, display priority (topmost / normal+fullscreen-hide / desktop-only), wander stay modes, multi-monitor, window snap, autostart, skip-version update logic
- Pet windows become plain native (non-webview) windows rendered by Rust/wgpu with real per-pixel alpha (pink chroma-key eliminated); settings window is a webview (vanilla TS); quick menu becomes a native context menu; tray via Tauri
- New: UI localization in zh-Hant, zh-Hans, en, ja, ko (system-locale detection, zh-Hant fallback)
- New: voice language setting (zh/ja/en/ko) independent of UI language; per-language asset folders + manifest; languages without assets shown disabled; voice volume 0–150% retained
- New: cross-platform support — Windows (strict parity), macOS (verified), Ubuntu on X11/XWayland (verified on GNOME/KDE; native Wayland out of scope)
- New: auto-update via `tauri-plugin-updater` + GitHub Releases with minisign-signed packages (replaces Gitee + `.bat` script)
- New: GitHub Actions CI — fmt/clippy/test on PRs and pushes to `develop`/`main`; `v*` tags build NSIS `.exe`, `.deb` + AppImage, universal macOS `.dmg` and upload to a draft GitHub Release
- Attribution: LICENSE retains original copyright and adds the new holder; About/README credit the original Ameath project and `-fugu-`, and state that GIF/voice assets are Wuthering Waves © Kuro Games, removed on request

## Capabilities

### New Capabilities

- `pet-behavior`: motion/state engine — wander, follow-mouse, curious, rest states, inertia and jitter physics, edge escape/respawn, drag interaction, pause mode with random animations, multi-instance management
- `pet-rendering`: native per-pet windows with wgpu-blitted GIF frames, real alpha transparency, scale steps, opacity steps, horizontal flip by direction
- `desktop-integration`: platform window layering — display priority modes (topmost / normal + fullscreen-hide / desktop-only), click-through, multi-monitor placement, window snap, system tray, native quick (right-click) menu; tiered per-platform acceptance
- `voice`: random voice-line playback on interaction, per-language asset packs (zh/ja/en/ko) with manifest, disabled-when-empty language picker, enable toggle, 0–150% volume
- `localization`: UI strings in zh-Hant/zh-Hans/en/ja/ko from JSON locale files consumed by both Rust (tray/menus) and webview UI; system-locale detection with zh-Hant fallback; user override
- `app-config`: typed config schema with validation, Tauri config-dir storage, one-shot migration from the legacy `ameath_config.json` (drops music keys, adds language keys, swaps autostart registration), per-OS autostart
- `settings-ui`: webview settings window with personalization, update, and about tabs (about carries credits and the Kuro Games asset disclaimer)
- `auto-update`: startup version check against GitHub Releases, signed package install via tauri-plugin-updater, skip-this-version and skip-all-updates behavior
- `release-pipeline`: CI quality gates (fmt/clippy/test) and tag-triggered multi-platform artifact builds uploaded to draft GitHub Releases with updater manifest

### Modified Capabilities

<!-- none — no existing specs; this change introduces the spec baseline -->

## Impact

- **Code**: new Cargo workspace (`crates/fleet-snowfluff-core`, `crates/fleet-snowfluff`, `ui/`); `main.py` + `ameath/` relocate to `legacy/`; `sound/music/` deleted; `justfile` retargeted to cargo/tauri
- **Assets**: `gifs/` reused as-is (native GIF transparency already present); `sound/voice/` restructured to `assets/voice/zh/` + empty `ja|en|ko` + manifest; `fonts/zpix.ttf` reused with CSS fallback chain (Hangul/kana coverage to verify)
- **Dependencies**: Tauri v2, wgpu, rodio, image/gif, sys-locale, tauri-plugin-updater, tauri-plugin-autostart, platform crates (`windows`, `objc2`, `x11rb`); Python deps frozen in `legacy/`
- **Distribution**: Gitee update flow retired; GitHub Releases becomes the sole channel; OS code signing/notarization deferred (documented SmartScreen/Gatekeeper caveats)
- **Users**: existing settings carried over by migration; music features disappear; old autostart registry entry (`DesktopPet`) replaced
