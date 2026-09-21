# Fleet Snowfluff 功能規劃（v8）

> **v8 是在 v7 基礎上的 domain integration，不是 append；本版補齊跨 Execution / Runtime 的 Context lifecycle。**
>
> 本版把 **Conversation → Execution → Task Router → Runtime Adapter → SessionManager**
> 提升為核心 execution architecture，並重新安排 Provider / Subscription、Aemeath
> Agent、Tool / Permission、MCP、Context、Memory 與 External Runtime 的責任邊界。
>
> 本文件仍定位為後續 OpenSpec / implementation planning 的輸入，而不是最終 implementation specification。

## 0. Design Principles

### v9 新增：Sub-agent 原則

> **Sub-agent 是 Child Execution，不是另一套 Agent domain。**

Sub-agent 與 parent 共享 Aemeath Conversation 的語義 context，但擁有自己的 Task、
Execution lifecycle、Runtime 與 Session。Context Engine 負責受限 context projection；
DelegationManager 負責建立 / join / cancel child；Task Router 負責 child routing；
Permission / budget / depth policy 負責限制整棵 execution tree。

```text
Desktop Pet + Chat
       │
       ▼
 Conversation
       │
       ▼
  Execution
       │
       ▼
 Task Router
       │
       ├── Aemeath Agent Runtime
       │      └── Qwen / OpenAI API / Anthropic API
       │
       └── External Agent Runtime
              ├── Claude Code
              ├── Codex
              └── OpenClaw
```

核心原則：

1. **Conversation ≠ Execution ≠ External Runtime Session**
2. **Conversation Context 是跨 Execution / Runtime 的 canonical shared context**
3. **Session Context 是 runtime-owned state，不是 Aemeath Conversation 的替代品**
4. **Task Router route 的是 Task / Execution，不只是 Model**
5. **Router 可以參考 Conversation 與 Session，但不擁有它們**
6. **誰擁有 Agent Runtime，誰就擁有該 Execution 的 Agent Loop**
7. **Aemeath Agent Runtime 負責裸 Model / API 路徑的 Agent Loop**
8. **Claude Code / Codex / OpenClaw 保留自己的 Agent Loop**
9. **Tool capability ownership ≠ Agent Loop ownership**
10. **MCP 是 capability bridge，不是 Agent 本身**
11. **Task Routing 與 Fallback 必須分離**
12. **Capability escalation 優先於一開始建立抽象 complexity score**
13. **Permission 必須在模型之外強制執行**
14. **Provider / Auth / Runtime 必須解耦**

---

## 1. Product Scope 與功能方向

### 1.1 互動與陪伴

- 對話氣泡
- 情境吐槽
- 情緒系統
- 多寵物互動

### 1.2 生產力

- 番茄鐘 / 休息提醒
- 剪貼簿助手
- 系統通知代理
- 專案助手

專案助手包含讀取專案、搜尋程式碼、執行測試、協助修改檔案等 Agent task。

### 1.3 技術方向

- 語音輸入
- 外掛 / Script
- MCP 工具整合
- Codex / Claude Code / OpenClaw
- Browser / automation

---

## 2. Domain Foundation：Conversation / Execution / Session

### Conversation / Execution / External Runtime Session 分層

在 subscription-first 與 multi-model 之後，Aemeath
還需要明確區分三種「session / state」。它們不能被當成同一個概念：

> **Aemeath Conversation ≠ Aemeath Execution ≠ External Runtime Session**

#### 1. Aemeath Conversation

`Conversation` 是使用者看到的長生命週期對話，屬於 Aemeath product
layer。它負責：

- 使用者可見的聊天歷史
- Persona / system context
- Aemeath Memory 的關聯
- 使用者權限與設定
- 對話 metadata

Conversation 不應綁死某一個模型或 runtime。

同一個 Conversation 可以在不同時間使用不同 backend：

```text
Conversation conv_001
  ├── Execution ex_001 → Qwen / Ollama
  ├── Execution ex_002 → Claude Code
  └── Execution ex_003 → OpenClaw
```

#### 2. Aemeath Execution

`Execution` 代表「一次 task / turn 的執行」。它是 Aemeath Agent Core
的生命週期單位，負責記錄：

- `conversation_id`
- 本次選定的 model / provider / runtime
- TaskRequirements / routing decision
- execution status
- tool trace
- cancellation / timeout
- external runtime reference

例如：

```rust
struct Conversation {
    id: ConversationId,
}

struct Execution {
    id: ExecutionId,
    conversation_id: ConversationId,

    // Sub-agent / delegation relationship. None = root execution.
    parent_execution_id: Option<ExecutionId>,
    kind: ExecutionKind,

    runtime: RuntimeId,
    external_ref: Option<ExternalSessionRef>,
}

enum ExecutionKind {
    Root,
    SubAgent {
        role: SubAgentRole,
        depth: u32,
    },
}
```

**Stage 2 不需要先實作完整 Execution engine，但應先把這個 domain
boundary 定義出來。** Stage 3 Agent Loop 開始後，Agent run、tool
trace、timeout、cancellation 都應掛在 `Execution` 上，而不是直接掛在
Conversation。

#### 3. External Runtime Session

`External Runtime Session` 是 Codex、Claude Code、OpenClaw、ACP 等外部
runtime 自己管理的 session state。Aemeath
不應接管其內部生命週期，只保存必要的 reference 與狀態。

例如未來可能出現：

```text
Aemeath Conversation fs_123
  └── Aemeath Execution ex_456
       runtime = OpenClaw
       external_session_id = os_789
            └── ACP session acp_abc
                 └── Claude Code session cc_xyz
```

這種 nested session 不需要全部同步到 Aemeath。Aemeath 只需要知道：

```text
Execution
  → external runtime
  → external session reference
  → lifecycle/status
```

至於 OpenClaw 如何建立 ACP session、ACP 如何管理 Claude Code
session，應由對應 runtime adapter / runtime 自己負責。

#### 4. Conversation Context：跨 Execution / Runtime 的 canonical shared context

不同 Execution / Session **不應直接互相傳遞 session state**。Aemeath 應維持一份
product-level 的 `Conversation Context`，作為同一 Conversation 內不同
Execution 之間共享語義資訊的 canonical source。

```text
                         Conversation
                              │
                       Conversation Context
                              │
             ┌────────────────┼────────────────┐
             │                │                │
        Execution #1     Execution #2     Execution #3
             │                │                │
         Qwen/Ollama      Claude Code       Codex
             │                │                │
         Session A         Session B        Session C
```

因此：

> **Context sharing = semantic sharing through Aemeath Context，而不是
> session sharing。**

建議的核心資料模型：

```rust
struct ConversationContext {
    conversation_id: ConversationId,

    summary: Option<String>,
    recent_messages: Vec<Message>,

    facts: Vec<Fact>,
    decisions: Vec<Decision>,

    artifacts: Vec<ArtifactRef>,
    executions: Vec<ExecutionRef>,
}
```

其中：

- `summary`：長對話的壓縮背景，避免每次重新傳送全部 history
- `recent_messages`：保留近期原始對話，維持自然 conversational continuity
- `facts`：較穩定、可跨 Execution 使用的資訊
- `decisions`：使用者 / Agent 已做出的重要設計與行為決策
- `artifacts`：檔案、程式碼修改、產出物等可重新引用的資源
- `executions`：可追溯的歷史 Execution reference，而非把所有 tool trace
  永久塞進 prompt

特別建議把 `decisions` 獨立出來。對 Agent 而言，「之前決定了什麼」通常比
完整重播所有舊訊息更有價值。

#### 5. Execution Context：每次執行的 context snapshot

Execution 不直接持有整個 Conversation history，而是由 ContextManager
在執行前產生一次針對該 Task 的 context：

```rust
struct ExecutionContext {
    conversation: ConversationContext,

    task: Task,

    session: Option<SessionRef>,
}
```

這是一個 **execution-time snapshot / assembled context**，不是另一份長期
canonical state。

例如：

```text
Conversation Context
        │
        ▼
   Context Builder
        │
        ├── relevant messages
        ├── summary
        ├── decisions
        ├── facts
        ├── relevant artifacts
        └── current task
                │
                ▼
        Execution Context
                │
                ▼
        Runtime Adapter
```

這樣可以避免：

```text
10,000 message Conversation
        ↓
每個 Execution 都完整複製
```

而改成：

```text
10,000 message Conversation
        ↓
Context Store
        ↓
Relevant Context Selection
        ↓
Execution Context
```

#### 6. ContextManager：負責 context lifecycle，不負責 routing

建議加入獨立的 `ContextManager`，把「要帶什麼 context」和
「要去哪個 runtime」分開：

```rust
trait ContextManager {
    async fn build_context(
        &self,
        conversation_id: ConversationId,
        execution: &Execution,
    ) -> Result<ExecutionContext>;

    async fn record_execution(
        &self,
        execution: &Execution,
        result: &ExecutionResult,
    ) -> Result<()>;
}
```

責任：

```text
Task Router
    │
    │ 決定去哪裡
    ▼
ExecutionRoute
    │
    ▼
Execution
    │
    │ 決定帶什麼
    ▼
ContextManager
    │
    ├── Context Store
    ├── Context Builder
    ├── Memory
    ├── Execution History
    └── Artifact references
    │
    ▼
Runtime Adapter
```

因此三者的邊界為：

- **Task Router**：決定「去哪裡做」
- **ContextManager**：決定「要帶什麼過去」
- **SessionManager**：決定「是否建立 / 恢復哪個 runtime session」

這三個責任不要合併。

#### 7. ExecutionResult ingestion：Execution 完成後回寫 Conversation Context

Execution 結束後，不是把 runtime 的整個 session history 複製回 Aemeath，
而是由 Runtime Adapter 產生 Aemeath 可理解的 `ExecutionResult`：

```rust
struct ExecutionResult {
    output: Message,

    summary: Option<String>,

    facts: Vec<Fact>,

    decisions: Vec<Decision>,

    artifacts: Vec<ArtifactRef>,
}
```

然後：

```text
Runtime Session
      │
      ▼
ExecutionResult
      │
      ▼
ContextManager.record_execution()
      │
      ▼
Conversation Context
```

這讓不同 runtime 可以共享「結果與語義」，而不需要共享彼此的內部 session。

例如：

```text
Execution #1
Claude Code
「分析並修改 Task Router」
        │
        ▼
ExecutionResult
 ├── summary
 ├── decisions
 ├── artifacts
 └── output
        │
        ▼
Conversation Context
        │
        ├───────────────┐
        ▼               ▼
Execution #2       Execution #3
Qwen              Codex
```

因此下一個 Execution 可以知道前一個 Execution 做了什麼，即使它們：

- 使用不同 Model
- 使用不同 Provider
- 使用不同 Runtime
- 沒有共享 external session

#### 8. Runtime Session 與 Conversation Context 可以同時存在

當 Task Router 選擇 `Resume(SessionRef)` 時，Execution 可以同時取得：

```text
Aemeath Conversation Context
          +
Runtime Session Context
          │
          ▼
     Runtime Adapter
```

例如：

```text
Conversation #123
│
├── Execution #001 → Qwen / Ollama
│
├── Execution #002 → Claude Code / Session A
│
└── Execution #003 → Claude Code / Resume Session A
```

Execution #003 可以同時使用：

1. Aemeath Context：跨 Execution 的 shared semantic context
2. Claude Session A：Claude Code 自己維護的 native session state

兩者不能互相取代。

#### 9. Context ownership 原則

| Context                        | Owner               | 是否跨 Runtime |
| ------------------------------ | ------------------- | -------------- |
| Conversation history           | Aemeath             | Yes            |
| Conversation summary           | Aemeath             | Yes            |
| Facts / Decisions              | Aemeath             | Yes            |
| Artifact references            | Aemeath             | Yes            |
| Execution metadata             | Aemeath             | Yes            |
| Tool trace                     | Execution / Runtime | 可摘要後共享   |
| Native runtime session state   | External Runtime    | No             |
| Provider-specific hidden state | Provider / Runtime  | No             |

因此：

- Aemeath 擁有 **canonical product context**
- Runtime 擁有 **native execution context**
- ContextManager 負責兩者之間的 context assembly / ingestion
- 不要求 Aemeath 讀取或同步 external runtime 的全部內部 state

這個分層讓 Codex Thread / Turn、Claude Code session、OpenClaw session 都能
透過 adapter 映射，而不需要污染 Aemeath Core。

---

---

### 10. Context Engine：把 ContextManager 升級成可組裝、壓縮與委派的 context lifecycle

前面的 `ContextManager` 已經定義了 context 的 ownership 與 ingestion。
隨著長對話、Memory、MCP、Sub-agent 出現，建議把它在概念上提升成
**Context Engine**。第一版不必新增完全不同的 Rust trait；可以直接由
`ContextManager` 實作，之後再視需求抽象。

```text
Conversation
    │
    ▼
Context Engine
    ├── Context Store
    ├── Context Builder
    ├── Context Selection
    ├── Compaction
    ├── Memory Retrieval
    ├── Execution Ingestion
    └── Sub-agent Context Projection
    │
    ▼
Execution / Sub-agent Execution
    │
    ▼
Runtime Session
```

核心原則：**Context Engine 決定這次 execution 應該看到什麼；Runtime Session
決定 runtime 自己記得什麼。**

> **實作時機：Context Engine 不整個放在單一 Stage，而是依能力分三段落地
> （見第 13 節）：**
>
> - **Stage 3**：`ContextManager` 基礎版——`build_context` / `record_execution`，
>   讓 Agent Loop 有東西可以組裝；不含 compaction、memory retrieval、sub-agent
>   projection。單一 Execution 就用得到，不需要等 delegation 或 memory 先存在。
> - **Stage 5**：加上 §10.1 的 Sub-agent Context Projection（`SubAgentContextSpec`、
>   `build_sub_agent_context`）——這一段邏輯上就是為 delegation 存在的，Stage 5
>   之前沒有 child execution 可以投影 context 給。
> - **Stage 7**：Compaction 與 Memory Retrieval，並視需求把 `ContextManager`
>   升級成 §10.3 的 `ContextEngine` trait——這兩者本質上是長期記憶／長對話摘要
>   問題，跟 Stage 7 本來就要做的近期記憶摘要、RAG 是同一類工作，沒有理由切成
>   兩個階段分別做。
>
> 不要在 Stage 3 就把完整 `ContextEngine`（含 compaction/retrieval/projection）
> 一次做完——那些能力分別依賴 delegation（Stage 5）與長期記憶（Stage 7）才有
> 真正的使用情境，提早做只會產生沒有真實需求驗證過的介面猜測。

#### 10.1 Context Projection：Parent → Sub-agent

Sub-agent 的 context 應是 parent execution context 的**受限 projection**，而不是完整 transcript copy。

```rust
struct SubAgentContextSpec {
    task: Task,
    include_summary: bool,
    include_recent_messages: bool,
    include_facts: Vec<FactId>,
    include_decisions: Vec<DecisionId>,
    include_artifacts: Vec<ArtifactRef>,
    parent_result_ref: Option<ExecutionResultRef>,
}
```

預設策略：不傳 parent 完整 tool trace；不傳整個 Conversation history；只傳
完成 task 所需要的 facts / decisions / artifacts。若 child 需要 parent 的
中間結果，使用 `parent_result_ref` 明確引用。Child 完成後只將
`summary / facts / decisions / artifacts / output` 回傳給 parent。

#### 10.2 Compaction 與 Sub-agent 不直接耦合

Sub-agent 只是 Context Engine 的 execution consumer；不要讓每個 child 自己建立
一套 memory / compaction，否則會產生多份互相不一致的 context。

```text
Large history
    │
    ├── recent messages
    ├── summary
    ├── facts / decisions
    └── memory retrieval
             │
             ▼
      execution context
```

#### 10.3 Context Engine API

第一版保留現有 `ContextManager` trait，增加 delegation-aware API：

```rust
trait ContextManager {
    async fn build_context(
        &self,
        conversation_id: ConversationId,
        execution: &Execution,
    ) -> Result<ExecutionContext>;

    async fn build_sub_agent_context(
        &self,
        parent: &Execution,
        child: &Execution,
        spec: &SubAgentContextSpec,
    ) -> Result<ExecutionContext>;

    async fn record_execution(
        &self,
        execution: &Execution,
        result: &ExecutionResult,
    ) -> Result<()>;
}
```

之後若需要 plugin-based context engine，再提升成：

```rust
trait ContextEngine {
    async fn assemble(&self, request: ContextRequest) -> Result<ExecutionContext>;
    async fn ingest(&self, result: ExecutionResult) -> Result<()>;
    async fn compact(&self, target: ContextTarget) -> Result<CompactionResult>;
}
```

因此 v9 不要求一次重構兩個 trait；`ContextManager` 可以先作為 Context Engine 的實作介面。

---

## 3. Provider / Auth / Model / Runtime Foundation

### 3.1 核心設計：Provider 抽象層

第一階段已完成 Provider 抽象層與聊天能力。原本以：

```rust
trait AiProvider {
    async fn chat(&self, messages: Vec<Message>) -> Result<Response>;
}
```

為核心。

後續不應把 Provider 抽象成「一個 HTTP API
endpoint」，而應逐步拆成三個概念：

1.  **Model Provider**：模型來自哪裡，例如 OpenAI、Anthropic、Ollama。
2.  **Auth Method**：如何取得使用權限，例如 API Key、OAuth、CLI
    Session、Local。
3.  **Execution Backend / Runtime**：由誰執行這次 Agent turn，例如直接
    API、Ollama、Codex、Claude Code、OpenClaw。

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

不要求現在立刻完整實作所有 enum；重點是不要把架構鎖死在
`API key -> chat()`。

### 3.2 Provider 實作

第一階段已有：

- `OpenAiCompatible`：可指向 OpenAI / Anthropic API，或任何 OpenAI
  相容端點
- `Ollama`：本地 HTTP server
- `Embedded`（未來選配）：用 Rust 原生的 candle 直接跑模型

重新評估後：

- **Ollama 應提升優先級**：因為它是後續本地 Agent / Tool Calling
  的主要基礎。
- **Embedded 暫時維持低優先級**：除非需要完全免安裝、單一
  binary，否則先不要承擔模型打包與跨平台硬體相容性的成本。
- **OpenAI / Anthropic API 仍保留**：適合一般雲端聊天、web
  search、需要較強模型的任務。
- **Codex / Claude Code 不應直接塞進 `OpenAiCompatible`**：它們屬於
  Agent runtime / CLI backend，而不是單純 API endpoint。

### 3.3 Auth / Provider / Runtime 分層

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

### 3.4 Subscription auth 的定位

**本專案重新評估後，subscription auth 提升為第一優先。**

第一階段目前已能以 API key / Ollama 進行一般
Chat，但如果產品目標是讓使用者把 Aemeath 當成自己的桌面 AI
助手，則不應要求使用者為已經持有的 ChatGPT / Claude subscription
再建立一組獨立 API billing。

因此下一階段的第一個目標應是：

> **先讓一般 Chat 可以透過 provider 的 subscription / OAuth / CLI auth
> 使用 AI，再往 Agent / Tool Calling 擴充。**

Subscription 不應理解成「把 ChatGPT / Claude subscription 轉成一般
API」。正確做法是使用 provider 自己支援的 subscription execution path。

目前 OpenClaw 採取的是 provider-specific 路徑：

- **OpenAI / Codex**：ChatGPT/Codex OAuth + native Codex
  app-server。OpenClaw 目前使用 canonical `openai/*` model route，並由
  runtime 選擇 Codex app-server；subscription credential 與 API-key
  credential 是不同 auth profile。citeturn0search0turn0search3
- **Anthropic / Claude Code**：使用已登入的 Claude Code CLI，透過
  `claude -p` / Agent SDK 類程式化路徑執行；OpenClaw
  目前文件記載此用量會計入登入帳號的 subscription limits，而且 Claude
  Code 自己管理 login / token refresh。citeturn0search1
- **API Key**：仍是一般 Platform API 的 usage-based billing，與
  subscription quota 分開。
- **其他 provider**：若支援 Coding Plan / CLI OAuth / subscription
  auth，也應視為 provider-specific execution
  backend，不應抽象成通用「OAuth 就能用 subscription」。OpenClaw
  目前也列出 Qwen Cloud、MiniMax、Z.AI/GLM 等 subscription-style
  選項。citeturn0search2

因此 Aemeath 應逐步形成：

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

**重要：subscription 路徑不是通用保證。Provider 可以修改計費、rate limit
或第三方使用政策，因此應把 subscription integration 做成 provider
adapter，而不是把它寫死在一般 `AiProvider::chat()` 裡。** Anthropic
官方/相關文件尤其明確提醒 billing 與 rate-limit
行為可能變動。citeturn0search1

### 3.5 整體資料流

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

---

## 4. Task Router：Execution 的 Routing Decision

Task Router 不應只是「選模型」的元件。它負責的是：

> 根據這一次 Task 的需求、可用 capability、使用者偏好與目前可用 Runtime，決定這個 Execution 應該交給哪個 Runtime，以及是否需要建立或恢復 external session。

但 Router **不直接管理 Conversation，也不直接操作 Session lifecycle**。

```text
Conversation
    │
    └── User Message
            │
            ▼
        Execution
            │
            ▼
        Task Router
            │
            ├── Aemeath Agent Runtime
            │      └── Qwen / OpenAI API / Anthropic API
            │
            └── External Agent Runtime
                   ├── Claude Code
                   ├── Codex
                   └── OpenClaw
                          │
                          ▼
                   External Session
```

### 4.1 RoutingContext

```rust
struct RoutingContext {
    conversation_id: ConversationId,
    message: String,

    // ContextManager provides the relevant Aemeath context.
    // Router reads it but does not own or assemble canonical context.
    conversation_context: ConversationContext,

    available_models: Vec<Model>,
    available_runtimes: Vec<Runtime>,
    capabilities: Vec<Capability>,

    user_preferences: RoutingPreferences,
}
```

`conversation_context` 可以包含 relevant conversation history、Persona、Memory
與 current task。Router 可以讀取這些資訊，但不負責重新組裝完整 model prompt。

### 4.2 TaskRequirements：先判斷 Capability，不先猜 Complexity

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

第一版不需要建立 `Simple / Medium / Complex` score。先回答：

> 這個 Task 需要哪些 capability？

### 4.3 ExecutionRoute：Router 的真正輸出

Router 不應只回傳 `ModelId`。

```rust
enum SessionStrategy {
    None,
    Create,
    Resume(SessionRef),
}

struct ExecutionRoute {
    runtime: RuntimeId,
    provider: ProviderId,
    model: ModelId,

    session_strategy: SessionStrategy,
}
```

若需要更完整的 audit / routing explanation：

```rust
struct ExecutionRoute {
    runtime: RuntimeId,
    provider: ProviderId,
    model: ModelId,

    capabilities: Vec<Capability>,
    session_strategy: SessionStrategy,

    reason: RoutingReason,
}
```

Route 描述的是「這一次 Execution 要怎麼執行」，而不是單純「使用哪個模型」。

### 4.4 Execution 保存 Routing Decision

```rust
struct Execution {
    id: ExecutionId,
    conversation_id: ConversationId,

    task: Task,
    route: ExecutionRoute,

    runtime: RuntimeId,
    session: Option<SessionRef>,

    status: ExecutionStatus,
}
```

Execution 必須保存本次實際 route，避免未來 routing policy 改變後無法重建歷史。

### 4.5 Context Management：跨 Execution 的共享 context

Task Router 得到的 `RoutingContext` 應由 ContextManager / Context Builder 提供，
而不是 Router 自己從 database 拼裝。

```text
Conversation
    │
    ▼
Context Store
    │
    ▼
Context Builder
    │
    ▼
RoutingContext
    │
    ▼
Task Router
```

Routing 完成後：

```text
ExecutionRoute
    │
    ▼
Execution
    │
    ▼
ContextManager.build_context()
    │
    ▼
ExecutionContext
    │
    ▼
Runtime Adapter
```

Execution 完成後：

```text
Runtime Adapter
    │
    ▼
ExecutionResult
    │
    ▼
ContextManager.record_execution()
    │
    ├── summary
    ├── facts
    ├── decisions
    └── artifacts
    │
    ▼
Conversation Context
```

這形成完整的 context lifecycle：

> **Load → Assemble → Execute → Summarize → Ingest**

### 4.6 跨 Runtime context sharing 範例

```text
Conversation #123
│
├── Execution #001
│     Runtime = Qwen / Ollama
│     Result = analysis
│              │
│              ▼
│        Conversation Context
│
├── Execution #002
│     Runtime = Claude Code
│     Session = A
│     Context = relevant Conversation Context
│     Result = implementation
│              │
│              ▼
│        Conversation Context
│
└── Execution #003
      Runtime = Codex
      Session = B
      Context = analysis + implementation summary + artifacts
```

Execution #003 不需要存取 Session A 或 Session B 的完整 history；只需要
ContextManager 選出的 relevant Aemeath context。

### 4.5 TaskRouter Trait

```rust
trait TaskRouter {
    async fn route(
        &self,
        context: RoutingContext,
    ) -> Result<ExecutionRoute>;
}
```

Router 的責任到 `ExecutionRoute` 為止。

不要把以下責任塞進 Router：

```rust
route_and_create_session(...)
route_and_resume_session(...)
route_and_execute(...)
```

正確的責任鏈：

```text
Task Router
     │
     ▼
ExecutionRoute
     │
     ▼
Execution
     │
     ▼
ContextManager
     │
     ▼
Runtime Adapter
     │
     ▼
SessionManager
     │
     ▼
Runtime
```

### 4.6 SessionStrategy：Router 決定，Runtime 執行

假設同一 Conversation 之前已經有 Claude Code session：

```text
Conversation conv_123

previous execution:
    runtime = claude_code
    session = cc_456

User:
「剛才那個 Rust bug 再幫我修一下」

Router:
    runtime = Claude Code
    session_strategy = Resume(cc_456)
```

第一次執行則可能是：

```text
SessionStrategy::Create
```

單純不需要 external session 的 Aemeath execution：

```text
SessionStrategy::None
```

真正的 create / resume / terminate 必須由 Runtime Adapter / SessionManager 完成。

### 4.7 Conversation 可以跨 Runtime

```text
Conversation #123
├── Execution #001 → Qwen / Ollama
├── Execution #002 → Claude Code → Session A
└── Execution #003 → Codex → Session B
```

因此：

```text
Conversation ≠ Execution ≠ Runtime Session
```

Conversation 是產品層的 continuity；Execution 是一次 task/run；Runtime Session
是某個 external runtime 自己維持的 execution context。

### 4.8 Task Routing 與 Fallback 分離

Task Routing：

```text
User Message
     │
 Task Router
     │
 ┌───┴────────────┐
 │                │
Simple           Complex
 │                │
Local Qwen       Claude / Codex
```

Fallback：

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

Fallback 可因 authentication failure、quota、rate limit、provider error 或
runtime unavailable 觸發。

Fallback 不永久改變下一輪 primary route。

使用者明確指定 model 時，不應偷偷切換；若產品允許 fallback，也應明確顯示
fallback 發生。

### 4.9 第一版使用 Deterministic Heuristic

```rust
if task.needs_code_edit && task.needs_iteration {
    Backend::Codex
} else if task.needs_web || task.needs_filesystem {
    Backend::LocalWithTools
} else {
    Backend::Local
}
```

先不用額外 Router LLM，避免額外成本與第二套 prompt；實際使用後再決定是否需要
classifier / routing model。

### 4.10 Capability Escalation

Routing 是 Execution 開始前的 initial decision；能力不足時再 escalation：

```text
User
 ↓
Task Router
 ↓
Local Qwen3
 ↓
能完成？
 ├── Yes → Answer
 └── No
      ↓
   需要 tool？
      ├── Yes → Native / MCP Tool
      │            ↓
      │          完成？
      │          ├── Yes → Answer
      │          └── No → Escalate
      │
      └── Complex / Iterative
               ↓
        Codex / Claude Code / OpenClaw
```

> **Task Routing = initial route selection；Capability Escalation = execution-time upgrade。**

因此「一開始選哪個 Runtime」與「執行途中能力不足而升級」是兩個不同概念。

### 4.11 Reasoning 不等於 Routing

```text
簡單問題
→ Qwen3 think=false

需要分析的本地問題
→ Qwen3 think=true

需要修改 repo + 執行 test
→ Codex / Claude Code
```

因此：

```text
Reasoning Mode
= 目前模型如何思考

Tool Calling
= 模型能否使用外部能力

Task Routing / Escalation
= 是否需要改變 runtime / backend
```

### 4.12 Router 不擁有 Conversation 或 Session

```text
Conversation
    │
    ▼
Execution
    │
    ▼
Task Router
    │
    ▼
ExecutionRoute
    │
    ▼
Runtime Adapter
    │
    ├── AemeathAgentRuntime
    │
    └── ExternalRuntime
             │
             ▼
        SessionManager
```

Router 可以參考 Conversation history 與 session reference，但不擁有它們的 lifecycle。

### 4.13 Task Router Mode：single / mix（第一版落地設計）

第 4.9 節的 deterministic heuristic 是概念性草圖；這裡是 Stage 3 要實際實作、
綁定既有 provider 設定的版本。

#### `mode` 設定

延續 Stage 2 已經有的 `AiSettings.default_profile`，新增一個 `mode` 設定：

```rust
enum TaskRouterMode {
    /// 目前 Stage 1/2 的行為：不管 task 難度，永遠用 default_profile。
    Single,
    /// simple task 用本機模型，complex task 用 default_profile；
    /// 本機模型不可用時 fallback 回 default_profile。
    Mix,
}

struct AiSettings {
    ai_enabled: bool,
    enabled_profiles: Vec<ProviderProfile>,
    default_profile: Option<ProfileKey>,
    acknowledged_disclosures: Vec<ProfileKey>,
    task_router_mode: TaskRouterMode, // 新增，預設 Single
}
```

`task_router_mode` 預設 `Single`——升級後行為不變，跟 Stage 2 `sanitize`
既有的「欄位缺失就取預設值、不強迫重新設定」原則一致。

#### single：不路由，永遠用 default_profile

```text
Task（簡單或複雜都一樣）
    │
    ▼
default_profile
```

等同今天已經有的行為：`TaskRouter` 在 `single` 模式下把 `RoutingContext`
原封不動轉成 `ExecutionRoute { profile: default_profile }`，不檢查
`TaskRequirements`。

#### mix：先問本機模型，本機模型自己判斷 simple / complex

**刻意不用 keyword/rule-based 打分（討論過但放棄的替代方案），也不用預先算好
`TaskRequirements` 再決定——關鍵字比對在 5 個 UI locale 下要維護 5 套片語
清單，還是很容易漏判自然語言的各種講法；獨立打分制又會變成 §4.2 / principle
12 明確反對的「一開始就建 complexity score」。改成讓本機模型自己讀規則、用它
本來就有的語言理解能力去判斷，不需要另外維護任何關鍵字表，也不必為此另外
發一次請求——判斷本身就是它要生成的回覆的一部分。**

機制：`mix` 模式下，訊息先送給本機模型（Ollama），system prompt 除了 persona
之外，額外附加一份人工維護的規則文件（見下方）。本機模型依規則判斷：

- **simple** → 照平常一樣，以 persona 正常回覆，使用者看到的行為完全不變
- **complex** → 整個回覆只回傳一個固定的 escalation marker（不夾雜任何
  persona 內容），Router 偵測到後丟棄這次回應，把同一則訊息改送
  `default_profile`（system prompt 只有 persona，不含規則文件——
  `default_profile` 不需要再判斷一次）

```text
Task（mix 模式）
    │
    ▼
本機模型（Ollama）
system prompt = persona + task-router-rules.md
    │
    ▼
串流的第一段內容
    │
    ├── 符合 escalation marker？
    │      │
    │      ├── 是 → 丟棄這次回應，同一則訊息改送 default_profile
    │      │        （system prompt 只有 persona）
    │      │
    │      └── 否 → 正常轉發給使用者，繼續串流
```

這跟 `claude_code_cli.rs` 現有的「偷看第一行、判斷是不是 resume 失敗，失敗
就用不帶 `--resume` 的參數重跑一次」是同一種手法（peek-then-conditionally-
retry），只是這裡用在 Router 層，不是單一 provider 內部。

「本機模型」指 `enabled_profiles` 裡 `provider == Ollama` 的那一個
profile——Aemeath 的設定 UI 本來就只有一個 Ollama slot（見 `settings-ui` 的
`PROFILE_SLOTS`），不需要再加一個獨立的「local_profile」指標欄位去指定是哪一個。

`TaskRequirements` 仍然保留為 domain type（見 Stage 3 roadmap），但這個
mix-mode 的判斷路徑不消費它——它是本機模型自己的語意判斷，不是套用預先算好
的 capability flags。`TaskRequirements` 留給 Stage 4 Capability Escalation
用得到的時候再實際填值。

#### Rules 檔案：`personas/task-router-rules.md`

跟 `personas/aemeath.yaml`（persona）一樣，是人工維護、放在檔案系統上的純文字
文件，用同一套載入機制讀進來（`persona_store::load()` 的同類作法），附加到
本機模型的 system prompt——只給本機模型看，`default_profile` 不需要。

格式範例（實際規則後續再細化，這裡先定調格式與 marker）：

```markdown
# Task Router Rules

判斷這個使用者訊息是否為「複雜任務」。如果是複雜任務，你的整個回覆必須
「只」包含以下這個 token，不要有任何其他文字：

<<ESCALATE>>

複雜任務的判斷依據：

- 需要讀寫檔案、執行 shell 指令、修改程式碼
- 需要瀏覽器操作
- 需要多步驟、反覆執行到成功為止
- 需要長時間執行或持續監控

如果不符合以上任何一項，就是簡單任務——正常以角色 persona 回覆使用者，不要
提到這份規則或 escalation 機制本身。
```

#### Escalation marker 偵測：比對開頭，不要求完全相等

本機模型不一定每次都嚴格照格式輸出（可能夾雜空白或贅字），用「開頭符合」
比對比較穩，不要求整段完全相等：

```rust
fn looks_like_escalation(chunk: &str) -> bool {
    chunk.trim_start().starts_with("<<ESCALATE>>")
}
```

誤判的後果不對稱，這也是這個設計可以接受「不用做到 100% 準確」的原因：

- **該 escalate 卻沒偵測到**（本機模型判斷錯誤，正常回覆了一個其實很複雜的
  任務）→ 使用者拿到一個品質較差的回答，不是系統性失敗
- **不該 escalate 卻誤判**（本機模型多加了不必要的 marker）→ 訊息改送
  `default_profile`，使用者完全不會發現，只是多花一點 API 用量

兩種誤判都不會讓對話失敗，只會讓路由決策比理想情況差一點，不需要把偵測邏輯
做到完美。

#### Fallback：infra 層與內容層，殊途同歸

- **infra 層**（跟本機模型的判斷完全無關）：Ollama 沒有被啟用、
  `check_availability()` 回報 runtime unavailable（沒裝 / 沒跑）、執行中途
  本機模型連線錯誤
- **內容層**（本機模型自己判斷）：本機模型回傳 escalation marker

兩種情況結果相同——同一則訊息改送 `default_profile`。`complex` 路徑本身沒有
第二層 fallback：如果 `default_profile` 本身不可用，那是既有「Provider
status display」的連線失敗狀態，不屬於 Task Router 的職責。

#### 已知的延遲成本

simple 任務的延遲跟現在 `single` 模式幾乎一樣——使用者直接看到本機模型的
第一段輸出。complex 任務則會先付出一次本機模型產生 escalation marker 的往返
時間，才輪到 `default_profile` 開始作答；只要規則檔要求「只回傳 marker、
不要有其他內容」，這次往返應該很短，但不是零成本。

#### 為什麼不現在就用完整 `ExecutionRoute { capabilities, reason }`

`single` / `mix` 兩種模式都只需要 §4.3 第一版的最小 `ExecutionRoute`
（`runtime` / `provider` / `model` / `session_strategy`），不需要
`capabilities` 或 `RoutingReason` 欄位——那些是給未來更多 profile、更多
runtime 同時存在時才有意義的 audit 資訊。目前只有「本機模型」與
「default_profile」兩個選項可選，沒有東西需要審計。

---

## 5. Subscription-first Chat

### 5.1 目標

第一階段已有一般 Chat。下一步先讓使用者能使用自己已有的 AI subscription，
再往 Agent / Tool Calling 擴充。

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

### 5.2 OpenAI / ChatGPT-Codex

```text
Aemeath Chat
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

不要把 OAuth token 當成一般 API key 使用；subscription path 應走 provider-specific
runtime。

### 5.3 Anthropic / Claude

```text
Aemeath Chat
   ↓
Anthropic Provider
   ↓
Claude Code CLI
   ↓
claude -p
   ↓
Claude subscription
```

Claude Code 負責自身 login / token lifecycle；Aemeath 不應複製 provider 原生 credential
management。

### 5.4 Subscription integration 原則

> **優先重用 provider 官方 CLI / SDK / app-server，而不是自行逆向 OAuth。**

### 5.5 Subscription fallback

```text
Subscription
    ↓ unavailable
API Key
    ↓ unavailable
Ollama
```

需要處理：

```text
subscription expired
subscription quota exhausted
login expired
provider changed policy
CLI unavailable
runtime unavailable
```

Fallback 是使用者 / product policy，不是模型自行決定。

---

## 6. Aemeath Agent Runtime

### 6.1 Chat 與 Agent 的區別

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

這個 loop 才是後續所有 MCP、web search、檔案操作與 coding agent
的共同基礎。

### 6.2 Tool Calling

Ollama / Qwen3 可以收到 tool definitions 並產生 tool call，但：

> **模型本身不會替應用程式執行工具。**

Runtime 必須：

1.  將 tools 定義送給模型
2.  接收 `tool_calls`
3.  依 tool name / arguments 找到實際 handler
4.  執行工具
5.  將 tool result 加回 conversation
6.  再呼叫模型
7.  重複直到模型產生最終回答

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

### 6.3 第一批 Tool

不要一開始接十幾個工具。

建議第一批：

1.  `web_search`
2.  `read_file`
3.  `list_directory`
4.  `run_command`（預設需要權限）
5.  `get_system_context`

其中：

- web search：驗證 tool calling
- filesystem：建立 Agent 與本機的實際連結
- command：驗證「執行 → 觀察結果 → 再決策」
- system context：與桌寵原本的情境感知整合

#### `web_search` 的實作範圍：只服務 Ollama 的 tool-calling，不是 Aemeath 的通用搜尋功能

**這個 `web_search` native tool 只在 `ToolCallingProvider`（v1 只有 Ollama）
的 Agent Loop 裡用得到。** Claude Code 跟 Codex 各自已經有第一方的 web
search（Anthropic/OpenAI 自己的 server-side 服務，綁在既有 subscription/
API session，不需要另外的 key），走的是 §6.9 的 native tool allow-list，
不透過這個 Tool Registry——所以這裡要做的東西範圍比「Aemeath 的網路搜尋
功能」小得多，只是「讓本機模型也能查資料」。

要求：免費、不需要登入/帳號（比對過 Brave Search API——2026 年初起免費方案
被砍，改成新用戶必須綁信用卡才能拿到額度，不符合這個門檻）。參考 OpenClaw
自己支援的 12 個 web search provider：它把 DuckDuckGo 跟 SearXNG 排在
優先序最後——也就是「不用設定就能動」的保底選項，不是首選，但確實是這個
生態圈裡已經在用的真實做法，不是憑空拼湊。

落地設計：兩層 fallback，內部自己試，不對使用者暴露「選 provider」的設定：

```text
web_search(query)
    │
    ▼
嘗試 public SearXNG instance（真正的排序搜尋結果，但是非官方、
無 SLA 的第三方基礎設施，可能隨時掛掉或擋流量）
    │
    ├── 成功 → 回傳結果
    │
    └── 失敗/連不上
          ▼
       嘗試 DuckDuckGo Instant Answer API
       （官方、免費、不用 key，但只有 infobox/定義/消歧義，
       不是完整搜尋結果，很多一般問題查不到東西）
          │
          ├── 成功 → 回傳結果
          │
          └── 失敗/沒有結果 → 老實回傳「沒有找到搜尋結果」，
                               讓模型自己決定怎麼回應，不要假裝
                               有結果或讓整個 turn 失敗
```

兩層都不需要 API key、不需要登入，符合門檻；SearXNG 顧結果品質，
DuckDuckGo IA 顧「至少永遠有一個官方、穩定的東西可以退回去」。

#### `read_file` / `list_directory`：project root 是 auto 範圍，範圍外用彈出視窗升級

桌寵是以登入使用者的完整權限跑的背景程式，而且是被動彈出來聊天，跟使用者
主動叫出來、指定在某個專案資料夾裡工作的 Claude Code 不是同一種信任情境。
不加範圍限制的話，模型理論上可以要求讀 `~/.ssh/id_rsa`、瀏覽器 cookie
儲存區、硬碟上任何檔案——對一個聊天陪伴 App 來說不是合理的預設。

```rust
struct AiSettings {
    // ...
    project_root: Option<PathBuf>, // 全域一份，不是 per-conversation——
                                    // Fleet 現在本來就只有一個 conversation，
                                    // 之後真的需要多 conversation 各自
                                    // 綁定不同專案時再重新考慮
}
```

`project_root`（使用者透過原生資料夾選擇對話框指定）是 auto 範圍，不是
硬性邊界——範圍外的請求不會直接被拒絕，而是升級成 §6.11 那個批次確認
彈出視窗，讓使用者看到實際請求的路徑，自己決定要不要允許：

```text
read_file(path) / list_directory(path)
    │
    ▼
project_root 有設定，而且 path canonicalize 後真的落在裡面？
（不能只做字串前綴比對——否則 `../../etc/passwd` 或指向外部的
symlink 會繞過去）
    │
    ├── 是 → auto，不彈視窗（今天的快速路徑）
    │
    └── 否（範圍外，或根本沒設定 project_root）
              │
              ▼
        跟 §6.11 同一個批次確認彈出視窗，顯示實際請求的路徑
              │
              ├── 使用者允許 → 讀這一次
              └── 使用者拒絕 → tool result：「使用者拒絕存取」
```

沒設定 `project_root` 時，等於「範圍」是空集合——每一次請求都會走彈出
視窗那條路，功能一開始就能用（只是每次都要問），設定了 `project_root`
之後才會變得比較不煩人。

**「這個 session 都允許」對檔案類 tool 要按路徑記，不是按 tool 名稱。**
§6.11 原本的「記住」是整個 tool 名稱（例如「以後都允許 `read_file`」）——
對範圍外的路徑來說太粗了，核准一個資料夾不應該等於這個 session 剩下的
時間都信任任何路徑，那樣就失去設 `project_root` 的意義了。檔案類 tool
的 session 記憶應該記 `(tool_name, path)` 這一組，等於把這個路徑暫時
併入這個 session 的信任範圍，一次一個資料夾地擴大，不是整個工具一次
全部信任。沒有路徑維度的 tool（`web_search`、`get_system_context`）維持
原本按 tool 名稱記的做法。

`run_command` 的工作目錄仍然**硬性**綁在 `project_root`，不套用這個
「彈出視窗升級」機制——shell 執行的風險層級比唯讀的檔案存取高一截，
不應該讓使用者在確認視窗上隨手就把執行目錄换到範圍外。

#### `get_system_context`：Stage 3 範圍比 §9 的 Context Awareness 小很多

先查過現有程式碼：`device_query` 已經是既有依賴，但那是給滑鼠追蹤用的
（跟隨滑鼠功能），不是系統情境感知。§9 那張資料來源表（CPU/記憶體/電量、
作用中視窗、閒置時間、剪貼簿）**一行都還沒做**——全部是 Stage 6 的工作，
不是 Stage 3 可以直接「整合既有情境感知」的東西（§6.3 原本那句話講得像已經
有東西可以接，實際上沒有）。

Stage 3 的 `get_system_context` 只做不需要任何特殊權限、跨平台不會有落差的
子集：

- CPU / 記憶體 / uptime（新增 `sysinfo` 這一個成熟、跨平台的 crate）
- 目前日期時間（`chrono`，既有 workspace 依賴）
- OS 平台（`std::env::consts::OS`）

**明確排除、留到 Stage 6 才做**：作用中視窗標題、閒置時間、剪貼簿內容——
這三個都是 §9 自己已經點名有真正落地成本的訊號（Wayland 沒有標準 API 讀
作用中視窗、macOS 需要輔助使用權限、閒置時間需要另外的平台 API/crate，
剪貼簿隱私敏感度最高本來就預設要關）。Stage 3 提前做這幾個，等於把 Stage 6
的工作提前又做得比較隨便，不如就留在原本規劃的階段做好。

### 6.4 MCP 的位置

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

### 6.5 Tool Permission

桌寵具備本機操作能力後，權限層必須獨立於模型。

建議：

```text
Tool
 ├── auto
 ├── confirm
 └── deny
```

初始建議：

Tool 預設

---

get_time / system status auto
web search auto
read project file confirm
write file confirm
run shell command confirm
delete file deny / confirm
send message / external side effect confirm

模型不能因為自己說「這是安全的」就繞過 permission layer。

---

### 6.6 Stage 3 Runtime Ownership：Aemeath Agent Runtime 與 External Agent Runtime

Stage 3 應明確建立一個重要邊界：

> **誰擁有 Agent Runtime，誰就擁有這次 execution 的 Agent Loop。**

Aemeath 不需要把所有模型都包進同一個 Agent Loop。應區分「裸
Model/API」與「已經具備 Agent Loop 的 Runtime」。

執行方式 Agent Loop owner Tool owner

---

Qwen / Ollama Aemeath Aemeath Native Tools
Qwen API Aemeath Aemeath Native Tools
OpenAI API Key Aemeath Aemeath Native Tools
Anthropic API Key Aemeath Aemeath Native Tools
Claude Code `claude -p` Claude Code Claude Code native tools
Codex app-server Codex Codex native tools
OpenClaw OpenClaw OpenClaw native tools

OpenClaw 目前的 runtime 文件也採用相同的分層：Provider、Model 與 Agent
Runtime 是不同概念；Agent Runtime 負責準備好的模型 loop、native tool
calls 與完成結果。citeturn0search0turn0search2

因此 Aemeath 最終應該有：

```text
                         Aemeath
                           │
                       Execution
                           │
              ┌────────────┴────────────┐
              │                         │
      Aemeath Agent Runtime       External Agent Runtime
              │                         │
      ┌───────┼────────┐        ┌───────┼────────┐
      │       │        │        │       │        │
    Qwen   OpenAI   Anthropic  Claude  Codex   OpenClaw
   Ollama    API       API      Code
      │       │        │        │       │        │
      └───────┼────────┘        └───────┼────────┘
              │                         │
       Aemeath Native Tools        Runtime Native Tools
```

**Stage 3 只實作左側 Aemeath Agent Runtime。**

Claude Code / Codex / OpenClaw 的 Agent Loop 不需要由 Aemeath
重做，也不要形成：

```text
Aemeath Agent Loop
    ↓
Claude Code
    ↓
Claude Agent Loop
```

正確方式是：

```text
Aemeath Execution
    ↓
ClaudeCodeRuntime
    ↓
claude -p
    ↓
Claude Code Agent Loop
    ↓
result
```

Codex 同理：

```text
Aemeath Execution
    ↓
CodexRuntime
    ↓
Codex app-server
    ↓
Codex Agent Loop
    ↓
result
```

---

### 6.7 Stage 3 的 Aemeath Agent Runtime

Stage 3 新增：

```rust
trait AgentRuntime {
    async fn execute(
        &self,
        execution: &Execution,
    ) -> Result<AgentResult>;
}
```

Aemeath 自己的 runtime：

```text
AemeathAgentRuntime
   ├── Ollama / Qwen
   ├── OpenAI API
   └── Anthropic API
```

其 loop 為：

```text
Execution
   ↓
Model request
   ↓
Model response
   ↓
tool_calls?
 ├── No  → final answer
 └── Yes
       ↓
   schema validation
       ↓
   permission check
       ↓
   Aemeath Tool Registry
       ↓
   tool result
       ↓
   model again
       ↓
   repeat
```

概念：

```rust
loop {
    let response = model.chat(messages, tools).await?;

    if response.tool_calls.is_empty() {
        return response.content;
    }

    for call in response.tool_calls {
        validate_tool_arguments(&call)?;
        permission.check(&call).await?;

        let result = tool_registry.execute(call).await?;

        messages.push(call.as_assistant_message());
        messages.push(result.as_tool_message());
    }

    execution.check_limits()?;
}
```

---

### 6.8 Stage 3 Tool Ownership

Stage 3 建立 Aemeath 自己的 Tool abstraction。實際落地版本比最初的草稿多兩個
方法，理由來自把 4 個 native tool 的具體設計（見 §6.3）攤開來看之後發現的：

- `read_file`/`list_directory` 是不是要 `confirm`，不是固定看 tool
  名稱，而是要看**這一次的 `path` 參數**落在 `project_root` 裡還是外面、
  或已經在 session 的允許清單裡——所以 permission tier 的判斷要能看到
  `args`，不能是一個靜態表。
- §6.11 的「這個 session 都允許」要按 tool 決定要不要提供這個捷徑
  （`run_command` 這種高風險工具不提供）。

而且 4 個 tool 實際盤點下來，**沒有一個在 `execute()` 裡需要
`AppHandle`**——確認彈出視窗、oneshot 那一整套機制都是 Agent Loop 自己
在做（收集這一回合所有需要 confirm 的呼叫、開一個視窗、等 oneshot、再
決定要不要呼叫 `execute()`），不是每個 `Tool` 實作自己要處理的事。這樣
`Tool` 可以維持跟 `fleet-snowfluff-ai` 裡 `AiProvider` 一樣的風格——不掛
Tauri 專屬型別，方便單獨測試：

```rust
trait Tool {
    fn definition(&self) -> ToolDefinition;

    /// 大多數 tool 忽略 `args`、回傳固定值（`run_command` 永遠
    /// `Confirm`；`web_search`/`get_system_context` 永遠 `Auto`）；
    /// `read_file`/`list_directory` 則要看 `args` 裡的 path 對照
    /// `ctx.project_root` 跟 session 允許清單，動態決定。
    fn required_permission(&self, args: &Value, ctx: &ToolContext) -> PermissionTier;

    /// §6.11 的「這個 session 都允許」要不要提供這個捷徑，預設不提供。
    fn allows_session_remember(&self) -> bool { false }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult, ToolError>;
}

struct ToolContext {
    project_root: Option<PathBuf>,
    conversation_id: ConversationId, // 用來查/更新 (tool_name, path) 的
                                      // session 允許清單
}

struct ToolResult {
    content: String, // 塞回去給模型當 tool_result 的內容
    is_error: bool,  // 讓模型分得出「正常執行、結果是空的/沒有權限」
                      // 跟「真的執行失敗」——影響它該不該換個方式重試
}
```

Agent Loop 對這一回合每個 tool call 呼叫 `required_permission()`，把
`Confirm` 的收集起來批次跳一個視窗（§6.11），視窗結果出來才決定要不要
呼叫 `execute()`——`execute()` 本身完全不需要知道自己是 auto 通過還是
使用者在彈出視窗裡按了允許。

以及：

```text
Aemeath Tool Registry
 ├── get_system_context
 ├── web_search
 ├── read_file
 ├── list_directory
 └── run_command
```

此階段 **不要加入 MCP Tool**。

因此 Stage 3 的 Tool Layer：

```text
Aemeath Agent Runtime
        ↓
Aemeath Tool Registry
        ↓
Native Tools
```

Stage 6 才擴充成：

```text
Aemeath Tool Registry
 ├── Native Tools
 └── MCP Tools
```

這樣 Agent Core 不會被 MCP protocol 綁死。

---

### 6.9 Claude Code 可以保留自己的 Tools

Claude Code 使用 `claude -p` 時，Agent Loop 與 native tools 都由 Claude
Code 負責。

例如：

```text
Claude Code Agent Loop
 ├── Read
 ├── Write
 ├── Edit
 ├── Bash
 ├── Glob
 ├── Grep
 └── ...
```

Aemeath 不需要在 Stage 3 將這些 tools 全部重新實作成 Aemeath Tools。

未來 Stage 6 如果需要讓 Claude Code 使用 Aemeath-owned capability，應透過
MCP：

```text
Claude Code Agent Loop
 ├── Claude native tools
 └── Aemeath MCP tools
```

因此：

> **Stage 3 不需要用 `--disallowedTools` 把 Claude Code 的所有 native
> tools 全部禁掉。**

只有當產品明確要求某個 capability 必須經過 Aemeath-controlled boundary
時，才對該 native tool 做限制；否則讓 Claude Code 使用自己的成熟 tool
system。

#### Native tool 的 by-case 允許清單（實際落地設計）

用實際安裝的 `claude` CLI（v2.1.275，`claude -p --help`）確認過的旗標：

```text
--allowedTools <tools...>     e.g. "Bash(git *) Edit"
--disallowedTools <tools...>  e.g. "Bash(git *) Edit"
--permission-mode <mode>      acceptEdits | auto | bypassPermissions
                              | manual | dontAsk | plan
```

`--allowedTools` / `--disallowedTools` 支援細到 subcommand 層級的 scoping
（`Bash(git *)` 只允許 `git` 子指令，不是任意 shell），所以取代現有
`claude_code_cli.rs` 裡整批擋掉的 `DISALLOWED_TOOLS` 常數是可行的——改用
per-profile 的設定，動態組出這次 spawn 要放進 `--allowedTools` 還是
`--disallowedTools` 的清單。

**v1 只做靜態、spawn 前就決定好的允許清單，不做即時 per-invocation
confirm。** `--permission-mode manual` / `dontAsk` 這些旗標存在，但在
headless、沒有 TTY 的 spawn 情境下是否真的能做「即時詢問、等待回覆」這件
事——沒有驗證過；而且現有 `claude_code_cli.rs` 的 `spawn()` 本來就把
`stdin(Stdio::null())` 關閉了，就算這個協議真的存在，Aemeath 現在的實作也
接不進去（要能回覆審批，至少要保留 stdin 可寫入）。跟這份文件對
`claude auth login` 的態度一樣：旗標存在不代表在這個 headless 用法下真的
如文件描述地運作，需要另外驗證，不能先假設可行。

因此 Aemeath 自己的 Permission model（auto / confirm / deny）套用到 external
CLI 的 native tools 時，**`confirm` 一律降級成 `deny`**——沒有驗證過的即時
確認管道，不能假裝有。預設分類建議：

| Native tool                                                          | 預設                                             |
| -------------------------------------------------------------------- | ------------------------------------------------ |
| `Read` / `Glob` / `Grep`（唯讀，風險低）                             | auto（放進 `--allowedTools`）                    |
| `WebSearch` / `WebFetch`（見下方說明）                               | auto（放進 `--allowedTools`）                    |
| `Write` / `Edit` / `Bash` / `NotebookEdit` / `Task` / `SlashCommand` | deny（放進 `--disallowedTools`，維持今天的行為） |

`WebSearch`/`WebFetch` 原本被歸在 deny 那一類，後來確認 Claude Code 的
`WebSearch` 是 Anthropic 自己的 server-side tool（`web_search_20250305`），
執行在 Anthropic 的基礎設施上，用的是同一個 subscription/API session，
不需要另外的 API key、不另外計費；Codex 也有等價的第一方 web search（預設
cached、`--search`/`web_search = "live"` 可切 live mode），同樣是 OpenAI
自己的服務。純唯讀、沒有檔案/shell 副作用、也不是 Fleet 要另外掏錢或另外
管理的依賴——風險等級跟 `Read`/`Glob`/`Grep` 同一類，所以移到 auto。

使用者可以在設定裡把個別 tool 從 deny 改成 auto（代表自己承擔「這個
profile 之後每次呼叫都自動允許」的風險，沒有逐次確認），但介面上不應該讓
它看起來像有逐次 confirm 這回事，避免造成安全感上的誤導。

**這是跟 Aemeath 自己 Tool Registry 完全獨立的一組 permission 設定**——工具
集不同（Claude Code 的十來個 native tools，跟 Aemeath 自己的 5 個 Native
Tools 沒有一一對應關係），owner 也不同（Claude Code 自己的 Agent Loop vs.
Aemeath Agent Runtime），現在不需要為了介面統一而勉強合併成同一套抽象。

Codex 目前沒有裝在這台機器上可以直接驗證；從既有文件推測，它的 sandbox
分級（`read-only` / `workspace-write` / `danger-full-access`）比 Claude
Code 的逐 tool allow-list 粗——「by case」對 Codex 可能只到 sandbox 等級，
不到個別 tool 名稱，這點也待驗證，跟本文件其他 Codex 相關敘述一樣標記為
unverified/experimental。**還有一點沒驗證過：`--sandbox read-only` 除了
擋檔案寫入，會不會連 Codex 自己的 web search（走 OpenAI 自己的服務，不是
本機網路存取）也一起擋掉——如果 sandbox 分級連網路存取都管，現有的
`read-only` spawn 設定可能已經在無意間關掉 Codex 的 web search 了，需要
實際裝一台有 Codex 的機器驗證。**

---

### 6.10 Qwen / API Key 與 Claude Code 的根本差異

Qwen / Ollama：

```text
Qwen
 ↓
tool_call
 ↓
Aemeath Agent Runtime
 ↓
Aemeath Tool Registry
 ↓
tool_result
 ↓
Qwen
 ↓
...
```

OpenAI API Key / Anthropic API Key 也是：

```text
Provider API
 ↓
tool_call
 ↓
Aemeath Agent Runtime
 ↓
Aemeath Tool Registry
 ↓
tool_result
 ↓
Provider API
 ↓
...
```

Claude Code：

```text
Claude Code
 ↓
native tool call
 ↓
Claude Code executor
 ↓
tool result
 ↓
Claude Code
 ↓
...
```

所以不能用「Provider 名稱」判斷 loop owner，而應使用：

```text
Runtime
```

判斷。

---

### 6.11 Stage 3 Permission Boundary

Aemeath Agent Runtime 的 native tools 必須由 Aemeath permission layer
強制控制：

```text
Model
 ↓
Tool Call
 ↓
Schema Validation
 ↓
Aemeath Permission
 ├── auto
 ├── confirm
 └── deny
 ↓
Executor
```

External Runtime 則是：

```text
Aemeath Execution Policy
        ↓
External Runtime
        ↓
Runtime-owned tools / permissions
```

Aemeath 應控制：

- 是否允許啟動 external runtime
- 哪個 runtime 可以被 routing 選擇
- execution timeout / cancellation
- external runtime 的 capability policy
- external session reference

但不應假設可以直接控制 external runtime 內部每一個 native tool。

如果未來需要統一控制某個產品級 capability，應優先把該 capability 放到
Aemeath-owned MCP boundary。

#### `confirm` 的實際落地：彈出視窗 + 批次確認 + session 內記住選擇

`confirm` 不是一個抽象狀態——它需要真的有東西跳出來問使用者，並且要有辦法把
答案送回正在等待的 Agent Loop。設計如下：

```text
Aemeath Agent Runtime（Ollama tool-calling loop）
    │
    ▼
這次模型回覆裡，所有需要 confirm 的 tool_calls（auto 的直接執行、
deny 的直接跳過，不會進到這裡）
    │
    ▼
開一個新視窗（跟 status_bubble.rs 的 WebviewWindowBuilder 同一套做法：
新的 window label "tool-confirmation"、沿用 index.html、client 端依
label 分流——但這次 focused(true)，不像 bubble 刻意 focused(false)，
因為使用者需要主動閱讀並回應）
    │
    ▼
視窗裡列出「這一次模型回合」所有待確認的 tool_calls（不是一個一個彈，
一次列完），每項可個別 Accept / Deny，並有「全部允許」/「全部拒絕」
的捷徑按鈕，外加每項一個「這個 session 都允許」的勾選框
    │
    ▼
Agent Loop 這時是 `.await` 一個 oneshot::Receiver，卡在這裡
    │
    ├── 使用者送出決定
    │     → Tauri command 把整批決定（每項 accept/deny +
    │       是否記住）送回，透過對應的 oneshot::Sender 喚醒 Agent Loop
    │     → 被拒絕的 tool_call 不執行，改把一則「使用者拒絕執行」的
    │       tool result 訊息塞回給模型，讓它知道別再盲目重試同一個呼叫
    │
    └── 使用者直接關掉視窗，沒有送出決定
          → sender 被丟棄，receiver 收到 Err → 視同全部 Deny（安全預設）
```

**聊天視窗與 status bubble 完全不需要新狀態。** `ChatRuntimeState.pending`
本來就在整個 generation task 還沒結束前維持 `Some(...)`；卡在 oneshot
上等待，本身就是這個 task 還在跑。`get_chat_state` 的 `is_pending` /
`partial_text`、bubble 的 `...` glyph，今天的邏輯就已經是對的，不用改一行。
也順便繼承了「同時只能有一個 generation」的既有規則——使用者本來就沒辦法
在確認視窗開著的時候送出下一則訊息。

**「這個 session 都允許」記住的是什麼、記多久：** 存在
`ChatRuntimeState`（跟 `cli_sessions`同一層級）裡一個
`Mutex<HashMap<(ConversationId, ToolName), ()>>`（或等價的 set），
**只存在記憶體、不落地存檔**——跟 `cli_sessions` 一樣的理由：這是暫時的
session 內信任升級，不是永久的設定變更；`new_chat_session` 清掉
`cli_sessions` 的同時，也應該一併清掉這個 allowlist，開新對話 =
信任狀態重置。Agent Loop 在跳出確認視窗之前，先查這個 allowlist——如果這次
的 tool 已經在裡面，直接當作 `auto` 執行，連視窗都不用開。這是暫時的
session-scoped upgrade，不是把 Settings 裡的預設 permission tier
永久改掉。

**一個值得先講清楚的安全考量：「記住」是按 tool 名稱，不是按參數。**
對 `read_file` / `list_directory` / `web_search` / `get_system_context`
這種參數變化風險不大的工具，「這個 session 都允許」很合理。但對
`run_command` 這種工具，同一個 tool 名稱底下，一次核准的指令
（例如 `ls -la`）跟之後模型可能嘗試的指令（例如某個危險指令）風險天差
地遠——按 tool 名稱記住等於「以後這個 session 內的任何 shell 指令都不用
再問」，這個粒度可能太粗。建議：**高風險工具（至少 `run_command`，可能也
包含 `write_file`）不提供「這個 session 都允許」的勾選框，每次都要單獨
confirm；低風險、讀取類的工具才提供這個捷徑。** 這個名單本身應該可以在
`Tool` trait 上加一個旗標（例如 `allows_session_remember() -> bool`），
不要寫死在 UI 裡。

（`read_file`/`list_directory` 這種有路徑維度的 tool，「記住」實際上要
按 `(tool_name, path)` 記，不是整個 tool 名稱一次全信任——完整設計見
§6.3 的 `read_file` / `list_directory` 小節。）

---

### 6.12 Stage 3 Execution Lifecycle

Stage 2 已建立 `Execution` domain；Stage 3 開始正式讓它承擔 Agent Run
lifecycle：

```text
Execution
 ├── created
 ├── running
 │    ├── model_call
 │    ├── tool_call
 │    ├── tool_result
 │    └── model_call
 ├── completed
 ├── failed
 ├── cancelled
 └── timed_out
```

至少需要：

- execution status
- parent / child execution relationship
- execution depth / child count
- iteration count
- tool trace
- timeout
- cancellation
- cancellation propagation to children
- tool execution errors
- schema validation errors
- usage / token metadata（provider 能提供時）
- execution-tree budget / cost accounting

並提供：

```rust
execution.max_iterations
execution.timeout
execution.cancel_token
```

避免：

```text
tool failure
 → model retry
 → tool failure
 → model retry
 → ...
```

---

### 6.13 Sub-agent / Delegation：原本 Execution 模型即可自然支援

> **這一節的內容屬於 Stage 5（獨立階段），不是 Stage 3。** 放在這裡是因為它在
> 概念上延續本節（Aemeath Agent Runtime / Execution）的討論，但實作順序上排在
> Stage 3（單一 Agent Loop 先穩定）與 Stage 4（external runtime session 生命
> 週期先存在）之後——見第 13 節 Implementation Roadmap 的 Stage 5。

原本 v8 的 `Conversation → Execution → Runtime Session` 已經足夠支援 Sub-agent；
不需要新增一個與 Execution 平行的 `SubAgent` domain。核心改動是：

1. `Execution` 支援 `parent_execution_id`
2. `Execution` 增加 `ExecutionKind::SubAgent`
3. Agent Runtime 增加 `DelegationManager`
4. Context Engine 提供 parent → child 的 context projection
5. Parent 只接收 child 的結構化 `ExecutionResult`，不共享 child session

> **Sub-agent 本質上是一個由 parent Execution 委派產生、具有自己 task/context/runtime/session 的 child Execution。**

#### 6.13.1 Execution Tree

```text
Conversation conv_001
        │
        ▼
Execution ex_root
        │
        ├── Sub-agent ex_a ──→ Research / Web
        │        └── Sub-agent ex_a1 ──→ Extract data
        ├── Sub-agent ex_b ──→ Code analysis
        └── Sub-agent ex_c ──→ Test / verification
```

所有 child execution 仍屬於同一個 Aemeath Conversation，但擁有自己的 execution
lifecycle。UI 可以只顯示 root 對話，也可以展開正在工作的 agents。

#### 6.13.2 Delegation domain model

```rust
enum SubAgentRole {
    Researcher,
    Coder,
    Reviewer,
    Tester,
    Planner,
    General,
}

struct DelegationRequest {
    parent_execution_id: ExecutionId,
    task: Task,
    role: SubAgentRole,
    preferred_route: Option<ExecutionRoute>,
    context: SubAgentContextSpec,
    max_depth: u32,
    timeout: Duration,
}

struct DelegationResult {
    child_execution_id: ExecutionId,
    result: ExecutionResult,
}
```

`preferred_route` 只是 parent 對 child 的 hint，不能繞過 Aemeath routing policy。

#### 6.13.3 Delegation loop

模型不應直接建立 process / session；應呼叫受控的 `delegate` capability：

```text
Parent Agent
    │
    │ delegate(task, role, context_spec)
    ▼
Delegation Manager
    │
    ├── policy / permission
    ├── depth limit
    ├── concurrency limit
    ├── budget / token limit
    └── Task Router
            │
            ▼
       Child Execution
            │
            ▼
       Context Engine
            │
            ▼
       Runtime Adapter
            │
            ▼
       Child Result
            │
            ▼
Parent Agent
```

#### 6.13.4 Child 可以使用不同 runtime

```text
Parent
Claude Subscription
    │
    ├── Research child → Web-capable runtime
    ├── Coding child   → Claude Code
    └── Local analysis → Qwen / Ollama
```

因此 Task Router 不需要 `SubAgentRouter`；每個 child 都是正常的 Execution。

#### 6.13.5 Parallel fan-out / join

```text
                 Parent
                   │
          ┌────────┼────────┐
          ▼        ▼        ▼
       Research  Coding   Testing
          │        │        │
          └────────┼────────┘
                   ▼
              Aggregation
                   │
                   ▼
                Parent
```

Aemeath 應由 `DelegationManager` 負責 fan-out / join，而不是讓 LLM 自己管理等待。

```rust
trait DelegationManager {
    async fn spawn(&self, request: DelegationRequest) -> Result<ExecutionId>;
    async fn wait(&self, execution_ids: &[ExecutionId]) -> Result<Vec<DelegationResult>>;
    async fn cancel(&self, execution_id: ExecutionId) -> Result<()>;
}
```

#### 6.13.6 Child result 不直接污染 Conversation history

Child 完整 transcript / tool trace 留在 child Execution。Parent 預設只拿：

```rust
struct ExecutionResult {
    output: Message,
    summary: Option<String>,
    facts: Vec<Fact>,
    decisions: Vec<Decision>,
    artifacts: Vec<ArtifactRef>,
}
```

需要 debug 時，再透過 `ExecutionRef` / `ArtifactRef` 載入詳細 trace。

#### 6.13.7 Delegation guardrails

| Guardrail                      | 用途                                       |
| ------------------------------ | ------------------------------------------ |
| `max_depth`                    | 防止無限遞迴                               |
| `max_children`                 | 限制一次 execution 的 child 數量           |
| `max_concurrency`              | 限制同時 runtime / process 數量            |
| `max_total_execution_time`     | 限制整棵 execution tree                    |
| `token_budget` / `cost_budget` | 控制 API / subscription / local model 資源 |
| permission inheritance         | child 不得取得高於 parent 的權限           |
| cancellation propagation       | parent cancel 時停止 child                 |
| failure policy                 | child failure 不一定等於 parent failure    |

最重要的安全原則：**Child permission ceiling ≤ Parent permission ceiling。**

#### 6.13.8 Failure policy

```rust
enum ChildFailurePolicy {
    FailParent,
    ContinueWithoutChild,
    Retry,
    Escalate,
}
```

例如 research child 失敗可讓 parent 繼續；測試 child 失敗則可能需要修正後重試。
這是 execution policy，不應由模型自行決定。

#### 6.13.9 Aemeath-owned vs External-runtime-owned sub-agent

**Aemeath-owned：**

```text
Aemeath Agent Runtime
   └── DelegationManager
        ├── Child Execution A
        ├── Child Execution B
        └── Child Execution C
```

**External-runtime-owned：**

```text
Aemeath Execution
   └── OpenClaw / Codex / Claude Code
          └── runtime-owned sub-agents
```

Aemeath 不應強行把 external runtime 的 internal agent tree 映射成 Aemeath Execution tree；
除非 adapter 能提供穩定 lifecycle / event 介面。否則只保存 external run/session
reference 與結果。

#### 6.13.10 UI

```text
雪絨正在處理…

├─ 🔎 Researching
├─ 💻 Reviewing code
└─ 🧪 Running tests

          ↓

完成：我整理好結果了。
```

桌寵 UX 不必把 child 的所有中間對話直接塞進聊天泡泡。

---

### 6.14 Stage 3 / Stage 4 / Stage 5 / Stage 6 的責任切分

```text
Stage 3
Aemeath Agent Runtime
├── Agent Loop
├── Native Tool
├── Tool Registry
├── Permission
└── Execution lifecycle

Stage 4
External Agent Runtime
├── Claude Code
├── Codex
├── OpenClaw
├── Runtime Adapter
├── External Session lifecycle
└── Capability Escalation

Stage 5
Sub-agent / Delegation
├── DelegationManager
├── parallel fan-out / join
└── delegation guardrails

Stage 6
MCP
├── MCP Client
├── MCP tool discovery
├── MCP invocation
├── server lifecycle
└── Aemeath capability bridge
```

這個切分避免 Stage 3 同時處理：

```text
Aemeath Agent Loop
MCP protocol
External Agent Loop
External Session
```

四種不同層級的問題。

---

## 7. Tool Layer 與 Permission

### 7.1 Tool abstraction

```rust
trait Tool {
    fn definition(&self) -> ToolDefinition;
    fn required_permission(&self, args: &Value, ctx: &ToolContext) -> PermissionTier;
    fn allows_session_remember(&self) -> bool { false }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<ToolResult, ToolError>;
}
```

實際落地版本（為什麼多了 `required_permission`/`allows_session_remember`、
`ToolContext`/`ToolResult` 各自裝什麼、為什麼 `execute()` 不需要
`AppHandle`）見 §6.8，這裡不重複，避免兩份說明之後只改一邊。

### 7.2 Stage 3 Native Tools

```text
Aemeath Tool Registry
 ├── get_system_context
 ├── web_search
 ├── read_file
 ├── list_directory
 └── run_command
```

Stage 3：

```text
Aemeath Agent Runtime
        ↓
Aemeath Tool Registry
        ↓
Native Tools
```

Stage 6 才擴充：

```text
Aemeath Tool Registry
 ├── Native Tools
 └── MCP Tools
```

### 7.3 Permission

```text
Tool
 ├── auto
 ├── confirm
 └── deny
```

初始方向：

| Tool                                | Default        |
| ----------------------------------- | -------------- |
| get_time / system status            | auto           |
| web search                          | auto           |
| read project file                   | confirm        |
| write file                          | confirm        |
| run shell command                   | confirm        |
| delete file                         | deny / confirm |
| send message / external side effect | confirm        |

模型不能自己宣稱安全並繞過 permission。

### 7.4 Aemeath Permission Boundary

Aemeath-owned native tools：

```text
Model
 ↓
Tool Call
 ↓
Schema Validation
 ↓
Aemeath Permission
 ├── auto
 ├── confirm
 └── deny
 ↓
Executor
```

External runtime：

```text
Aemeath Execution Policy
        ↓
External Runtime
        ↓
Runtime-owned tools / permissions
```

Aemeath 可以控制：

- 是否允許啟動 external runtime
- 哪個 runtime 可以被 routing
- execution timeout / cancellation
- external runtime capability policy
- external-runtime-owned sub-agent mapping（若 runtime 提供 lifecycle/events）
- Context Engine / advanced compaction integration
- external session reference

但不能假設自己能直接攔截 external runtime 的所有 native tool calls。

如果某個 capability 必須統一經過 Aemeath policy，優先放在 Aemeath-owned MCP boundary。

### 7.5 Claude Code native tools

Claude Code 使用 `claude -p` 時，其 Agent Loop 與 native tools 由 Claude Code
負責：

```text
Claude Code Agent Loop
 ├── Read
 ├── Write
 ├── Edit
 ├── Bash
 ├── Glob
 ├── Grep
 └── ...
```

Aemeath 不需要 Stage 3 重新實作。

也不需要為了架構一致而用 `--disallowedTools` 把 Claude Code 所有 native tools
全部禁掉。只有當某個 capability 明確要求 Aemeath-controlled boundary 時，才限制
對應 native tool。

> 實際的 by-case 允許清單設計（`--allowedTools`/`--disallowedTools` 的具體
> 用法、預設分類、為什麼 v1 不做即時 confirm）見 §6.9 的完整版本，這裡不
> 重複，避免兩份說明之後改一邊忘了改另一邊。

---

## 8. MCP：Capability Bridge

MCP 是 Tool Layer 的標準介面，而不是 Agent 本身：

```text
Agent
  ↓
Tool Registry
  ├── Native Tool
  ├── MCP Tool
  └── Future Plugin
```

Stage 6：

```text
Aemeath Agent Runtime → Aemeath MCP
Claude Code         → Aemeath MCP
Codex               → Aemeath MCP
OpenClaw            → Aemeath MCP
```

MCP server 的 filesystem / network / shell capability 仍需經過 Aemeath permission policy。

---

## 9. Context Awareness

> 核心原則：**本地規則引擎決定「要不要講」，AI 只負責「怎麼講」。**

### 5.1 資料來源與抓取方式

---

資料來源 建議作法 平台差異 / 注意事項

---

作用中視窗標題/程式名稱 `active-win-pos-rs` Windows 用
等跨平台 crate `GetForegroundWindow`；macOS
需要輔助使用權限；Linux
Wayland 目前沒有標準 API

CPU / 記憶體 / 電量 / `sysinfo` 三平台支援
開機時間

閒置時間 平台 API 或 `user-idle` 平台實作不同

剪貼簿內容 `arboard` 隱私敏感度最高，預設關閉

---

### 5.2 規則引擎

1.  所有資料來源彙整成 `ContextSnapshot`
2.  定期輪詢
3.  規則決定是否觸發
4.  每條規則有 cooldown
5.  再加全域 cooldown
6.  規則觸發後才呼叫 AI 生成內容

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

---

## 10. Persona / Memory / Knowledge

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

層級 性質 管理方式

---

好感度/心情值 結構化數值 Rust
近期互動記憶 短摘要 AI + SQLite / file
長期歷史記憶 大量語意資料 向量檢索

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

---

## 11. External Agent Runtime

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

### 7.2 不建議讓 OpenClaw 成為 Aemeath 核心

Aemeath 應保留：

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

因此使用者始終只是在跟雪絨聊天，但背後可以從 Qwen3 升級到 Codex / Claude
Code。

---

---

## 12. Privacy / Data Boundary

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

---

## 13. Implementation Roadmap

### Stage 1 — 已完成

Provider + Chat + Persona。

### Stage 2 — Subscription-first Chat + Domain Foundation（已完成）

建立：

- Provider / Auth / Model / Runtime domain
- Conversation
- Execution
- ExternalSessionRef

已透過 `subscription-first-chat` 這個 change 完成並封存（見
`openspec/changes/archive/2026-09-18-subscription-first-chat`）。**Task Router
domain 原本列在這一階段，但實際實作時明確排除在外**（見該
change 自己 proposal.md 的 Non-Goals）——只有 `ConversationId` /
`ExecutionId` / `ExternalSessionRef` typing 真正完成。因此下面把 Task Router
domain 從這裡移到 Stage 3，讓文件跟實際狀態一致。

### Stage 3 — Aemeath Agent Core + Native Tools + Permission

（Task Router domain 原本列在 Stage 2，因為 Stage 2 實際實作時排除在外，移到
這裡一起做——見上方 Stage 2 的說明。）

建立：

- Task
- TaskRequirements
- RoutingContext
- ExecutionRoute
- SessionStrategy
- TaskRouter
- Routing / Fallback policy
- Task Router `mode`：`single` / `mix`（見第 4.13 節的具體設計）
- Aemeath Agent Runtime
- Agent Loop
- Tool abstraction
- Tool Registry
- Native Tools
- Permission
- Execution lifecycle
- streaming / thinking / final-answer handling
- iteration / timeout / cancellation
- ContextManager（基礎版：`build_context` / `record_execution`，見第 10 節；不含
  compaction、memory retrieval、sub-agent context projection，那些留到 Stage 5 /
  Stage 7）
- `ToolCallingProvider`：**v1 只實作 Ollama**，OpenAiCompatible / Anthropic
  的 tool-calling 是 fast-follow，不在這個 proposal 裡（三者的串流 tool-call
  wire format 各不相同，是各自獨立的實作工作，見 §6.7 之後的討論）
- Claude Code / Codex 的 native tool by-case 允許清單（`--allowedTools` /
  `--disallowedTools`，取代現有整批擋掉的 `DISALLOWED_TOOLS`；見 §6.9 的
  具體設計，v1 不含即時 confirm）
- Aemeath Native Tools 的 `confirm` 彈出視窗：批次列出同一回合所有待確認的
  tool_calls、oneshot 喚醒 Agent Loop、session 內「都允許」的記住選項
  （高風險工具如 `run_command` 不提供這個捷徑）——見 §6.11 的具體設計
- 第一批 5 個 native tools 的實際落地範圍（見 §6.3 對應小節）：
  `web_search`（SearXNG + DuckDuckGo IA 兩層 fallback，只服務 Ollama 的
  tool-calling）、`read_file`/`list_directory`（`project_root` 為 auto
  範圍，範圍外走 §6.11 的確認視窗）、`run_command`（30s timeout、~20KB
  輸出上限、工作目錄硬性綁 `project_root`）、`get_system_context`（新增
  `sysinfo` 依賴，只做 CPU/記憶體/uptime/日期時間/OS，不含作用中視窗/
  閒置時間/剪貼簿——那三個留到 Stage 6）

**不包含 MCP。**

**不要求 Claude Code / Codex / OpenClaw 使用 Aemeath Agent Loop——牠們的 native
tools 用自己的 allow-list 機制控制，不透過 Aemeath Tool Registry。**

**Sub-agent / Delegation 不在本階段** —— 移到 Stage 5，獨立成一個階段。原因：
delegation 需要先有 external runtime session 生命週期（Stage 4）才能讓 child
execution 安全地委派給 Claude Code / Codex / OpenClaw；在單一 execution loop
都還沒穩定前就建 `DelegationManager`，容易產生猜測性介面。

### Stage 4 — Capability Escalation + External Runtime Sessions

（原 Stage 6，移到這裡：Sub-agent/Delegation 需要它先存在，見 Stage 5。）

建立：

- Task capability detection
- Local / MCP / Advanced backend routing
- External Runtime Adapter
- SessionManager
- Codex session / Thread mapping
- Claude Code session mapping
- OpenClaw optional backend
- nested session isolation
- Execution → ExternalSessionRef
- session recovery
- external runtime capability policy

### Stage 5 — Sub-agent / Delegation

（原本在 Stage 3 內，獨立成自己的階段 —— 見第 6.13 節的完整設計。）

建立：

- DelegationManager
- Sub-agent child Execution（`parent_execution_id`、`ExecutionKind::SubAgent`）
- parallel fan-out / join
- delegation guardrails（depth / concurrency / budget / permission ceiling）
- Context Engine：Sub-agent Context Projection（`SubAgentContextSpec`、
  `build_sub_agent_context`，見第 10.1 節）

依賴 Stage 3（要有可委派的 Agent Loop）與 Stage 4（child 可能委派給 external
runtime，需要 SessionManager / Runtime Adapter 已存在）。

### Stage 6 — MCP + Context Awareness

（原 Stage 4。此處「Context Awareness」是環境情境感知——CPU / 視窗 / 閒置時間等
系統訊號（見第 9 節），與下面 Stage 7 提到的 Context Engine〔對話 / 執行的語意
context lifecycle，見第 10 節〕是兩個不同概念，只是中英文命名相近，不要混淆。
作用中視窗標題、閒置時間、剪貼簿內容——這三個 Stage 3 的 `get_system_context`
明確不做，留到這裡才做，見 §6.3 的 `get_system_context` 小節。）

建立：

- MCP Client
- MCP discovery / invocation
- MCP server lifecycle
- Aemeath MCP bridge
- Context Snapshot（含作用中視窗、閒置時間、剪貼簿——Stage 3 的
  `get_system_context` 沒有的那三個訊號）
- Rule Engine
- Emotion → Animation
- Proactive interaction

### Stage 7 — Memory / Knowledge

（原 Stage 5。）

建立：

- 好感度 / 心情值
- 近期互動記憶摘要
- RAG
- 長期向量記憶（需求確認後）
- Context Engine 升級：Compaction、Memory Retrieval，`ContextManager` →
  `ContextEngine` trait（見第 10.3 節）

### Stage 8 — Voice / Automation

（原 Stage 7。）

- TTS
- STT
- Browser automation
- 剪貼簿助手
- 多寵物互動
- Plugin / Script system

### Stage Boundary

```text
Stage 2 (已完成)
Provider / Auth / Runtime
Conversation / Execution
        │
        ▼
Stage 3
Task Router domain
+ Aemeath Agent Runtime
+ Native Tools
+ Permission
+ Execution lifecycle
+ ContextManager (basic)
        │
        ▼
Stage 4
External Agent Runtime
+ Claude Code
+ Codex
+ OpenClaw
+ Session lifecycle
+ Capability escalation
        │
        ▼
Stage 5
Sub-agent / Delegation
+ DelegationManager
+ parallel fan-out / join
+ Context Engine: sub-agent projection
        │
        ▼
Stage 6
MCP
+ Context Awareness
        │
        ▼
Stage 7
Memory / Knowledge
+ Context Engine: compaction / retrieval
        │
        ▼
Stage 8
Voice / Browser / Automation
```

---

## 14. Stage 3 MVP

Stage 3 的目的不是增加 backend 數量，而是驗證：

> **Aemeath 是否已經具備一個可獨立運作的 Agent Core。**

```text
Fleet Snowfluff
      │
      ▼
   Chat UI
      │
      ▼
Aemeath Agent Runtime
      │
      ▼
Qwen3 / Ollama
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

---

## 15. Agent MVP Completion Checklist

Context / execution lifecycle：

- [ ] ConversationContext 作為 cross-execution canonical context
- [ ] ExecutionContext 作為 per-execution assembled snapshot
- [ ] ContextManager 負責 build / ingest
- [ ] ExecutionResult 可產生 summary / facts / decisions / artifacts
- [ ] 不直接把 external runtime session history 當作 Aemeath Conversation
- [ ] 不同 runtime 可以只透過 Aemeath Context 共享前一個 Execution 的結果

- [ ] Aemeath Agent Runtime 可以獨立執行完整 agent loop
- [ ] 模型可以選擇直接回答或呼叫 tool
- [ ] tool call 可以多輪執行
- [ ] tool result 能正確回到模型
- [ ] streaming 不會因 thinking / tool call 導致 UI 空白
- [ ] 有最大 iteration
- [ ] 有 timeout / cancellation
- [ ] tool argument 有 schema validation
- [ ] 高風險 tool 要求使用者確認
- [ ] 一次任務可以使用多個工具
- [ ] tool 失敗後 Agent 可以重新決策
- [ ] 可以記錄 Agent run 的 tool trace
- [ ] Qwen / Ollama 可使用 Aemeath Native Tools
- [ ] OpenAI API Key / Anthropic API Key 使用相同 Aemeath Agent Runtime
- [ ] Claude Code / Codex 不被錯誤包進 Aemeath Agent Loop
- [ ] MCP 不被 Stage 3 Native Tool abstraction 綁死
- [ ] Execution 正確管理 Agent run lifecycle
- [ ] Task Router 可以輸出 ExecutionRoute
- [ ] Router 不直接管理 session lifecycle

---

## 16. Known Risks / Technical Constraints

- **Wayland 視窗資訊讀取**：目前無標準
  API，情境感知的視窗偵測需明確標示限制
- **本地模型的 Agent 能力**：小模型即使支援 tool
  calling，也可能產生錯誤 arguments、選錯工具或在多步任務中失去目標
- **Qwen3 thinking token budget**：thinking 與 final answer 共用生成
  token budget；若設定過小，可能出現大量 thinking 後沒有足夠 token
  產生最終回答
- **Tool execution 不是模型責任**：Ollama/Qwen3 產生 tool call
  後，實際執行必須由 Aemeath runtime 完成
- **Agent Loop ownership**：Aemeath Agent Runtime 與 Claude Code / Codex
  / OpenClaw 的 external Agent Runtime 不應雙重包裝；external runtime
  的 loop 與 native tools 由其自身管理
- **MCP timing**：MCP 保留到 Stage 6，避免 Stage 3 的 Tool abstraction
  被外部 protocol 綁定；Stage 6 再將 Aemeath-owned capability 暴露給不同
  runtime
- **MCP security**：MCP server 提供的工具可能具有檔案、網路或 shell
  能力，必須套用 Aemeath 自己的 permission policy
- **Shell / filesystem**：不得因為模型要求就直接執行任意命令，需
  permission / confirmation
- **Agent loop runaway**：必須設定 iteration、timeout、token budget 與
  cancellation
- **Subscription auth**：OpenAI Codex / Anthropic Claude Code 的
  subscription 使用方式是 provider-specific，可能因 provider
  政策改變；不可視為永久穩定的 API
- **Anthropic subscription**：目前 OpenClaw 文件記載 `claude -p` /
  Agent SDK / 第三方應用程式用量仍計入登入 subscription limits，但
  Anthropic 可以在 OpenClaw 不更新的情況下修改規則；正式環境仍應考慮
  API key
- **OpenAI subscription**：OpenClaw 目前透過 ChatGPT/Codex OAuth 與
  native Codex app-server 使用 subscription，不等於一般 OpenAI API key
- **OpenClaw dependency**：若把 OpenClaw 作為核心
  runtime，將增加版本、權限與 provider policy 的耦合；建議保持
  optional backend
- **RAG 知識庫維護成本**：遊戲改版後需手動更新內容
- **長期記憶成本**：不要在需求未驗證前建立重型 vector DB
- **語音 pipeline**：TTS/STT 會增加資源、平台相容性與 UX 複雜度
- **角色 IP 使用邊界**：persona
  檔案不可寫入官方逐字台詞，需自行改寫並附版權聲明

---

---

## 17. Final Architecture

### Context / Execution relationship

```text
                         Conversation
                              │
                              ▼
                       Context Store
                              │
                    ┌─────────┴─────────┐
                    │                   │
             Context Builder      Conversation UI
                    │
                    ▼
              RoutingContext
                    │
                    ▼
               Task Router
                    │
                    ▼
            ExecutionRoute
                    │
                    ▼
                 Execution
                    │
                    ▼
              ContextManager
                    │
                    ▼
            ExecutionContext
                    │
                    ▼
             Runtime Adapter
              ┌─────┴─────┐
              │           │
       Aemeath Agent      External Runtime
              │           │
           Qwen/API   Claude/Codex/OpenClaw
              │           │
              └─────┬─────┘
                    ▼
             ExecutionResult
                    │
                    ▼
         ContextManager.record_execution()
                    │
                    ▼
              Context Store
```

核心原則：

> **Conversation 是 shared semantic state 的容器；Execution 是一次工作；
> Runtime Session 是 runtime-owned execution state。**

> **ContextManager 負責把 Conversation Context 組裝成 Execution Context，
> 並把 ExecutionResult 的可共享語義回寫 Conversation Context。**

### 17.1 Domain relationship

```text
Conversation
    │
    ├── Conversation History
    ├── Persona
    ├── Memory
    │
    └── Executions
          │
          ├── Execution #001
          │      └── AemeathAgentRuntime
          │
          ├── Execution #002
          │      └── ClaudeCodeRuntime
          │             └── ExternalSessionRef
          │
          └── Execution #003
                 └── CodexRuntime
                        └── ExternalSessionRef
```

### 17.2 Routing relationship

```text
Conversation
     │
     ▼
User Message
     │
     ▼
Execution
     │
     ▼
RoutingContext
     │
     ▼
Task Router
     │
     ▼
ExecutionRoute
     │
     ├── runtime
     ├── provider
     ├── model
     └── session strategy
              │
              ▼
       Runtime Adapter
              │
        ┌─────┴─────┐
        │           │
        ▼           ▼
Aemeath Runtime   External Runtime
                    │
                    ▼
               SessionManager
```

### 17.3 Agent ownership

```text
Aemeath Agent Runtime
    ├── Agent Loop
    ├── Native Tool Registry
    ├── Permission
    └── Execution lifecycle

External Agent Runtime
    ├── Agent Loop
    ├── Native Tools
    └── Runtime-owned Session

MCP
    └── Aemeath capability bridge
```

### 17.4 最終責任邊界

> **Aemeath owns the product-level Conversation, Execution, routing, permission
> and Aemeath capabilities.**
>
> **External runtimes own their own Agent Loop, native tools and internal session state.**
>
> **Task Router decides the route; Runtime Adapter and SessionManager execute the lifecycle.**

---

## 18. Roadmap Summary

```text
已完成
Provider + Chat + Persona
        │
        ▼
Stage 2 (已完成)
Subscription-first Chat
+ Conversation
+ Execution
+ ExternalSessionRef
        │
        ▼
Stage 3
Task Router domain
+ Routing / Fallback model (mode: single / mix)
+ Aemeath Agent Runtime
+ Native Tools
+ Permission
+ Execution lifecycle
+ ContextManager (basic)
        │
        ▼
Stage 4
Capability Escalation
+ Claude Code
+ Codex
+ OpenClaw
+ Runtime Adapters
+ External Session lifecycle
        │
        ▼
Stage 5
Sub-agent / Delegation
+ DelegationManager
+ parallel fan-out / join
+ delegation guardrails
+ Context Engine: sub-agent projection
        │
        ▼
Stage 6
MCP
+ Context Awareness
        │
        ▼
Stage 7
Memory / Knowledge
+ Context Engine: compaction / retrieval
        │
        ▼
Stage 8
Voice / Browser / Automation
```

> **先讓 Aemeath 自己擁有完整 Agent 基礎，再用 MCP 擴充 capability，最後才編排已經自帶 Agent Loop 的 external runtimes。**

如此可以避免把 Aemeath Core 同時綁定 Conversation、Agent Loop、MCP protocol、
Claude Code、Codex、OpenClaw 與 Session lifecycle，並保留未來替換 Model、
Provider、Runtime、MCP server 與 external Agent backend 的空間。
