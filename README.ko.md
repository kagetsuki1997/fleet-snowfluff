# Fleet Snowfluff

<p align="center">
  <img src="assets/gifs/ameath.gif" alt="Fleet Snowfluff 데스크톱 펫" width="40%">
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
  <a href="README.ja.md">日本語</a> ·
  한국어
</p>

Fleet Snowfluff는 원본 Python 프로젝트
[Ameath](https://gitee.com/lzy-buaa-jdi/ameath)를 Rust로 다시 만든
크로스플랫폼 데스크톱 펫입니다. 화면을 배회하고, 마우스 커서를 따라다니며,
드래그하면 반응합니다 — **Windows**, **macOS**, **Linux(X11)**에서 네이티브로
동작합니다.

> **개발 상태:** 이 프로젝트는 `v0.1.0`을 향해 마무리 단계에 있으며, 핵심 기능은
> 이미 세 플랫폼 모두에서 동작합니다. 원본 Python 앱은 동작 참고용으로
> [`legacy/`](legacy/)에 그대로 남아 있습니다. 개발 배경, 설계 결정, 명세 문서는
> [`openspec/changes/`](openspec/changes/)를 참고하세요.

## ✨ 주요 기능

- 🐾 **스마트한 이동 시스템** — 배회/추적/호기심/휴식 상태 머신과 관성 기반 이동
  모델로 기계적이지 않은 자연스러운 움직임
- 🖱️ **마우스 따라가기** — 거리 기반 판단: 멀리서는 적극적으로 따라오고, 가까이서는
  호기심 있게 관찰
- 🎭 **다양한 애니메이션** — 이동, 대기, 드래그, 일시정지 리액션 등 다양한
  애니메이션 상태. 드래그하면 음성 리액션도 재생
- 👥 **다중 인스턴스** — 여러 마리를 동시에 표시 가능, 설정은 모든 인스턴스에
  즉시 적용
- 📏 **자유로운 크기·불투명도 조절** — 재시작 없이 즉시 적용
- 🖥️ **3가지 표시 우선순위** — 항상 위에 표시, 일반(전체 화면 앱 사용 시 자동
  숨김), 바탕화면 전용
- 👆 **클릭 통과** 및 **다중 모니터 배치** — 평소 작업을 방해하지 않으며, 표시할
  모니터도 지정 가능
- 🖱️ **우클릭 빠른 메뉴** 및 **시스템 트레이** — 자주 쓰는 설정에 손쉽게 접근
- 🌐 **5가지 UI 언어** — 번체 중국어, 간체 중국어, 영어, 일본어, 한국어를 시스템
  로케일에서 자동 감지
- 🔊 **다양한 음성 언어** — 중국어/일본어/영어/한국어 음성 클립을 전환 가능,
  볼륨 조절도 지원
- 🔄 **서명된 자동 업데이트** — GitHub Releases를 통해 배포되며, 모든 패키지는
  서명 검증을 거침

원본 Ameath에 있던 음악 플레이어 기능은 이번 재작성에서 의도적으로 제외되었습니다.

## 📦 다운로드 및 설치

[GitHub Releases](https://github.com/kagetsuki1997/fleet-snowfluff/releases)에서
사용 중인 플랫폼에 맞는 설치 파일을 받으세요. 이 프로젝트는 활발히 개발 중입니다.
아직 릴리스가 없거나 최신 코드를 미리 사용해보거나 개발에 참여하고 싶다면,
소스에서 빌드하는 방법은 [`DEVELOP.md`](DEVELOP.md)를 참고하세요.

또한 NixOS/Nix 사용자는 이 저장소의 flake를 통해 직접 빌드하거나 실행할 수 있으며
(`nix build`/`nix run`), flake input으로 자신의 시스템 설정에 추가할 수도
있습니다. 자세한 내용은 [`DEVELOP.md`](DEVELOP.md#nix--nixos)를 참고하세요.

## 🧑‍💻 개발

<img src="assets/gifs/screen7.gif" alt="" align="right" width="72">

로컬 실행, 각 플랫폼용 빌드, 크로스플랫폼 빌드의 세부 사항(Windows 크로스 컴파일,
macOS 코드 서명, Linux 컨테이너 빌드 등)은 [`DEVELOP.md`](DEVELOP.md)를
참고하세요.

## 🐣 유래 및 감사의 말

Fleet Snowfluff는 [**-fugu-**](https://space.bilibili.com/84508966)님이 제작한
팬메이드 데스크톱 펫 **Ameath**를 Rust로 다시 만든 버전입니다. 이 저장소에 포함된
모든 캐릭터 아트, 애니메이션, 음성은 해당 프로젝트에서 비롯되었습니다. Rust
재작성 버전(아키텍처, 렌더링, 플랫폼 지원)은
[kagetsuki1997](https://github.com/kagetsuki1997)이 관리합니다.

펫 캐릭터와 관련 리소스의 저작권은 쿠로 게임즈(**Kuro Games**)의
**명조**(**Wuthering Waves**)에 있습니다. 이 프로젝트는 비공식 팬 프로젝트이며
쿠로 게임즈와 제휴하거나 그들의 승인을 받지 않았습니다. 정당한 저작권 침해 신고가
접수되면 해당 리소스를 즉시 삭제합니다.

## 📜 라이선스

<img src="assets/gifs/idle4.gif" alt="" align="right" width="72">

코드는 [MIT 라이선스](LICENSE)로 제공됩니다. 번들로 포함된 캐릭터 리소스(GIF
애니메이션, 음성 클립)는 이 허가 범위에 포함되지 않습니다. 자세한 내용은 위의
감사의 말을 참고하세요.
