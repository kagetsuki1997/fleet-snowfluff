# Fleet Snowfluff

<p align="center">
  <img src="assets/gifs/ameath.gif" alt="Fleet Snowfluff デスクトップペット" width="40%">
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
  <a href="README.md">繁體中文</a> ·
  日本語 ·
  <a href="README.ko.md">한국어</a>
</p>

Fleet Snowfluff は、オリジナルの Python プロジェクト
[Ameath](https://gitee.com/lzy-buaa-jdi/ameath) を Rust で書き直した、クロス
プラットフォーム対応のデスクトップペットです。画面上を歩き回り、マウスカーソルに
ついてきて、ドラッグすると反応します——**Windows**・**macOS**・**Linux（X11）**
にネイティブ対応。

> **開発状況：** 本プロジェクトは `v0.1.0` に向けて仕上げの段階にあり、コア機能は
> すでに 3 つのプラットフォームすべてで動作しています。オリジナルの Python 版は
> 動作リファレンスとして [`legacy/`](legacy/) に残されています。開発の経緯や設計
> 判断、仕様については [`openspec/changes/`](openspec/changes/) をご覧ください。

## ✨ 主な機能

- 🐾 **スマートな動作システム** — 徘徊／追従／興味／休憩のステートマシンと慣性
  ベースの移動モデルで、機械的にならない自然な動き
- 🖱️ **マウス追従** — 距離に応じて判断し、遠くでは積極的に追従、近くでは興味深そうに
  観察
- 🎭 **豊富なアニメーション** — 移動・待機・ドラッグ・一時停止時のリアクションなど
  多彩なアニメーション。ドラッグすると音声リアクションも
- 👥 **複数インスタンス** — 複数体を同時に表示可能、設定はすべてのインスタンスに
  即時反映
- 📏 **自由なスケール・不透明度調整** — どちらも再起動なしで即座に反映
- 🖥️ **3 種類の表示優先度** — 常に最前面、通常（全画面アプリ使用時は自動的に
  非表示）、デスクトップのみ
- 👆 **クリックスルー** と **マルチモニター配置** — 日常操作の邪魔をせず、表示する
  モニターも指定可能
- 🖱️ **右クリックのクイックメニュー** と **システムトレイ** — よく使う設定に
  すぐアクセス
- 🌐 **5 つの表示言語** — 繁体字中国語・簡体字中国語・英語・日本語・韓国語に対応、
  システムのロケールから自動検出
- 🔊 **複数の音声言語** — 中国語／日本語／英語／韓国語のボイスを切り替え可能、
  音量調整も
- 🔄 **署名付き自動アップデート** — GitHub Releases 経由で配布、パッケージは
  すべて署名検証済み

オリジナルの Ameath にあった音楽プレイヤー機能は、今回の書き直しで意図的に
削除されています。

## 📦 ダウンロードとインストール

[GitHub Releases](https://github.com/kagetsuki1997/fleet-snowfluff/releases)
から、お使いのプラットフォーム向けのインストーラーを入手してください。本プロジェクトは
現在も活発に開発中です。まだリリースがない場合や、最新のコードを試したい・開発に
参加したい場合は、[`DEVELOP.md`](DEVELOP.md) のソースからのビルド方法をご覧ください。

また、NixOS／Nix ユーザーは本リポジトリの flake から直接ビルド・実行
（`nix build`／`nix run`）したり、flake input として自分のシステム構成に
組み込んだりすることもできます。詳細は
[`DEVELOP.md`](DEVELOP.md#nix--nixos) をご覧ください。

## 🧑‍💻 開発

<img src="assets/gifs/screen7.gif" alt="" align="right" width="72">

ローカルでの実行方法、各プラットフォーム向けのビルド、クロスプラットフォームビルドの
詳細（Windowsのクロスコンパイル、macOSのコード署名、Linuxのコンテナビルドなど）に
ついては、[`DEVELOP.md`](DEVELOP.md) をご覧ください。

## 🐣 由来と謝辞

Fleet Snowfluff は、[**-fugu-**](https://space.bilibili.com/84508966) 氏が
制作したファンメイドのデスクトップペット **Ameath** を Rust で書き直したものです。
本リポジトリに含まれるすべてのキャラクターイラスト、アニメーション、ボイスは
同プロジェクトに由来します。Rust 版の書き直し（アーキテクチャ、描画、プラットフォーム
対応）は [kagetsuki1997](https://github.com/kagetsuki1997) が保守しています。

ペットキャラクターおよびその素材の著作権は、Kuro Games の『**鳴潮**
（**Wuthering Waves**）』に帰属します。本プロジェクトは非公式のファン制作物であり、
Kuro Games とは提携・承認関係にありません。正当な権利侵害の申し立てを受けた場合、
該当素材は速やかに削除します。

## 📜 ライセンス

<img src="assets/gifs/idle4.gif" alt="" align="right" width="72">

コードは [MIT ライセンス](LICENSE) の下で提供されます。同梱のキャラクター素材
（GIF アニメーション、ボイス）はこの許諾範囲に含まれません。詳細は上記の謝辞を
ご覧ください。
