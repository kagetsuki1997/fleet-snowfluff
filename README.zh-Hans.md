# Fleet Snowfluff（飞行雪绒）

<p align="center">
  <img src="assets/gifs/ameath.gif" alt="Fleet Snowfluff 桌面宠物" width="40%">
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg?style=flat-square" alt="MIT License"></a>
  <a href="https://github.com/kagetsuki1997/fleet-snowfluff/releases"><img src="https://img.shields.io/github/v/release/kagetsuki1997/fleet-snowfluff?style=flat-square&include_prereleases" alt="Latest Release"></a>
  <img src="https://img.shields.io/badge/Windows-0078D6?style=flat-square&logo=windows11&logoColor=white" alt="Windows">
  <img src="https://img.shields.io/badge/macOS-000000?style=flat-square&logo=apple&logoColor=white" alt="macOS">
  <img src="https://img.shields.io/badge/Linux-FCC624?style=flat-square&logo=linux&logoColor=black" alt="Linux">
</p>

<p align="center">
  <a href="README.en.md">English</a> ·
  简体中文 ·
  <a href="README.md">繁體中文</a> ·
  <a href="README.ja.md">日本語</a> ·
  <a href="README.ko.md">한국어</a>
</p>

Fleet Snowfluff 是一只跨平台的桌面宠物，将原始 Python 项目
[Ameath](https://gitee.com/lzy-buaa-jdi/ameath) 用 Rust 重写而成。它会在屏幕上游荡、
跟随你的鼠标、被拖动时会有反应——原生支持 **Windows**、**macOS** 与 **Linux（X11）**。

> **开发状态：** 本项目正朝 `v0.1.0` 迈进，核心功能已在三个平台上可运行。原始 Python
> 版本仍保留在 [`legacy/`](legacy/)，作为行为对照的参考版本。想了解开发脉络、设计
> 决策与规格文档，请见 [`openspec/changes/`](openspec/changes/)。

## ✨ 特色功能

- 🐾 **智能运动系统** — 游荡／跟随／好奇／休息状态机，配合惯性移动模型，动作自然不生硬
- 🖱️ **鼠标跟随** — 依距离智能判断，远距离主动跟随，近距离好奇观察
- 🎭 **多样动画** — 移动、待机、拖动、暂停反应等多种动画状态，可拖动并伴随语音反应
- 👥 **多重实例** — 同时显示多只桌面宠物，设置即时应用于所有实例
- 📏 **自由缩放与透明度** — 缩放与不透明度均可即时调整，无需重启
- 🖥️ **三种显示优先级** — 始终置顶、常规（全屏应用时自动隐藏）、仅桌面层
- 👆 **鼠标穿透** 与 **多屏幕摆放** — 不干扰日常操作，并可指定要出现在哪个屏幕
- 🖱️ **右键快速菜单** 与 **系统托盘** — 常用设置触手可及
- 🌐 **五种界面语言** — 繁体中文、简体中文、英文、日文、韩文，依系统语言自动检测
- 🔊 **多语音语言** — 中／日／英／韩语音片段可切换，并可调整音量
- 🔄 **签名式自动更新** — 通过 GitHub Releases 发布，更新包均经签名验证

原版 Ameath 的音乐播放器功能，已在本次重写中刻意移除。

## 📦 下载与安装

前往 [GitHub Releases](https://github.com/kagetsuki1997/fleet-snowfluff/releases)
下载对应平台的安装包。项目仍在活跃开发中，若尚未有正式发行版，或想抢先体验、参与开发，
可参考 [`DEVELOP.md`](DEVELOP.md) 从源码构建。

## 🧑‍💻 开发

<img src="assets/gifs/screen7.gif" alt="" align="right" width="72">

想在本机运行、构建各平台版本，或了解跨平台构建的细节（Windows 交叉编译、macOS 签名、
Linux 容器构建等），请见 [`DEVELOP.md`](DEVELOP.md)。

## 🐣 起源与致谢

Fleet Snowfluff 是 **Ameath** 的 Rust 重写版，原作者为
[**-fugu-**](https://space.bilibili.com/84508966)。本项目中包含的所有角色美术、
动画与语音片段，均来自该项目。Rust 重写版本（架构、渲染与平台适配）由
[kagetsuki1997](https://github.com/kagetsuki1997) 维护。

宠物角色及其资源版权归《**鸣潮**》（**库洛游戏**）所有。本项目为非官方粉丝作品，
与库洛游戏无关联、未获其认可。如收到合法侵权申诉，将立即移除相关资源。

## 📜 许可协议

<img src="assets/gifs/idle4.gif" alt="" align="right" width="72">

代码采用 [MIT 许可证](LICENSE)。随附的角色资源（GIF 动画、语音片段）不在此授权
范围内，详见上方致谢说明。
