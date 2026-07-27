# Fleet Snowfluff（飛行雪絨）

<p align="center">
  <img src="assets/gifs/ameath.gif" alt="Fleet Snowfluff 桌面寵物" width="40%">
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg?style=flat-square" alt="MIT License"></a>
  <a href="https://github.com/kagetsuki1997/fleet-snowfluff/releases"><img src="https://img.shields.io/github/v/release/kagetsuki1997/fleet-snowfluff?style=flat-square&include_prereleases" alt="Latest Release"></a>
  <img src="https://img.shields.io/badge/Windows-0078D6?style=flat-square&logo=windows11&logoColor=white" alt="Windows">
  <img src="https://img.shields.io/badge/macOS-000000?style=flat-square&logo=apple&logoColor=white" alt="macOS">
  <img src="https://img.shields.io/badge/Linux-FCC624?style=flat-square&logo=linux&logoColor=black" alt="Linux">
  <a href="DEVELOP.md#nix--nixos"><img src="https://img.shields.io/badge/Nix-flake-5277C3?style=flat-square&logo=nixos&logoColor=white" alt="Nix flake"></a>
</p>

<p align="center">
  <a href="README.en.md">English</a> ·
  <a href="README.zh-Hans.md">简体中文</a> ·
  繁體中文 ·
  <a href="README.ja.md">日本語</a> ·
  <a href="README.ko.md">한국어</a>
</p>

Fleet Snowfluff 是一隻跨平台的桌面寵物，將原始 Python 專案
[Ameath](https://gitee.com/lzy-buaa-jdi/ameath) 以 Rust 重寫而成。牠會在螢幕上遊蕩、
跟隨你的滑鼠、被拖曳時會有反應——原生支援 **Windows**、**macOS** 與 **Linux（X11）**。

> **開發狀態：** 本專案正朝 `v0.1.0` 邁進，核心功能已在三個平台上可運作。原始 Python
> 版本仍保留於 [`legacy/`](legacy/)，作為行為對照的參考版本。有興趣了解開發脈絡、
> 設計決策與規格文件，請見 [`openspec/changes/`](openspec/changes/)。

## ✨ 特色功能

- 🐾 **智慧運動系統** — 遊蕩／跟隨／好奇／休息狀態機，搭配慣性移動模型，動作自然不生硬
- 🖱️ **滑鼠跟隨** — 依距離智慧判斷，遠距離主動跟隨，近距離好奇觀察
- 🎭 **多樣動畫** — 移動、待機、拖曳、暫停反應等多種動畫狀態，可拖曳並伴隨語音反應
- 👥 **多重實例** — 同時顯示多隻桌面寵物，設定即時套用於所有實例
- 📏 **自由縮放與透明度** — 縮放與不透明度皆可即時調整，不需重新啟動
- 🖥️ **三種顯示優先度** — 永遠置頂、一般（全螢幕應用程式時自動隱藏）、僅桌面層
- 👆 **滑鼠穿透** 與 **多螢幕擺放** — 不干擾日常操作，並可指定要出現在哪個螢幕
- 🖱️ **右鍵快速選單** 與 **系統托盤** — 常用設定觸手可及
- 🌐 **五種介面語言** — 繁體中文、簡體中文、英文、日文、韓文，依系統語言自動偵測
- 🔊 **多語音語言** — 中／日／英／韓語音片段可切換，並可調整音量
- 🔄 **簽章式自動更新** — 透過 GitHub Releases 發布，更新套件皆經簽章驗證

原版 Ameath 的音樂播放器功能，已在本次重寫中刻意移除。

## 📦 下載與安裝

前往 [GitHub Releases](https://github.com/kagetsuki1997/fleet-snowfluff/releases)
下載對應平台的安裝包。專案仍在活躍開發中，若尚未有正式發行版，或想搶先體驗、參與開發，
可參考 [`DEVELOP.md`](DEVELOP.md) 從原始碼建置。

另外，NixOS／Nix 使用者可透過本專案提供的 flake 直接建置或執行（`nix build`／
`nix run`），或作為 flake input 加入自己的系統設定，詳見
[`DEVELOP.md`](DEVELOP.md#nix--nixos)。

## 🧑‍💻 開發

<img src="assets/gifs/screen7.gif" alt="" align="right" width="72">

想在本機執行、建置各平台版本，或了解跨平台建置的細節（Windows 交叉編譯、macOS 簽章、
Linux 容器建置等），請見 [`DEVELOP.md`](DEVELOP.md)。

## 🐣 起源與致謝

Fleet Snowfluff 是 **Ameath** 的 Rust 重寫版，原作者為
[**-fugu-**](https://space.bilibili.com/84508966)。本專案中包含的所有角色美術、
動畫與語音片段，皆來自該專案。Rust 重寫版本（架構、繪製與平台支援）由
[kagetsuki1997](https://github.com/kagetsuki1997) 維護。

寵物角色及其資源版權屬於《**鳴潮**》（**Kuro Games**）所有。本專案為非官方粉絲作品，
與 Kuro Games 無關聯、亦未獲其認可。若收到合法侵權申訴，將立即移除相關資源。

## 📜 授權條款

<img src="assets/gifs/idle4.gif" alt="" align="right" width="72">

程式碼採用 [MIT 授權條款](LICENSE)。隨附的角色資源（GIF 動畫、語音片段）不在此授權
範圍內，詳見上方致謝說明。
