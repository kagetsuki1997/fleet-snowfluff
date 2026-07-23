# Fleet Snowfluff

<p align="center">
  <img src="assets/gifs/ameath.gif" alt="Fleet Snowfluff desktop pet" width="40%">
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg?style=flat-square" alt="MIT License"></a>
  <a href="https://github.com/kagetsuki1997/fleet-snowfluff/releases"><img src="https://img.shields.io/github/v/release/kagetsuki1997/fleet-snowfluff?style=flat-square&include_prereleases" alt="Latest Release"></a>
  <img src="https://img.shields.io/badge/Windows-0078D6?style=flat-square&logo=windows11&logoColor=white" alt="Windows">
  <img src="https://img.shields.io/badge/macOS-000000?style=flat-square&logo=apple&logoColor=white" alt="macOS">
  <img src="https://img.shields.io/badge/Linux-FCC624?style=flat-square&logo=linux&logoColor=black" alt="Linux">
</p>

<p align="center">
  English ·
  <a href="README.zh-Hans.md">简体中文</a> ·
  <a href="README.md">繁體中文</a> ·
  <a href="README.ja.md">日本語</a> ·
  <a href="README.ko.md">한국어</a>
</p>

A cross-platform desktop pet, rewritten in Rust from the original Python
project [Ameath](https://gitee.com/lzy-buaa-jdi/ameath). Fleet Snowfluff
wanders your screen, follows your cursor, and reacts when you drag it —
running natively on **Windows**, **macOS**, and **Linux (X11)**.

> **Status:** this project is closing in on `v0.1.0`, with core functionality
> already working on all three platforms. The original Python app still lives
> at [`legacy/`](legacy/) as the behavior reference. For the development
> history, design decisions, and specs, see
> [`openspec/changes/`](openspec/changes/).

## ✨ Features

- 🐾 **Smart motion system** — wander/follow/curious/rest state machine with
  an inertia-based movement model, so it never feels robotic
- 🖱️ **Follows your cursor** — distance-aware: actively follows from far
  away, curiously watches up close
- 🎭 **Rich animation set** — move, idle, drag, and pause-reaction
  animations; draggable with a voice reaction
- 👥 **Multiple instances** — several pets on screen at once, with settings
  applying live to every one
- 📏 **Adjustable scale and opacity** — both apply immediately, no restart
- 🖥️ **Three display-priority modes** — always-on-top, normal (auto-hides
  over fullscreen apps), or desktop-only
- 👆 **Click-through** and **multi-monitor placement** — stays out of your
  way, and can be confined to a specific screen
- 🖱️ **Right-click quick menu** and **system tray** — everyday settings a
  click away
- 🌐 **Five UI languages** — Traditional Chinese, Simplified Chinese,
  English, Japanese, and Korean, auto-detected from the system locale
- 🔊 **Multiple voice languages** — switchable Chinese/Japanese/English/Korean
  voice clips with a volume control
- 🔄 **Signed auto-updates** — distributed via GitHub Releases, every package
  signature-verified

The music player from the original Ameath app has been intentionally
dropped from this rewrite.

## 📦 Download & Install

Grab the installer for your platform from
[GitHub Releases](https://github.com/kagetsuki1997/fleet-snowfluff/releases).
This project is under active development — if there isn't a release yet, or
you'd like to try the latest code or contribute, see
[`DEVELOP.md`](DEVELOP.md) for building from source.

## 🧑‍💻 Development

<img src="assets/gifs/screen7.gif" alt="" align="right" width="72">

For running locally, building for each platform, and the details of
cross-platform builds (Windows cross-compilation, macOS code signing, the
Linux container build, and more), see [`DEVELOP.md`](DEVELOP.md).

## 🐣 Origins & Attribution

Fleet Snowfluff is a Rust rewrite of **Ameath**, a fan-made desktop pet
originally created by [**-fugu-**](https://space.bilibili.com/84508966).
All character art, animations, and voice clips bundled in this repository
originate from that project. The Rust rewrite (architecture, rendering,
and platform support) is maintained by
[kagetsuki1997](https://github.com/kagetsuki1997).

The pet character and its assets belong to **Wuthering Waves** by
**Kuro Games**. This is an unofficial fan project; it is not affiliated
with or endorsed by Kuro Games. Assets will be removed promptly upon any
legitimate infringement request.

## 📜 License

<img src="assets/gifs/idle4.gif" alt="" align="right" width="72">

Code is licensed under the [MIT License](LICENSE). Bundled character
assets (GIFs, voice clips) are excluded from that grant — see the
attribution section above.
