# Fleet Snowfluff 功能規劃摘要

> 本文件整理自一次關於「桌面寵物新功能與 AI 整合」的討論，並在第一階段完成後重新評估後續架構。涵蓋功能發想、AI provider 整合、Persona、情境感知、記憶、Agent / Tool Calling、任務升級（escalation）、OpenClaw / Codex / Claude Code 整合等主題。目的是作為後續深入規劃（例如搭配 openspec）的輸入來源，非最終規格書。

> 專案背景：Fleet Snowfluff 是以 Rust 重寫的跨平台桌面寵物（原始碼庫已具備遊蕩/跟隨/好奇狀態機、拖曳語音反應、多實例、縮放/透明度、系統托盤、五種 UI 語言等功能）。目前已完成第一階段的 AI 基礎能力，本文件重新整理下一階段應優先建立的 Agent 能力。

---

## 1. 新功能發想

### 1.1 互動與陪伴類

- **對話氣泡**：點擊寵物跳出輸入框，打字互動，以文字氣泡回覆。第一階段已完成，後續重點轉向讓「聊天」不只是問答，而能逐步變成 Agent 入口。
- **情境吐槽**：讀取當前作用中視窗、系統狀態（CPU/電量/時間），主動講一句應景的話。細節見第 5 節「情境感知」。
- **情緒系統**：依互動頻率/時間累積好感度或心情值，影響動畫與語音選擇。
- **多寵物互動**：專案已支援多實例，可讓兩隻寵物靠近時觸發「對話」動畫，適合用 AI 生成互動內容。

### 1.2 生產力類

- **番茄鐘/休息提醒**：主動提醒休息，建議依實際使用模式（情境感知規則）判斷，而非死板計時。
- **剪貼簿助手**：選取文字後右鍵選單加一個「問問牠」，把選取內容丟給 AI 摘要或翻譯。
- **系統通知代理**：接 calendar/待辦事項，寵物主動唸出今天行程。
- **專案助手**：讀取專案、搜尋程式碼、執行測試、協助修改檔案。這類功能屬於 Agent 任務，不應直接塞進單純 Chat Provider。

### 1.3 技術類

- **語音輸入**：接麥克風 + STT，直接用講的互動（本次仍列為未來方向）。
- **外掛/腳本系統**：讓使用者自訂觸發規則（例如「偵測到某網站在背景執行就講一句話」）。
- **MCP 工具整合**：讓 Agent 能以統一方式接 web search、GitHub、檔案、資料庫等外部工具。
- **Agent Backend 整合**：必要時將複雜任務升級給 Codex / Claude Code / OpenClaw，而不是所有對話都使用重型 Agent runtime。

---

## 2. AI Provider / 本地模型整合架構

### 2.1 核心設計：Provider 抽象層

第一階段已完成 Provider 抽象層與聊天能力。原本以：

```rust
trait AiProvider {
    async fn chat(&self, messages: Vec<Message>) -> Result<Response>;
}
```

為核心。

後續不應把 Provider 抽象成「一個 HTTP API endpoint」，而應逐步拆成三個概念：

1. **Model Provider**：模型來自哪裡，例如 OpenAI、Anthropic、Ollama。
2. **Auth Method**：如何取得使用權限，例如 API Key、OAuth、CLI Session、Local。
3. **Execution Backend / Runtime**：由誰執行這次 Agent turn，例如直接 API、Ollama、Codex、Claude Code、OpenClaw。

建議概念模型：

```rust
enum AuthMethod {
    Local,
    ApiKey,
    OAuth,
    CliSession,
}

enum ExecutionBackend {
    DirectApi,
    Local,
    Cli,
    NativeAgent,
}
```

不要求現在立刻完整實作所有 enum；重點是不要把架構鎖死在 `API key -> chat()`。

### 2.2 Provider 實作

第一階段已有：

- `OpenAiCompatible`：可指向 OpenAI / Anthropic API，或任何 OpenAI 相容端點
- `Ollama`：本地 HTTP server
- `Embedded`（未來選配）：用 Rust 原生的 candle 直接跑模型

重新評估後：

- **Ollama 應提升優先級**：因為它是後續本地 Agent / Tool Calling 的主要基礎。
- **Embedded 暫時維持低優先級**：除非需要完全免安裝、單一 binary，否則先不要承擔模型打包與跨平台硬體相容性的成本。
- **OpenAI / Anthropic API 仍保留**：適合一般雲端聊天、web search、需要較強模型的任務。
- **Codex / Claude Code 不應直接塞進 `OpenAiCompatible`**：它們屬於 Agent runtime / CLI backend，而不是單純 API endpoint。

### 2.3 Auth / Provider / Runtime 分層

建議最後形成：

```text
                         Fleet Snowfluff
                                │
                         Agent Runtime
                                │
             ┌──────────────────┴──────────────────┐
             │                                     │
        Model Provider                       Execution Backend
             │                                     │
      ┌──────┼─────────┐                  ┌─────────┼─────────┐
      │      │         │                  │         │         │
   Ollama  OpenAI  Anthropic           Direct    Codex   Claude Code
      │      │         │                  API       │         │
    Local   API/OAuth API/CLI                     │         │
                                                 subscription
```

這樣未來才可以讓「同一個聊天 UI」根據任務選擇不同 backend。

### 2.4 Subscription auth 的定位

**本專案重新評估後，subscription auth 提升為第一優先。**

第一階段目前已能以 API key / Ollama 進行一般 Chat，但如果產品目標是讓使用者把 Fleet 當成自己的桌面 AI 助手，則不應要求使用者為已經持有的 ChatGPT / Claude subscription 再建立一組獨立 API billing。

因此下一階段的第一個目標應是：

> **先讓一般 Chat 可以透過 provider 的 subscription / OAuth / CLI auth 使用 AI，再往 Agent / Tool Calling 擴充。**

Subscription 不應理解成「把 ChatGPT / Claude subscription 轉成一般 API」。正確做法是使用 provider 自己支援的 subscription execution path。

目前 OpenClaw 採取的是 provider-specific 路徑：

- **OpenAI / Codex**：ChatGPT/Codex OAuth + native Codex app-server。OpenClaw 目前使用 canonical `openai/*` model route，並由 runtime 選擇 Codex app-server；subscription credential 與 API-key credential 是不同 auth profile。citeturn0search0turn0search3
- **Anthropic / Claude Code**：使用已登入的 Claude Code CLI，透過 `claude -p` / Agent SDK 類程式化路徑執行；OpenClaw 目前文件記載此用量會計入登入帳號的 subscription limits，而且 Claude Code 自己管理 login / token refresh。citeturn0search1
- **API Key**：仍是一般 Platform API 的 usage-based billing，與 subscription quota 分開。
- **其他 provider**：若支援 Coding Plan / CLI OAuth / subscription auth，也應視為 provider-specific execution backend，不應抽象成通用「OAuth 就能用 subscription」。OpenClaw 目前也列出 Qwen Cloud、MiniMax、Z.AI/GLM 等 subscription-style 選項。citeturn0search2

因此 Fleet 應逐步形成：

```text
OpenAI
 ├── API Key → Direct API
 └── OAuth   → Codex runtime

Anthropic
 ├── API Key → Direct API
 └── CLI     → Claude Code runtime

Ollama
 └── Local   → Local runtime
```

**重要：subscription 路徑不是通用保證。Provider 可以修改計費、rate limit 或第三方使用政策，因此應把 subscription integration 做成 provider adapter，而不是把它寫死在一般 `AiProvider::chat()` 裡。** Anthropic 官方/相關文件尤其明確提醒 billing 與 rate-limit 行為可能變動。citeturn0search1

### 2.5 整體資料流

第一階段：

```text
輸入來源（使用者輸入 / 系統情境）
        ↓
Persona + Prompt 組裝
        ↓
Provider 抽象層
        ↓
回應 + 情緒標籤
        ↓
輸出分派 ── 對話氣泡 / TTS 語音 / 動畫
```

後續：

```text
輸入來源
        ↓
Conversation / Task
        ↓
Agent Runtime
        ↓
Task capability 判斷
        ↓
┌───────────────┬────────────────┬─────────────────┐
│               │                │
Simple Chat     Tool Task        Complex Task
│               │                │
Local Qwen3     Qwen3 + MCP      Codex / Claude
│               │                │
answer          execute tools    long-running work
```

---

## 3. Subscription-first Chat：下一個最高優先級

### 3.1 目標

第一階段已完成一般 Chat，因此下一步不先追求更多工具，而是先把「Chat 使用哪一種 credential / runtime」做好。

目標 UX：

```text
設定
 ├── 本地模型
 │    └── Ollama
 │
 ├── OpenAI
 │    ├── ChatGPT/Codex subscription
 │    └── API Key
 │
 └── Anthropic
      ├── Claude subscription / Claude Code
      └── API Key
```

使用者不需要理解 API endpoint、token 或 billing model；UI 應讓使用者選擇：

```text
Connect with ChatGPT
Connect with Claude
Use API Key
Use Local Model
```

### 3.2 OpenAI：ChatGPT/Codex subscription

OpenClaw 目前的做法值得直接作為參考：

```text
Fleet Chat
   ↓
OpenAI Provider
   ↓
OAuth login
   ↓
OpenAI auth profile
   ↓
Codex app-server runtime
   ↓
ChatGPT/Codex subscription
```

目前 OpenClaw 使用：

```bash
openclaw models auth login --provider openai
```

之後仍以 `openai/*` 作為 canonical model route；runtime policy 再決定是否走 native Codex app-server。這代表「Provider」與「Runtime」必須分離。citeturn0search0turn0search3

Fleet 不應自己把 OAuth token 塞進一般 OpenAI API request：

```text
不要：

OAuth token
   ↓
POST /v1/responses
   ↓
當成 API key 使用
```

而應：

```text
OAuth
  ↓
provider-specific auth profile
  ↓
Codex-compatible runtime
```

### 3.3 Anthropic：Claude subscription

Anthropic 的路徑與 OpenAI 不完全相同。

OpenClaw 目前直接使用同一台主機上的 Claude Code executable / login：

```text
Fleet Chat
   ↓
Anthropic Provider
   ↓
Claude Code CLI
   ↓
claude -p
   ↓
Claude subscription
```

Claude Code 自己負責 login / token refresh，OpenClaw 不應自己複製或管理原生 Claude login token。citeturn0search1

因此 Fleet 第一版 Anthropic subscription integration 可以考慮：

```text
Claude Code installed?
        │
       yes
        ↓
claude auth status
        ↓
authenticated?
   ├── yes → Claude CLI backend
   └── no  → open/login flow
```

### 3.4 第一版不要自己重寫所有 OAuth

Subscription integration 的第一版原則：

> **優先重用 provider 官方 CLI / SDK / app-server，而不是自己逆向 provider authentication。**

原因：

- token refresh / expiry 由 provider 官方 runtime 管理
- 不需要自己保存敏感 credential
- provider 改變 OAuth flow 時維護成本較低
- 比「把 OAuth token 當 API key」正確
- 與 OpenClaw 目前的實作方向一致

### 3.5 Auth profile abstraction

Fleet 可以建立自己的 auth abstraction：

```rust
enum AuthMethod {
    Local,
    ApiKey,
    OAuth,
    CliSession,
}

struct ProviderProfile {
    provider: ProviderId,
    auth: AuthMethod,
    profile_id: String,
}
```

但：

> `AuthMethod::OAuth` 不應直接代表「可以呼叫 HTTP API」。

它只代表「已完成 provider-specific authentication」。

實際執行仍由 backend 決定：

```rust
struct ModelRoute {
    provider: ProviderId,
    model: ModelId,
    auth_profile: ProfileId,
    runtime: ExecutionBackend,
}
```

### 3.6 Subscription-first 的 UI

設定頁建議從：

```text
Provider:
[ OpenAI ]

API Key:
[____________]
```

改成：

```text
AI Provider

OpenAI
  ○ ChatGPT / Codex
  ○ API Key

Anthropic
  ○ Claude
  ○ API Key

Local
  ○ Ollama

[ Connect ]
```

登入完成後顯示：

```text
OpenAI
✓ ChatGPT/Codex connected

Authentication:
Subscription

Runtime:
Codex

Model:
gpt-...
```

不要要求使用者手動填 OAuth token。

### 3.7 Subscription-first 的限制

Subscription auth 要視為「可用的 provider capability」，而不是產品可以永久保證的服務。

因此設定 UI / runtime 都要能處理：

```text
subscription expired
subscription quota exhausted
login expired
provider changed policy
CLI unavailable
runtime unavailable
```

並提供：

```text
Subscription
    ↓ unavailable
API Key
    ↓ unavailable
Ollama
```

這個 fallback 順序可以由使用者設定，而不是由模型自行決定。

---

### Multi-model：Task Routing 與 Fallback 必須分離

Subscription-first Chat 確立之後，Fleet 可以進一步支援多模型並用，但需要明確區分 **Task Routing** 與 **Fallback**。兩者都是模型選擇機制，但觸發條件不同。

#### Task Routing

Task Routing 是「在 request 開始前，根據任務特性決定使用哪個模型」，不是因為某個模型失敗才切換。

例如同時具備本地 Qwen 與 Claude Subscription：

```text
                    User Message
                         │
                    Task Router
                         │
              ┌──────────┴──────────┐
              │                     │
          Simple Task           Complex Task
              │                     │
           Local Qwen              Claude
              │                     │
              └──────────┬──────────┘
                         │
                    Final Response
```

第一版不需要額外呼叫一個 LLM 來判斷任務難度，可先使用 deterministic heuristic，例如：

- 一般聊天、簡單問答、簡單改寫 → Local
- coding / planning / research → Complex
- 需要 tool calls 或多步驟操作 → Complex
- context 很長 → Complex

之後再視實際需求加入小型 router model。

#### Fallback

Fallback 是「已經選定模型，但該模型無法正常使用時才切換」。

例如：

```text
Primary:
  Claude Subscription

Fallback:
  Qwen Local

正常：
  Claude → Response

Claude unavailable：
  Claude → Qwen → Response
```

可能觸發 fallback 的原因包括：

- authentication failure
- quota / subscription unavailable
- rate limit
- provider service error
- runtime unavailable

Fallback 不應該永久改變使用者下一輪的 primary model；下一個 request 仍應依照原本的 routing / model preference 再做選擇。

#### User 只有一個 Model 時

如果使用者只設定一個 provider，例如：

```text
Claude Subscription
```

則不需要強制要求設定 fallback：

```text
Simple Task  → Claude
Complex Task → Claude
Fallback     → None
```

如果 Fleet 本身偵測到本地 Ollama / Qwen 可用，則 Local 可以成為另一個可選 backend：

```text
Available:
  Qwen3 14B       Local
  Claude Sonnet   Subscription

Auto:
  Simple → Qwen
  Complex → Claude
```

但如果使用者明確指定某個 model，應尊重該選擇，不應因為 Fleet 自己認為另一個 model 更適合而偷偷切換。

#### 建議的使用者設定

```text
AI Models

Available
────────────────────────

● Claude Sonnet
  Anthropic · Subscription

● Qwen3 14B
  Local · Ollama


Default behavior
────────────────────────

○ Claude
  Always use Claude

● Auto
  Simple tasks → Local
  Complex tasks → Claude


Fallback
────────────────────────

Claude unavailable
  → Qwen3 14B

[Advanced]
```

因此 Fleet 的模型系統應將以下概念分開：

```text
Provider
Auth
Runtime
Model
Task Routing
Fallback
```

其中：

```text
Task Routing
  = 選擇「這一輪原本要用哪個 model」

Fallback
  = 原本選定的 model 失敗後，用什麼 model 備援
```

這種設計可以讓 Local / API Key / Subscription 同時存在，而 Persona、Memory、MCP 與 Agent Core 不需要知道模型是如何被選出的。

## 3. Agent Core：從 Chat 升級成可執行任務的核心

### 3.1 Chat 與 Agent 的區別

第一階段的 Chat 是：

```text
User
 ↓
LLM
 ↓
Text Response
```

後續應逐步變成：

```text
User
 ↓
Agent
 ↓
LLM
 ↓
Tool Call?
 ├── No  → Answer
 └── Yes
       ↓
    Execute Tool
       ↓
    Tool Result
       ↓
      LLM
       ↓
    Done / More Tools
```

這個 loop 才是後續所有 MCP、web search、檔案操作與 coding agent 的共同基礎。

### 3.2 Tool Calling

Ollama / Qwen3 可以收到 tool definitions 並產生 tool call，但：

> **模型本身不會替應用程式執行工具。**

Runtime 必須：

1. 將 tools 定義送給模型
2. 接收 `tool_calls`
3. 依 tool name / arguments 找到實際 handler
4. 執行工具
5. 將 tool result 加回 conversation
6. 再呼叫模型
7. 重複直到模型產生最終回答

概念：

```rust
loop {
    let response = model.chat(messages, tools).await?;

    if response.tool_calls.is_empty() {
        return response.content;
    }

    for call in response.tool_calls {
        let result = tools.execute(call).await?;
        messages.push(call.as_assistant_message());
        messages.push(result.as_tool_message());
    }
}
```

### 3.3 第一批 Tool

不要一開始接十幾個工具。

建議第一批：

1. `web_search`
2. `read_file`
3. `list_directory`
4. `run_command`（預設需要權限）
5. `get_system_context`

其中：

- web search：驗證 tool calling
- filesystem：建立 Agent 與本機的實際連結
- command：驗證「執行 → 觀察結果 → 再決策」
- system context：與桌寵原本的情境感知整合

### 3.4 MCP 的位置

MCP 應該是 Tool Layer 的標準介面，而不是 Agent 本身：

```text
Agent
  ↓
Tool Registry
  ├── Native Tool
  ├── MCP Tool
  └── Future Plugin
```

因此：

- Agent 不需要知道工具是 Rust function 還是 MCP server
- UI 不需要知道工具如何執行
- Permission layer 可以統一控制所有工具
- 未來接 OpenClaw 時，可以把 OpenClaw 視為另一個 Agent/Tool backend

### 3.5 Tool Permission

桌寵具備本機操作能力後，權限層必須獨立於模型。

建議：

```text
Tool
 ├── auto
 ├── confirm
 └── deny
```

初始建議：

| Tool                                | 預設           |
| ----------------------------------- | -------------- |
| get_time / system status            | auto           |
| web search                          | auto           |
| read project file                   | confirm        |
| write file                          | confirm        |
| run shell command                   | confirm        |
| delete file                         | deny / confirm |
| send message / external side effect | confirm        |

模型不能因為自己說「這是安全的」就繞過 permission layer。

---

## 4. Agent 如何區分簡單與複雜 Task

### 4.1 不建議一開始做「Simple / Medium / Complex」模型

Agent 不需要先猜一個抽象的複雜度分數。

更實用的問題是：

> **這個 task 需要哪些 capability？**

建議 TaskRequirements：

```rust
struct TaskRequirements {
    needs_web: bool,
    needs_filesystem: bool,
    needs_shell: bool,
    needs_code_edit: bool,
    needs_browser: bool,
    needs_long_running: bool,
    needs_multiple_steps: bool,
    needs_iteration: bool,
}
```

例如：

```text
「今天天氣如何？」
→ needs_web

「幫我看看這個 Rust project 的 unused dependency」
→ needs_filesystem + needs_shell

「幫我修這個 Rust bug，跑 test，失敗就繼續修」
→ needs_filesystem
 + needs_shell
 + needs_code_edit
 + needs_multiple_steps
 + needs_iteration
```

### 4.2 Capability escalation

推薦採用「能力不足才升級」而不是一開始就做複雜度預測。

```text
User
 ↓
Local Qwen3
 ↓
能直接回答？
 ├── Yes → Answer
 └── No
      ↓
   需要 tool？
      ├── Yes → MCP / Native Tool
      │            ↓
      │          完成？
      │          ├── Yes → Answer
      │          └── No → Escalate
      │
      └── Complex / Iterative
               ↓
         Codex / Claude Code
```

### 4.3 為什麼這比 Router LLM 更適合第一版

優點：

- 不需要額外支付一個 Router model
- 不需要維護第二套 prompt
- 判斷依據是 capability，而不是主觀的「複雜度」
- Agent 執行到一半才發現需要額外能力時，可以動態升級
- 方便加入新的 backend

第一版甚至可以完全用規則：

```rust
if task.needs_code_edit && task.needs_iteration {
    Backend::Codex
} else if task.needs_web || task.needs_filesystem {
    Backend::LocalWithTools
} else {
    Backend::Local
}
```

等實際使用後發現規則不足，再加入小模型 classifier。

### 4.4 不要把 reasoning 與 task complexity 混在一起

`think: true/false` 是模型推理模式，不等於 task routing。

例如：

```text
簡單問題
→ Qwen3 think=false

需要分析的本地問題
→ Qwen3 think=true

需要修改 repo + 執行 test
→ Codex / Claude Code
```

因此：

- **Reasoning mode**：控制目前模型如何思考
- **Tool calling**：控制模型能否使用外部能力
- **Escalation**：控制是否換到更強的 runtime

三者應獨立。

---

## 5. 情境感知（Context Awareness）

> 核心原則：**本地規則引擎決定「要不要講」，AI 只負責「怎麼講」。**

### 5.1 資料來源與抓取方式

| 資料來源                       | 建議作法                           | 平台差異 / 注意事項                                                                      |
| ------------------------------ | ---------------------------------- | ---------------------------------------------------------------------------------------- |
| 作用中視窗標題/程式名稱        | `active-win-pos-rs` 等跨平台 crate | Windows 用 `GetForegroundWindow`；macOS 需要輔助使用權限；Linux Wayland 目前沒有標準 API |
| CPU / 記憶體 / 電量 / 開機時間 | `sysinfo`                          | 三平台支援                                                                               |
| 閒置時間                       | 平台 API 或 `user-idle`            | 平台實作不同                                                                             |
| 剪貼簿內容                     | `arboard`                          | 隱私敏感度最高，預設關閉                                                                 |

### 5.2 規則引擎

1. 所有資料來源彙整成 `ContextSnapshot`
2. 定期輪詢
3. 規則決定是否觸發
4. 每條規則有 cooldown
5. 再加全域 cooldown
6. 規則觸發後才呼叫 AI 生成內容

### 5.3 Context 與 Agent 的關係

情境感知不要直接把整個 snapshot 塞給模型。

應該：

```text
Context Snapshot
      ↓
Rule Engine
      ↓
Event
      ↓
Relevant Context
      ↓
Persona + Prompt
      ↓
LLM
```

例如：

```text
Event:
user_worked_90_minutes_without_break

Context:
continuous_work_minutes = 90
```

而不是：

```json
{
  "cpu": 17,
  "memory": 42,
  "battery": 78,
  "active_window": "...",
  "idle": 0,
  ...
}
```

---

## 6. Persona / Memory / Knowledge

### 6.1 Persona 與 App 設定分離

Persona 是：

> 「牠是誰、怎麼說話」

App settings 是：

> 「牠能做什麼」

Persona 不應包含 tool enable/disable、web search 或 RAG 開關。

### 6.2 Persona 結構

```yaml
name: "雪絨"
personality: "有點傲嬌、怕冷、喜歡窩在角落，但其實很關心使用者"
speech_style: "講話簡短，句尾偶爾加語氣詞，不會用敬語"
boundaries:
  - "不主動提政治話題"
  - "被問到隱私問題會轉移話題"
few_shot_examples:
  - user: "在幹嘛"
    pet: "在看你打字啊，很無聊誒"
response_language: "auto"
```

### 6.3 三層記憶

| 層級          | 性質         | 管理方式           |
| ------------- | ------------ | ------------------ |
| 好感度/心情值 | 結構化數值   | Rust               |
| 近期互動記憶  | 短摘要       | AI + SQLite / file |
| 長期歷史記憶  | 大量語意資料 | 向量檢索           |

第一階段已完成後，**近期記憶摘要應優先於長期向量記憶**。

長期記憶等實際使用量證明需要後再加入。

### 6.4 RAG

RAG 仍適合：

- 遊戲角色設定
- 劇情知識
- 使用者自己的文件
- 大量長期記憶

但不要因為「Agent」就提前建立重型 vector DB。

優先：

```text
SQLite / JSON
   ↓
少量檢索
   ↓
prompt
```

等資料量真正成長後再升級。

---

## 7. OpenClaw / Codex / Claude Code 的定位

### 7.1 OpenClaw 不是 Model

OpenClaw 應理解成：

> Agent runtime / tool orchestration / provider integration layer

不是 LLM。

它可以搭配：

- 本地模型
- API key provider
- subscription-style provider
- MCP
- browser / shell / files 等工具

### 7.2 不建議讓 OpenClaw 成為 Fleet 核心

Fleet 應保留：

- Desktop Pet UI
- Persona
- Conversation
- Memory
- Permission
- Context Awareness
- 基本 Agent loop

OpenClaw 可以成為：

```text
Optional Advanced Agent Backend
```

架構：

```text
Fleet Snowfluff
      │
      ▼
   Agent Core
      │
 ┌────┼───────────────┐
 │    │               │
Local Tools      MCP / Tools     Advanced Backend
 │                                    │
Qwen3                         ┌────────┴────────┐
                              │                 │
                           OpenClaw           Direct
                              │              Codex/
                              │            Claude Code
```

### 7.3 何時交給 OpenClaw / Codex / Claude Code

適合 escalation 的條件：

- 多檔案修改
- 需要 shell + code edit + test 的迭代
- 長時間任務
- browser automation
- 多工具協作
- 需要完整 coding agent workflow
- 使用者明確要求「幫我完成」而不是「告訴我怎麼做」

不適合：

- 一般聊天
- Persona 對話
- 簡單問答
- 短摘要
- 簡單 web search

### 7.4 Chat 仍然可以作為 Agent 的入口

「Chat」與「Agent」不是兩套 UI。

推薦：

```text
User
 ↓
Chat
 ↓
Agent Core
 ├── answer directly
 ├── call tools
 ├── continue loop
 └── escalate to advanced runtime
```

因此使用者始終只是在跟雪絨聊天，但背後可以從 Qwen3 升級到 Codex / Claude Code。

---

## 8. 隱私與 Permission 設計

任何會讀取或修改本機資源的能力都應經過獨立 permission layer。

### 8.1 三層權限

```text
auto
confirm
deny
```

### 8.2 資料離開本機

UI 必須明確標示：

```text
Local:
Ollama / local files / local memory

Cloud:
OpenAI API / Anthropic API / subscription runtime
```

即使使用 subscription auth，也不能把它標示成「完全本地」。

### 8.3 Shell / File 操作

尤其要避免：

```text
LLM → 任意 shell
```

應該是：

```text
LLM
 ↓
Tool Call
 ↓
Permission Layer
 ↓
Policy
 ↓
User Confirmation
 ↓
Executor
```

---

## 10. 重新評估後的實作順序

> 第一階段已完成。依照新的產品優先級，**下一步先做 subscription-first Chat，再做 Agent Core**。

### 第二階段 — Subscription-first Chat

- Multi-model Task Routing / Fallback

**目標：讓使用者不用 API key，也能直接用自己已有的 AI subscription 與 Fleet 聊天。**

1. **Auth / Profile abstraction**
   - API Key
   - OAuth
   - CLI Session
   - Local

2. **OpenAI ChatGPT/Codex subscription**
   - OAuth login
   - OpenClaw-style auth profile
   - Codex app-server backend
   - canonical `openai/*` model route

3. **Anthropic Claude subscription**
   - Claude Code detection
   - `claude auth status`
   - Claude CLI backend
   - `claude -p`
   - 不自己保存 / refresh Claude 原生 login token

4. **Provider connection UI**
   - Connect ChatGPT
   - Connect Claude
   - API Key
   - Ollama

5. **Auth / runtime status**
   - connected
   - expired
   - quota exhausted
   - runtime unavailable
   - fallback backend

> 此階段完成後，使用者可以只登入 ChatGPT / Claude，就用 Fleet 的一般 Chat，不必先取得 API key。

### 第三階段 — Agent Core

**目標：讓 subscription-backed Chat 也可以逐步變成 Agent。**

6. **Tool abstraction**
7. **Tool registry**
8. **Agent loop**
9. **第一批 Native Tools**
   - system context
   - web search
   - read file
   - list directory
10. **Permission layer**
11. **iteration / timeout / cancellation**

### 第四階段 — MCP + Context

12. **MCP client**
13. **Context Awareness**
14. **Rule Engine**
15. **Emotion → Animation**
16. **Proactive interaction**

### 第五階段 — Memory / Knowledge

17. **好感度 / 心情值**
18. **近期互動記憶摘要**
19. **RAG**
20. **長期向量記憶（需求確認後）**

### 第六階段 — Capability Escalation

21. **Task capability detection**
22. **Local / MCP / Advanced backend routing**
23. **Codex session lifecycle**
24. **Claude Code session lifecycle**
25. **OpenClaw optional backend**

### 第七階段 — Voice / Automation

26. **TTS**
27. **STT**
28. **Browser automation**
29. **剪貼簿助手**
30. **多寵物互動**
31. **Plugin / Script system**

---

## 11. 下一階段的 MVP 定義

重新評估後，下一個可驗證版本不應以「更多功能」作為目標，而應驗證：

> **使用者是否願意把 Fleet 當成一個能幫忙做事的桌面 Agent。**

建議 MVP：

```text
Fleet Snowfluff
      │
      ▼
   Chat UI
      │
      ▼
   Qwen3/Ollama
      │
      ├── direct answer
      │
      └── tool call
            │
      ┌─────┼──────┐
      │     │      │
    search  file  system
      │     │      │
      └─────┼──────┘
            ↓
        tool result
            ↓
          Qwen3
            ↓
          answer
```

先不要同時加入：

- Codex
- Claude Code
- OpenClaw
- vector DB
- TTS
- STT
- browser automation

這些都建立在 Agent loop 穩定之後。

---

## 12. Agent MVP 完成條件

第二階段完成的判定不應是「tool call 可以成功一次」，而應包括：

- [ ] 模型可以選擇直接回答或呼叫 tool
- [ ] tool call 可以多輪執行
- [ ] tool result 能正確回到模型
- [ ] streaming 不會因 thinking / tool call 導致 UI 顯示空白
- [ ] 有最大 iteration 次數
- [ ] 有 timeout / cancellation
- [ ] tool argument 有 schema validation
- [ ] 高風險 tool 會要求使用者確認
- [ ] Agent 可以在一次任務中使用多個不同工具
- [ ] Agent 可以在工具失敗後重新決策
- [ ] 可以記錄一次 Agent run 的 tool trace
- [ ] 本地模型與雲端模型都能使用相同 Tool interface

---

## 13. 已知技術限制與風險

- **Wayland 視窗資訊讀取**：目前無標準 API，情境感知的視窗偵測需明確標示限制
- **本地模型的 Agent 能力**：小模型即使支援 tool calling，也可能產生錯誤 arguments、選錯工具或在多步任務中失去目標
- **Qwen3 thinking token budget**：thinking 與 final answer 共用生成 token budget；若設定過小，可能出現大量 thinking 後沒有足夠 token 產生最終回答
- **Tool execution 不是模型責任**：Ollama/Qwen3 產生 tool call 後，實際執行必須由 Fleet runtime 完成
- **MCP security**：MCP server 提供的工具可能具有檔案、網路或 shell 能力，必須套用 Fleet 自己的 permission policy
- **Shell / filesystem**：不得因為模型要求就直接執行任意命令，需 permission / confirmation
- **Agent loop runaway**：必須設定 iteration、timeout、token budget 與 cancellation
- **Subscription auth**：OpenAI Codex / Anthropic Claude Code 的 subscription 使用方式是 provider-specific，可能因 provider 政策改變；不可視為永久穩定的 API
- **Anthropic subscription**：目前 OpenClaw 文件記載 `claude -p` / Agent SDK / 第三方應用程式用量仍計入登入 subscription limits，但 Anthropic 可以在 OpenClaw 不更新的情況下修改規則；正式環境仍應考慮 API key
- **OpenAI subscription**：OpenClaw 目前透過 ChatGPT/Codex OAuth 與 native Codex app-server 使用 subscription，不等於一般 OpenAI API key
- **OpenClaw dependency**：若把 OpenClaw 作為核心 runtime，將增加版本、權限與 provider policy 的耦合；建議保持 optional backend
- **RAG 知識庫維護成本**：遊戲改版後需手動更新內容
- **長期記憶成本**：不要在需求未驗證前建立重型 vector DB
- **語音 pipeline**：TTS/STT 會增加資源、平台相容性與 UX 複雜度
- **角色 IP 使用邊界**：persona 檔案不可寫入官方逐字台詞，需自行改寫並附版權聲明

---

## 14. 最終架構方向

長期目標：

```text
                         Fleet Snowfluff
                              │
                     Desktop Pet + Chat
                              │
                         Agent Core
                              │
             ┌────────────────┼────────────────┐
             │                │                │
          Persona          Memory          Permission
             │                │                │
             └────────────────┼────────────────┘
                              │
                         Task Router
                              │
          ┌───────────────────┼────────────────────┐
          │                   │                    │
      Local Chat          Tool Agent         Advanced Agent
          │                   │                    │
       Qwen3              MCP / Native      ┌─────┴─────┐
          │                   │              │           │
       Ollama            Web / Files      Codex      Claude Code
                                             │           │
                                          OpenAI       Anthropic
                                          OAuth          CLI
                                             │
                                      OpenClaw (optional)
```

核心原則：

1. **Chat 是使用者介面，不是能力邊界。**
2. **Agent Core 是 Fleet 自己掌握的核心。**
3. **Tool / MCP 是能力擴充層。**
4. **Capability escalation 取代一開始的複雜度預測。**
5. **Local Qwen3 優先處理一般聊天與簡單工具任務。**
6. **只有需要多步、code edit、shell、browser 或長時間迭代時才升級。**
7. **Codex / Claude Code / OpenClaw 是 backend，不是 Fleet 的產品核心。**
8. **Provider、Auth、Runtime 三層分離，避免未來被 API Key 模式鎖死。**
9. **Permission 必須在模型之外，由 Rust runtime 強制執行。**
10. **先驗證 Agent 是否真的增加桌寵價值，再投入語音、長期記憶與大型 automation。**

---

### Multi-model Layer

Model selection should sit between Agent Core and the concrete Model Runtime：

```text
Agent Core
    │
    ▼
Model Router
    │
    ├── Task Routing
    │      ├── Simple → Local
    │      └── Complex → Subscription/API
    │
    └── Fallback
           └── Selected Runtime unavailable → fallback model
```

The Agent Core should remain independent of whether the selected runtime is Ollama, an API, Codex, or Claude Code.

## 15. 與原規劃相比的主要調整

原本第一階段完成後，下一步容易直接往「情緒、情境、RAG、語音」發展。

重新評估後，優先順序改成：

```text
已完成
Provider + Chat + Persona
        ↓
現在
Agent Loop + Tools + Permission
        ↓
接著
MCP + Context Awareness + Emotion
        ↓
再來
Memory + RAG
        ↓
之後
Capability Escalation
        ↓
最後
Codex / Claude Code / OpenClaw
        ↓
Voice / Browser / Automation
```

原因是：

> **Agent Loop 是所有後續「讓寵物幫你做事情」功能的共同基礎。**

如果沒有這一層，web search、MCP、Codex、Claude Code、OpenClaw 都只能變成彼此獨立的整合；有了這一層，Fleet 才真正擁有自己的 Agent architecture。
