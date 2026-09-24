import { Channel, invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open as openExternalLink } from "@tauri-apps/plugin-shell";
import { open as openFolderPicker } from "@tauri-apps/plugin-dialog";
import "@picocss/pico/css/pico.min.css";
import "./style.css";

type Dictionary = Record<string, string>;

interface Config {
  total_screen: boolean;
  screen_index: number;
  scale_index: number;
  window_snap: boolean;
  transparency_index: number;
  auto_startup: boolean;
  click_through: boolean;
  follow_mouse: boolean;
  display_priority: number;
  wander_idle_stay_mode: number;
  instance_count: number;
  skip_updates: boolean;
  skip_version: string | null;
  voice_enabled: boolean;
  voice_volume: number;
  ui_language: string;
  voice_language: string;
}

interface PersonalizationSnapshot {
  config: Config;
  scale_options: number[];
  opacity_options: number[];
  monitor_count: number;
  voice_languages_with_clips: string[];
  config_path: string;
}

type ProviderKind = "open_ai" | "anthropic" | "ollama" | "mock";
type AuthMethod = "api_key" | "subscription" | "local";

interface ProfileKey {
  provider: ProviderKind;
  auth_method: AuthMethod;
}

interface ProviderProfile {
  provider: ProviderKind;
  auth_method: AuthMethod;
  model: string | null;
  base_url: string | null;
}

type TaskRouterMode = "single" | "mix";
type NativeToolAccess = "auto" | "deny";

// Mirrors `ClaudeCodeToolAccess` (`crates/fleet-snowfluff-ai/src/settings.rs`) --
// field names are its Rust field names verbatim (no `rename_all`, so
// `snake_case` round-trips as-is).
interface ClaudeCodeToolAccess {
  read: NativeToolAccess;
  glob: NativeToolAccess;
  grep: NativeToolAccess;
  web_search: NativeToolAccess;
  web_fetch: NativeToolAccess;
  write: NativeToolAccess;
  edit: NativeToolAccess;
  bash: NativeToolAccess;
  notebook_edit: NativeToolAccess;
  task: NativeToolAccess;
  slash_command: NativeToolAccess;
  todo_write: NativeToolAccess;
}

const CLAUDE_CODE_TOOL_ACCESS_KEYS = [
  "read",
  "glob",
  "grep",
  "web_search",
  "web_fetch",
  "write",
  "edit",
  "bash",
  "notebook_edit",
  "task",
  "slash_command",
  "todo_write",
] as const satisfies readonly (keyof ClaudeCodeToolAccess)[];

interface AiSettings {
  ai_enabled: boolean;
  enabled_profiles: ProviderProfile[];
  default_profile: ProfileKey | null;
  task_router_mode: TaskRouterMode;
  project_root: string | null;
  claude_code_tool_access: ClaudeCodeToolAccess;
}

interface AiSettingsSnapshot {
  settings: AiSettings;
  openai_key_set: boolean;
  anthropic_key_set: boolean;
  persona_warning: string | null;
}

// Mirrors `ai_commands::ProfileStatus` (`#[serde(tag = "state", rename_all
// = "snake_case")]`) -- checked fresh every time the AI tab opens, never
// cached (`ai-provider`'s "Provider status display").
type ProfileStatus =
  | { state: "not_configured" }
  | { state: "disclosure_pending" }
  | { state: "connected" }
  | { state: "runtime_unavailable"; detail: string }
  | { state: "not_logged_in"; detail: string }
  | { state: "quota_exhausted"; detail: string }
  | { state: "error"; detail: string };

// The fixed, fully-enumerable set of selectable profiles -- six total
// (OpenAI and Anthropic each get two auth methods; Ollama and Mock have
// only Local). Grouped for display as OpenAI / Anthropic / Local,
// subscription before API key within each cloud provider (matching the
// source doc's own "subscription-first" mockup).
const PROFILE_SLOTS: { provider: ProviderKind; auth_method: AuthMethod; experimental: boolean }[] =
  [
    { provider: "anthropic", auth_method: "subscription", experimental: false },
    { provider: "anthropic", auth_method: "api_key", experimental: false },
    { provider: "open_ai", auth_method: "subscription", experimental: true },
    { provider: "open_ai", auth_method: "api_key", experimental: false },
    { provider: "ollama", auth_method: "local", experimental: false },
    { provider: "mock", auth_method: "local", experimental: false },
  ];

// `--` not `:` -- a colon is legal in an HTML `id` attribute value, but
// breaks `querySelector` when used bare (`#ai-model-list-anthropic:subscription`
// parses `:subscription` as a pseudo-class and throws `SyntaxError:
// ... is not a valid selector`, for every single profile since every
// slot's key contains a colon). `data-slot="..."` attribute-selector
// lookups (`[data-slot="..."]`) would have been fine either way, but
// the bare `#id` lookups below aren't, so the key itself stays
// selector-safe everywhere it's used.
function slotKey(slot: ProfileKey): string {
  return `${slot.provider}--${slot.auth_method}`;
}

function parseSlotKey(key: string): ProfileKey {
  const [provider, auth_method] = key.split("--") as [ProviderKind, AuthMethod];
  return { provider, auth_method };
}

function findProfile(settings: AiSettings, slot: ProfileKey): ProviderProfile | undefined {
  return settings.enabled_profiles.find(
    (p) => p.provider === slot.provider && p.auth_method === slot.auth_method,
  );
}

function isDefaultProfile(settings: AiSettings, slot: ProfileKey): boolean {
  return (
    settings.default_profile?.provider === slot.provider &&
    settings.default_profile?.auth_method === slot.auth_method
  );
}

interface ModelInfo {
  id: string;
  display_name: string;
}

interface ModelListResult {
  models: ModelInfo[];
  error: string | null;
}

type LogRole = "user" | "assistant" | "error";

interface LogEntry {
  role: LogRole;
  content: string;
  timestamp: string;
}

type NotReadyReason = "disabled" | "no_provider" | "disclosure_pending";

interface ChatStateSnapshot {
  entries: LogEntry[];
  is_pending: boolean;
  partial_text: string;
  ai_ready: boolean;
  not_ready_reason: NotReadyReason | null;
}

type ChatEvent =
  | { type: "chunk"; delta: string }
  | { type: "done"; content: string }
  | { type: "error"; message: string };

interface PendingConfirmationItem {
  id: string;
  tool_name: string;
  summary: string;
  allows_remember: boolean;
}

interface ToolConfirmationResponse {
  id: string;
  approved: boolean;
  remember: boolean;
}

const UI_LANGUAGES = ["zh-hant", "zh-hans", "en", "ja", "ko"];

let dict: Dictionary = {};

function t(key: string, vars?: Record<string, string | number>): string {
  let s = dict[key] ?? key;
  if (vars) {
    for (const [k, v] of Object.entries(vars)) s = s.replace(`{${k}}`, String(v));
  }
  return s;
}

async function loadDictionary(): Promise<void> {
  const raw = await invoke<string>("locale_dictionary");
  dict = JSON.parse(raw) as Dictionary;
}

type Tab = "personalization" | "ai" | "update" | "about";
let activeTab: Tab = "personalization";

interface UpdateInfo {
  version: string;
  body: string | null;
}

// Whatever the startup check (updater.rs, task 14.2) already found, if
// anything -- read once at load via `pending_update` (no network
// request of its own) so the update tab can show it immediately
// instead of re-checking GitHub a second time.
let pendingUpdate: UpdateInfo | null = null;

function applyWindowOpacity(opacity: number): void {
  document.querySelector<HTMLDivElement>("#app")!.style.opacity = String(opacity);
}

// Shared by both windows (settings-ui's "Settings window opacity" /
// ai-chat's "Chat window opacity"): fetch the currently configured
// value once on load, then stay in sync with the personalization
// tab's slider via a broadcast event rather than each window polling.
async function initWindowOpacity(): Promise<void> {
  applyWindowOpacity(await invoke<number>("get_window_opacity"));
  await listen<number>("opacity-changed", (event) => applyWindowOpacity(event.payload));
}

async function main(): Promise<void> {
  // The chat window shares this same index.html/main.ts entry point
  // (chat_window.rs's own comment explains why: WebviewUrl::App is
  // only reliable for the literal "index.html" path with this
  // project's custom-protocol setup) -- branch on the window's own
  // label rather than trying to ship a second HTML page.
  if (getCurrentWindow().label === "chat") {
    await mainChat();
    return;
  }
  // The status bubble shares the same entry point too -- without this
  // branch it fell through to the settings UI below, rendered (barely
  // visibly) inside the bubble's tiny 180x36 window instead of the
  // "...", success, or failure glyph it's actually meant to show.
  if (getCurrentWindow().label === "status-bubble") {
    await mainStatusBubble();
    return;
  }
  // The tool-confirmation popup shares the same entry point too
  // (`agent-core-and-task-router`'s Group 6) -- same reason as the
  // chat window and status bubble above.
  if (getCurrentWindow().label === "tool-confirmation") {
    await mainToolConfirmation();
    return;
  }

  try {
    await loadDictionary();
    pendingUpdate = await invoke<UpdateInfo | null>("pending_update");
    if (pendingUpdate) {
      activeTab = "update";
    }
    await render();
    await initWindowOpacity();

    // Only relevant if the startup check finds an update *after* this
    // window is already open (pendingUpdate above only covers the case
    // where it was found before the window existed).
    await listen<Tab>("switch-tab", (event) => {
      activeTab = event.payload;
      applyActiveTab();
    });
  } catch (err) {
    // Nothing above renders anything on its own failure -- without
    // this, a thrown error here (e.g. an invoke() rejection) would
    // leave a completely blank window with no trace of why.
    console.error("settings window failed to initialize:", err);
    document.querySelector<HTMLDivElement>("#app")!.innerHTML =
      `<p class="error">${String(err)}</p>`;
  }
}

async function render(): Promise<void> {
  const app = document.querySelector<HTMLDivElement>("#app")!;
  app.innerHTML = `
    <main class="container-fluid settings-window">
      <nav class="tabs">
        <button class="tab-button" data-tab="personalization">${t("settings.tab.personalization")}</button>
        <button class="tab-button" data-tab="ai">${t("settings.tab.ai")}</button>
        <button class="tab-button" data-tab="update">${t("settings.tab.update")}</button>
        <button class="tab-button" data-tab="about">${t("settings.tab.about")}</button>
      </nav>
      <section class="panel" data-panel="personalization"></section>
      <section class="panel" data-panel="ai"></section>
      <section class="panel" data-panel="update"></section>
      <section class="panel" data-panel="about"></section>
    </main>
  `;

  for (const button of app.querySelectorAll<HTMLButtonElement>(".tab-button")) {
    button.addEventListener("click", () => {
      activeTab = button.dataset.tab as Tab;
      applyActiveTab();
      // `subscription-first-chat`'s "Provider status display" requires
      // status to be checked fresh "when the AI tab is opened," not
      // only once when the whole settings window first opened -- a
      // profile's CLI could have logged out or hit its quota while the
      // user was looking at a different tab. A full `renderAi()` would
      // also discard any in-progress, unsaved edits in the row's own
      // inputs, so only the status (and its login-button visibility)
      // is re-checked, not the whole panel.
      if (activeTab === "ai") {
        const panel = document.querySelector<HTMLElement>('[data-panel="ai"]');
        if (panel) {
          for (const slot of PROFILE_SLOTS) void refreshProfileStatus(panel, slot);
        }
      }
    });
  }

  // Select the default tab immediately, before the panels' content has
  // loaded -- each panel below fills in independently (and reports its
  // own error rather than leaving the window blank if one of them
  // fails), so tab selection must not wait on all three to succeed.
  applyActiveTab();

  await Promise.allSettled([
    renderPersonalization().catch((err) => renderError("personalization", err)),
    renderAi().catch((err) => renderError("ai", err)),
    renderUpdate().catch((err) => renderError("update", err)),
    renderAbout().catch((err) => renderError("about", err)),
  ]);
}

function renderError(tab: Tab, err: unknown): void {
  const panel = document.querySelector<HTMLElement>(`[data-panel="${tab}"]`)!;
  console.error(`failed to render ${tab} tab:`, err);
  panel.innerHTML = `<p class="error">${String(err)}</p>`;
}

function applyActiveTab(): void {
  for (const button of document.querySelectorAll<HTMLButtonElement>(".tab-button")) {
    button.classList.toggle("active", button.dataset.tab === activeTab);
  }
  for (const panel of document.querySelectorAll<HTMLElement>(".panel")) {
    panel.classList.toggle("active", panel.dataset.panel === activeTab);
  }
}

function field(labelText: string, controlHtml: string): string {
  return `<label class="field-row"><span>${labelText}</span>${controlHtml}</label>`;
}

async function renderPersonalization(): Promise<void> {
  const panel = document.querySelector<HTMLElement>('[data-panel="personalization"]')!;
  const snapshot = await invoke<PersonalizationSnapshot>("get_personalization");
  const {
    config,
    scale_options,
    opacity_options,
    monitor_count,
    voice_languages_with_clips,
    config_path,
  } = snapshot;

  const scaleOptionsHtml = scale_options
    .map(
      (v, i) =>
        `<option value="${i}" ${i === config.scale_index ? "selected" : ""}>${v.toFixed(1)}x</option>`,
    )
    .join("");
  const opacityOptionsHtml = opacity_options
    .map(
      (v, i) =>
        `<option value="${i}" ${i === config.transparency_index ? "selected" : ""}>${Math.round(v * 100)}%</option>`,
    )
    .join("");
  const displayPriorityOptionsHtml = (
    [
      [1, "personalization.display_priority.topmost"],
      [2, "personalization.display_priority.fullscreen_hide"],
      [3, "personalization.display_priority.desktop_only"],
    ] as const
  )
    .map(
      ([value, key]) =>
        `<option value="${value}" ${value === config.display_priority ? "selected" : ""}>${t(key)}</option>`,
    )
    .join("");
  const wanderOptionsHtml = (
    [
      [0, "personalization.wander_stay_mode.always_move"],
      [1, "personalization.wander_stay_mode.probabilistic"],
      [2, "personalization.wander_stay_mode.stationary"],
    ] as const
  )
    .map(
      ([value, key]) =>
        `<option value="${value}" ${value === config.wander_idle_stay_mode ? "selected" : ""}>${t(key)}</option>`,
    )
    .join("");
  const monitorOptionsHtml = Array.from({ length: monitor_count }, (_, i) => i)
    .map(
      (i) =>
        `<option value="${i}" ${i === config.screen_index ? "selected" : ""}>${t("personalization.monitor.numbered", { index: i })}</option>`,
    )
    .join("");
  const uiLanguageOptionsHtml = UI_LANGUAGES.map(
    (lang) =>
      `<option value="${lang}" ${lang === config.ui_language ? "selected" : ""}>${t(`language.${lang.replace("-", "_")}`)}</option>`,
  ).join("");
  const voiceLanguageOptionsHtml = voice_languages_with_clips
    .map(
      (lang) =>
        `<option value="${lang}" ${lang === config.voice_language ? "selected" : ""}>${t(`language.${lang}`)}</option>`,
    )
    .join("");

  panel.innerHTML = `
    ${field(t("personalization.scale_label"), `<select id="scale-select">${scaleOptionsHtml}</select>`)}
    ${field(t("personalization.opacity_label"), `<select id="opacity-select">${opacityOptionsHtml}</select>`)}
    ${field(t("personalization.display_priority_label"), `<select id="display-priority-select">${displayPriorityOptionsHtml}</select>`)}
    ${field(t("personalization.wander_stay_mode_label"), `<select id="wander-select">${wanderOptionsHtml}</select>`)}
    ${field(t("personalization.monitor.all_screens"), `<input type="checkbox" id="all-screens-checkbox" ${config.total_screen ? "checked" : ""} />`)}
    ${field(t("personalization.monitor_label"), `<select id="monitor-select" ${config.total_screen ? "disabled" : ""}>${monitorOptionsHtml}</select>`)}
    ${field(t("personalization.window_snap_label"), `<input type="checkbox" id="window-snap-checkbox" ${config.window_snap ? "checked" : ""} />`)}
    ${field(t("personalization.instance_count_label"), `<input type="number" id="instance-count-input" min="1" max="80" value="${config.instance_count}" />`)}
    <p class="hint">${t("personalization.instance_count_warning", { path: config_path })}</p>
    ${field(t("personalization.autostart_label"), `<input type="checkbox" id="autostart-checkbox" ${config.auto_startup ? "checked" : ""} />`)}
    ${field(t("personalization.ui_language_label"), `<select id="ui-language-select">${uiLanguageOptionsHtml}</select>`)}
    ${field(t("personalization.voice_enabled_label"), `<input type="checkbox" id="voice-enabled-checkbox" ${config.voice_enabled ? "checked" : ""} />`)}
    ${field(t("personalization.voice_volume_label"), `<input type="range" id="voice-volume-input" min="0" max="150" value="${config.voice_volume}" />`)}
    ${field(t("personalization.voice_language_label"), `<select id="voice-language-select">${voiceLanguageOptionsHtml}</select>`)}
  `;

  const byId = <T extends HTMLElement>(id: string) => panel.querySelector<T>(`#${id}`)!;

  byId<HTMLSelectElement>("scale-select").addEventListener("change", (e) => {
    invoke("set_scale_index", { index: Number((e.target as HTMLSelectElement).value) });
  });
  byId<HTMLSelectElement>("opacity-select").addEventListener("change", (e) => {
    invoke("set_opacity_index", { index: Number((e.target as HTMLSelectElement).value) });
  });
  byId<HTMLSelectElement>("display-priority-select").addEventListener("change", (e) => {
    invoke("set_display_priority", { mode: Number((e.target as HTMLSelectElement).value) });
  });
  byId<HTMLSelectElement>("wander-select").addEventListener("change", (e) => {
    invoke("set_wander_stay_mode", { mode: Number((e.target as HTMLSelectElement).value) });
  });
  byId<HTMLInputElement>("all-screens-checkbox").addEventListener("change", (e) => {
    const enabled = (e.target as HTMLInputElement).checked;
    byId<HTMLSelectElement>("monitor-select").disabled = enabled;
    invoke("set_total_screen", { enabled });
  });
  byId<HTMLSelectElement>("monitor-select").addEventListener("change", (e) => {
    invoke("set_monitor_index", { index: Number((e.target as HTMLSelectElement).value) });
  });
  byId<HTMLInputElement>("window-snap-checkbox").addEventListener("change", (e) => {
    invoke("set_window_snap", { enabled: (e.target as HTMLInputElement).checked });
  });
  byId<HTMLInputElement>("instance-count-input").addEventListener("change", (e) => {
    invoke("set_instance_count", { count: Number((e.target as HTMLInputElement).value) });
  });
  byId<HTMLInputElement>("autostart-checkbox").addEventListener("change", (e) => {
    invoke("set_auto_startup", { enabled: (e.target as HTMLInputElement).checked });
  });
  byId<HTMLSelectElement>("ui-language-select").addEventListener("change", async (e) => {
    const language = (e.target as HTMLSelectElement).value;
    await invoke("set_ui_language", { language });
    // Localization spec: language changes apply with an immediate UI
    // refresh rather than requiring a restart.
    await loadDictionary();
    await render();
  });
  byId<HTMLInputElement>("voice-enabled-checkbox").addEventListener("change", (e) => {
    invoke("set_voice_enabled", { enabled: (e.target as HTMLInputElement).checked });
  });
  byId<HTMLInputElement>("voice-volume-input").addEventListener("change", (e) => {
    invoke("set_voice_volume", { percent: Number((e.target as HTMLInputElement).value) });
  });
  byId<HTMLSelectElement>("voice-language-select").addEventListener("change", (e) => {
    invoke("set_voice_language", { language: (e.target as HTMLSelectElement).value });
  });
}

async function renderAi(): Promise<void> {
  const panel = document.querySelector<HTMLElement>('[data-panel="ai"]')!;
  const snapshot = await invoke<AiSettingsSnapshot>("get_ai_settings");
  renderAiPanel(panel, snapshot);
}

function renderAiPanel(panel: HTMLElement, snapshot: AiSettingsSnapshot): void {
  const { settings, persona_warning } = snapshot;

  const rowsHtml = PROFILE_SLOTS.map((slot) => renderProfileRow(snapshot, slot)).join("");
  const enabledSlots = PROFILE_SLOTS.filter((slot) => findProfile(settings, slot) !== undefined);
  const defaultOptionsHtml = enabledSlots.length
    ? enabledSlots
        .map((slot) => {
          const key = slotKey(slot);
          return `<option value="${key}" ${isDefaultProfile(settings, slot) ? "selected" : ""}>${t(`ai.profile.${slot.provider}.${slot.auth_method}`)}</option>`;
        })
        .join("")
    : `<option value="">${t("ai.default.none_enabled_option")}</option>`;

  const taskRouterModeOptionsHtml = (["single", "mix"] as const satisfies TaskRouterMode[])
    .map(
      (mode) =>
        `<option value="${mode}" ${mode === settings.task_router_mode ? "selected" : ""}>${t(`ai.task_router_mode.${mode}`)}</option>`,
    )
    .join("");

  const toolAccessRowsHtml = CLAUDE_CODE_TOOL_ACCESS_KEYS.map(
    (key) =>
      `<label class="field-row ai-tool-access-row"><span>${t(`ai.tool_access.${key}`)}</span><input type="checkbox" class="ai-tool-access-checkbox" data-tool="${key}" ${settings.claude_code_tool_access[key] === "auto" ? "checked" : ""} /></label>`,
  ).join("");

  panel.innerHTML = `
    ${field(t("ai.enabled_label"), `<input type="checkbox" id="ai-enabled-checkbox" ${settings.ai_enabled ? "checked" : ""} />`)}
    <fieldset class="ai-section">
      <legend>${t("ai.profiles_section_title")}</legend>
      <div id="ai-profiles">${rowsHtml}</div>
    </fieldset>
    <fieldset class="ai-section">
      <legend>${t("ai.default_section_title")}</legend>
      ${field(t("ai.default_label"), `<select id="ai-default-select" ${enabledSlots.length ? "" : "disabled"}>${defaultOptionsHtml}</select>`)}
    </fieldset>
    <fieldset class="ai-section">
      <legend>${t("ai.task_router_section_title")}</legend>
      ${field(t("ai.task_router_mode_label"), `<select id="ai-task-router-mode-select">${taskRouterModeOptionsHtml}</select>`)}
    </fieldset>
    <fieldset class="ai-section">
      <legend>${t("ai.project_root_section_title")}</legend>
      <p class="hint">${t("ai.project_root.hint")}</p>
      ${field(t("ai.project_root_label"), `<span id="ai-project-root-value">${settings.project_root ?? t("ai.project_root.not_configured")}</span>`)}
      <button type="button" id="ai-project-root-choose-button" class="secondary">${t("ai.project_root.choose_button")}</button>
      <button type="button" id="ai-project-root-clear-button" class="secondary" ${settings.project_root ? "" : "disabled"}>${t("ai.project_root.clear_button")}</button>
    </fieldset>
    <fieldset class="ai-section">
      <legend>${t("ai.tool_access_section_title")}</legend>
      <p class="hint">${t("ai.tool_access.hint")}</p>
      ${toolAccessRowsHtml}
    </fieldset>
    ${persona_warning ? `<p class="error">${t("ai.persona_warning", { error: persona_warning })}</p>` : ""}
  `;

  panel.querySelector<HTMLInputElement>("#ai-enabled-checkbox")!.addEventListener("change", (e) => {
    invoke("set_ai_enabled", { enabled: (e.target as HTMLInputElement).checked });
  });

  const defaultSelect = panel.querySelector<HTMLSelectElement>("#ai-default-select")!;
  defaultSelect.addEventListener("change", async () => {
    if (!defaultSelect.value) return;
    const slot = parseSlotKey(defaultSelect.value);
    const ok = await invoke<boolean>("set_default_profile", {
      provider: slot.provider,
      authMethod: slot.auth_method,
    });
    if (!ok)
      defaultSelect.value = settings.default_profile ? slotKey(settings.default_profile) : "";
    await renderAi();
  });

  panel
    .querySelector<HTMLSelectElement>("#ai-task-router-mode-select")!
    .addEventListener("change", (e) => {
      invoke("set_task_router_mode", { mode: (e.target as HTMLSelectElement).value });
    });

  panel
    .querySelector<HTMLButtonElement>("#ai-project-root-choose-button")!
    .addEventListener("click", async () => {
      const selected = await openFolderPicker({ directory: true, multiple: false });
      if (!selected) return;
      await invoke("set_project_root", { path: selected });
      await renderAi();
    });

  panel
    .querySelector<HTMLButtonElement>("#ai-project-root-clear-button")!
    .addEventListener("click", async () => {
      await invoke("set_project_root", { path: "" });
      await renderAi();
    });

  for (const checkbox of panel.querySelectorAll<HTMLInputElement>(".ai-tool-access-checkbox")) {
    checkbox.addEventListener("change", () => {
      const key = checkbox.dataset.tool as keyof ClaudeCodeToolAccess;
      const toolAccess: ClaudeCodeToolAccess = {
        ...settings.claude_code_tool_access,
        [key]: checkbox.checked ? "auto" : "deny",
      };
      invoke("set_claude_code_tool_access", { toolAccess });
    });
  }

  for (const slot of PROFILE_SLOTS) {
    wireProfileRow(panel, slot);
    void refreshProfileStatus(panel, slot);
  }
}

function renderProfileRow(
  snapshot: AiSettingsSnapshot,
  slot: { provider: ProviderKind; auth_method: AuthMethod; experimental: boolean },
): string {
  const { settings } = snapshot;
  const profile = findProfile(settings, slot);
  const enabled = profile !== undefined;
  const key = slotKey(slot);

  const needsKey = slot.auth_method === "api_key";
  const needsBaseUrl = slot.auth_method !== "subscription";
  const keySet =
    slot.provider === "open_ai"
      ? snapshot.openai_key_set
      : slot.provider === "anthropic"
        ? snapshot.anthropic_key_set
        : true;

  const configHtml = enabled
    ? `
      ${field(t("ai.profile_enabled_label"), `<input type="checkbox" class="ai-profile-enable" data-slot="${key}" checked />`)}
      ${
        needsKey
          ? field(
              t("ai.api_key_label"),
              `<input type="password" class="ai-api-key-input" data-slot="${key}" placeholder="${keySet ? t("ai.api_key.set_placeholder") : t("ai.api_key.unset_placeholder")}" />`,
            )
          : ""
      }
      ${
        needsBaseUrl
          ? field(
              t("ai.base_url_label"),
              `<input type="text" class="ai-base-url-input" data-slot="${key}" placeholder="${t("ai.base_url.default_placeholder")}" value="${profile?.base_url ?? ""}" />`,
            )
          : ""
      }
      ${field(t("ai.model_label"), `<input type="text" class="ai-model-input" data-slot="${key}" list="ai-model-list-${key}" value="${profile?.model ?? ""}" />`)}
      <datalist id="ai-model-list-${key}"></datalist>
      <button type="button" class="ai-fetch-models-button secondary" data-slot="${key}">${t("ai.fetch_models_button")}</button>
      <div class="ai-fetch-models-result" data-slot="${key}"></div>
    `
    : field(
        t("ai.profile_enabled_label"),
        `<input type="checkbox" class="ai-profile-enable" data-slot="${key}" />`,
      );

  return `
    <details class="ai-profile-row" data-slot="${key}" ${enabled ? "open" : ""}>
      <summary>
        <span class="ai-profile-name">${t(`ai.profile.${slot.provider}.${slot.auth_method}`)}</span>
        ${slot.experimental ? `<span class="badge-experimental">${t("ai.experimental_badge")}</span>` : ""}
        <span class="ai-profile-status" data-slot="${key}">${t("ai.status.checking")}</span>
        <button type="button" class="ai-profile-login-button secondary" data-slot="${key}" hidden>${t("ai.login_button")}</button>
      </summary>
      <div class="ai-profile-body" data-slot="${key}">
        <div class="ai-disclosure" data-slot="${key}"></div>
        ${configHtml}
      </div>
    </details>
  `;
}

function wireProfileRow(
  panel: HTMLElement,
  slot: { provider: ProviderKind; auth_method: AuthMethod; experimental: boolean },
): void {
  const key = slotKey(slot);
  const row = panel.querySelector<HTMLElement>(`.ai-profile-row[data-slot="${key}"]`)!;

  row
    .querySelector<HTMLInputElement>(".ai-profile-enable")!
    .addEventListener("change", async (e) => {
      const checkbox = e.target as HTMLInputElement;
      if (!checkbox.checked) {
        await invoke("disable_profile", { provider: slot.provider, authMethod: slot.auth_method });
        await renderAi();
        return;
      }

      // `enable_profile` itself is the source of truth for whether this
      // (provider, auth method) still needs its disclosure acknowledged --
      // the frontend doesn't try to track that (see
      // `AiSettings::acknowledged_disclosures`'s backend doc comment for
      // why: it must survive a disable/re-enable cycle, which the
      // frontend has no visibility into). A `false` return means refused.
      const enabled = await invoke<boolean>("enable_profile", {
        provider: slot.provider,
        authMethod: slot.auth_method,
      });
      if (enabled) {
        await renderAi();
        return;
      }

      const disclosureEl = row.querySelector<HTMLElement>(`.ai-disclosure[data-slot="${key}"]`)!;
      disclosureEl.innerHTML = `
      <p class="hint">${t(`ai.disclosure.${slot.provider}.${slot.auth_method}`)}</p>
      <button type="button" class="ai-disclosure-accept">${t("ai.disclosure.accept")}</button>
      <button type="button" class="ai-disclosure-cancel secondary">${t("ai.disclosure.cancel")}</button>
    `;
      disclosureEl
        .querySelector<HTMLButtonElement>(".ai-disclosure-accept")!
        .addEventListener("click", async () => {
          await invoke("acknowledge_profile_disclosure", {
            provider: slot.provider,
            authMethod: slot.auth_method,
          });
          await invoke("enable_profile", { provider: slot.provider, authMethod: slot.auth_method });
          await renderAi();
        });
      disclosureEl
        .querySelector<HTMLButtonElement>(".ai-disclosure-cancel")!
        .addEventListener("click", () => {
          checkbox.checked = false;
          disclosureEl.innerHTML = "";
        });
    });

  row
    .querySelector<HTMLButtonElement>(".ai-profile-login-button")
    ?.addEventListener("click", async (e) => {
      // The button lives inside <summary>, whose native click behavior
      // toggles the enclosing <details> -- without this, clicking "Log
      // in" would also collapse/expand the row.
      e.preventDefault();
      e.stopPropagation();
      const button = e.currentTarget as HTMLButtonElement;
      button.disabled = true;
      try {
        await invoke<boolean>("trigger_profile_login", {
          provider: slot.provider,
          authMethod: slot.auth_method,
        });
      } catch (err) {
        button.title = String(err);
      } finally {
        button.disabled = false;
        // Re-check status right away -- if the CLI's login flow
        // completed instantly (unlikely, but possible for an
        // already-valid-but-stale session) this reflects it without
        // waiting for the next full tab render; otherwise it just
        // leaves the button visible for another attempt.
        void refreshProfileStatus(panel, slot);
      }
    });

  row.querySelector<HTMLInputElement>(".ai-api-key-input")?.addEventListener("change", (e) => {
    const value = (e.target as HTMLInputElement).value;
    if (value) void invoke("set_provider_api_key", { provider: slot.provider, apiKey: value });
  });

  row.querySelector<HTMLInputElement>(".ai-base-url-input")?.addEventListener("change", (e) => {
    invoke("set_profile_base_url", {
      provider: slot.provider,
      authMethod: slot.auth_method,
      baseUrl: (e.target as HTMLInputElement).value,
    });
  });

  row.querySelector<HTMLInputElement>(".ai-model-input")?.addEventListener("change", (e) => {
    invoke("set_profile_model", {
      provider: slot.provider,
      authMethod: slot.auth_method,
      model: (e.target as HTMLInputElement).value,
    });
  });

  const resultEl = row.querySelector<HTMLElement>(`.ai-fetch-models-result[data-slot="${key}"]`);
  row
    .querySelector<HTMLButtonElement>(".ai-fetch-models-button")
    ?.addEventListener("click", async (e) => {
      const button = e.target as HTMLButtonElement;
      button.disabled = true;
      resultEl!.innerHTML = `<p>${t("ai.fetch_models.loading")}</p>`;
      try {
        const result = await invoke<ModelListResult>("fetch_provider_models", {
          provider: slot.provider,
          authMethod: slot.auth_method,
        });
        if (result.error) {
          resultEl!.innerHTML = `<p class="error">${t("ai.fetch_models.error", { error: result.error })}</p>`;
        } else {
          const datalist = row.querySelector<HTMLDataListElement>(`#ai-model-list-${key}`)!;
          datalist.innerHTML = result.models
            .map((m) => `<option value="${m.id}">${m.display_name}</option>`)
            .join("");
          resultEl!.innerHTML = `<p>${t("ai.fetch_models.success", { count: result.models.length })}</p>`;
        }
      } catch (err) {
        resultEl!.innerHTML = `<p class="error">${String(err)}</p>`;
      } finally {
        button.disabled = false;
      }
    });
}

async function refreshProfileStatus(
  panel: HTMLElement,
  slot: { provider: ProviderKind; auth_method: AuthMethod },
): Promise<void> {
  try {
    const status = await invoke<ProfileStatus>("check_profile_status", {
      provider: slot.provider,
      authMethod: slot.auth_method,
    });
    // The panel may already have been re-rendered (e.g. the user toggled
    // something else while this was in flight) -- a missing element just
    // means this result is stale and there's nothing to update.
    const key = slotKey(slot);
    const el = panel.querySelector<HTMLElement>(`.ai-profile-status[data-slot="${key}"]`);
    if (!el) return;
    el.textContent = t(`ai.status.${status.state}`);
    el.className = `ai-profile-status ai-profile-status--${status.state}`;
    if ("detail" in status) el.title = status.detail;

    // An *enabled* profile whose acknowledgement was cleared (its
    // disclosure text changed): show the updated disclosure right here,
    // with the one action that resolves it. Chat is blocked for this
    // profile until then (`chat_readiness`), so it must not be a bare
    // status label with no way forward.
    if (status.state === "disclosure_pending") {
      const disclosureEl = panel.querySelector<HTMLElement>(`.ai-disclosure[data-slot="${key}"]`);
      if (disclosureEl && !disclosureEl.querySelector(".ai-disclosure-accept")) {
        disclosureEl.innerHTML = `
          <p class="hint">${t(`ai.disclosure.${slot.provider}.${slot.auth_method}`)}</p>
          <button type="button" class="ai-disclosure-accept">${t("ai.disclosure.accept")}</button>
        `;
        disclosureEl
          .querySelector<HTMLButtonElement>(".ai-disclosure-accept")!
          .addEventListener("click", async () => {
            await invoke("acknowledge_profile_disclosure", {
              provider: slot.provider,
              authMethod: slot.auth_method,
            });
            await renderAi();
          });
      }
    }

    // Only offer the login-trigger button for the one state it actually
    // applies to (`subscription-first-chat`'s "CLI installed but not
    // logged in" scenario) -- every other state either doesn't need a
    // login at all or isn't fixable by triggering one (e.g. a missing
    // CLI binary).
    const loginButton = panel.querySelector<HTMLButtonElement>(
      `.ai-profile-login-button[data-slot="${key}"]`,
    );
    if (loginButton) loginButton.hidden = status.state !== "not_logged_in";
  } catch {
    // Best-effort only -- leave the "checking..." placeholder in place.
  }
}

async function renderUpdate(): Promise<void> {
  const panel = document.querySelector<HTMLElement>('[data-panel="update"]')!;
  const version = await getVersion();
  const snapshot = await invoke<PersonalizationSnapshot>("get_personalization");

  panel.innerHTML = `
    <p>${t("update.current_version_label", { version })}</p>
    <button id="update-check-button">${t("update.check_button")}</button>
    <div id="update-result"></div>
    ${field(t("update.skip_all_updates"), `<input type="checkbox" id="skip-updates-checkbox" ${snapshot.config.skip_updates ? "checked" : ""} />`)}
  `;

  panel
    .querySelector<HTMLInputElement>("#skip-updates-checkbox")!
    .addEventListener("change", (e) => {
      invoke("set_skip_updates", { enabled: (e.target as HTMLInputElement).checked });
    });

  const resultEl = panel.querySelector<HTMLElement>("#update-result")!;
  const checkButton = panel.querySelector<HTMLButtonElement>("#update-check-button")!;
  checkButton.addEventListener("click", () => checkForUpdate(resultEl, checkButton));

  // The startup check (14.2) may have already found this before the
  // window opened -- show it directly rather than hitting GitHub again.
  if (pendingUpdate) {
    showUpdateResult(resultEl, pendingUpdate);
  }
}

async function checkForUpdate(
  resultEl: HTMLElement,
  checkButton: HTMLButtonElement,
): Promise<void> {
  checkButton.disabled = true;
  resultEl.innerHTML = `<p>${t("update.checking")}</p>`;
  try {
    const update = await invoke<UpdateInfo | null>("check_for_update");
    showUpdateResult(resultEl, update);
  } catch (err) {
    console.error("update check failed:", err);
    resultEl.innerHTML = `<p class="error">${t("update.error")}</p>`;
  } finally {
    checkButton.disabled = false;
  }
}

function showUpdateResult(resultEl: HTMLElement, update: UpdateInfo | null): void {
  if (!update) {
    resultEl.innerHTML = `<p>${t("update.up_to_date")}</p>`;
    return;
  }
  resultEl.innerHTML = `
    <p>${t("update.latest_version_label", { version: update.version })}</p>
    ${update.body ? `<p>${update.body}</p>` : ""}
    <button id="update-install-button">${t("update.install_button")}</button>
    <button id="update-skip-version-button" class="secondary">${t("update.skip_this_version")}</button>
  `;
  resultEl
    .querySelector<HTMLButtonElement>("#update-install-button")!
    .addEventListener("click", async (e) => {
      (e.target as HTMLButtonElement).disabled = true;
      resultEl.insertAdjacentHTML("beforeend", `<p>${t("update.installing")}</p>`);
      try {
        // Restarts the app on success (Rust side calls AppHandle::
        // restart), so this only returns here on failure.
        await invoke("install_update");
      } catch (err) {
        console.error("update install failed:", err);
        resultEl.insertAdjacentHTML("beforeend", `<p class="error">${t("update.error")}</p>`);
      }
    });
  resultEl
    .querySelector<HTMLButtonElement>("#update-skip-version-button")!
    .addEventListener("click", async () => {
      await invoke("set_skip_version", { version: update.version });
      resultEl.innerHTML = "";
    });
}

const AMEATH_URL = "https://gitee.com/lzy-buaa-jdi/ameath";
const FUGU_URL = "https://space.bilibili.com/84508966";
const AUTHOR_URL = "https://github.com/kagetsuki1997";
const REPO_URL = "https://github.com/kagetsuki1997/fleet-snowfluff";

function externalLink(url: string, label: string): string {
  return `<a href="${url}" class="external-link">${label}</a>`;
}

async function renderAbout(): Promise<void> {
  const panel = document.querySelector<HTMLElement>('[data-panel="about"]')!;
  const [version, commit] = await Promise.all([getVersion(), invoke<string>("build_commit")]);

  panel.innerHTML = `
    <img class="app-icon" src="/app-icon.png" alt="" />
    <p class="version">${t("about.version", { version })}</p>
    <p class="build">${t("about.build", { commit })}</p>
    <hr />
    <p>${t("about.license_notice")}</p>
    <p class="disclaimer">${t("about.asset_disclaimer")}</p>
    <hr />
    <h3>${t("about.credits_heading")}</h3>
    <p>${t("about.credits_original", { ameath_link: externalLink(AMEATH_URL, "Ameath"), fugu_link: externalLink(FUGU_URL, "-fugu-") })}</p>
    <p>${t("about.credits_rewrite", { author_link: externalLink(AUTHOR_URL, "kagetsuki1997"), repo_link: externalLink(REPO_URL, "github.com/kagetsuki1997/fleet-snowfluff") })}</p>
  `;

  // Links must open in the system browser, not navigate this settings
  // window itself away to an external site.
  for (const link of panel.querySelectorAll<HTMLAnchorElement>("a.external-link")) {
    link.addEventListener("click", (e) => {
      e.preventDefault();
      openExternalLink(link.href);
    });
  }
}

function escapeHtml(text: string): string {
  const div = document.createElement("div");
  div.textContent = text;
  return div.innerHTML;
}

function entryHtml(entry: LogEntry): string {
  if (entry.role === "error") {
    return `<p class="chat-entry chat-entry-error">⚠️ ${escapeHtml(entry.content)}</p>`;
  }
  return `<p class="chat-entry chat-entry-${entry.role}">${escapeHtml(entry.content)}</p>`;
}

// Fixed literal glyphs (`ai-chat`'s "Status bubble" -- not localized
// text), matching the persona rather than the UI language.
const BUBBLE_THINKING = "...";
const BUBBLE_REPLY = "Ciallo～(∠・ω< )⌒☆";
const BUBBLE_FAILURE = "(×_×)";
const BUBBLE_POLL_MS = 500;

async function mainStatusBubble(): Promise<void> {
  document.body.classList.add("bubble-window");
  const app = document.querySelector<HTMLDivElement>("#app")!;
  app.innerHTML = `<div id="bubble" class="status-bubble"></div>`;
  const bubbleEl = app.querySelector<HTMLDivElement>("#bubble")!;

  // `status_bubble.rs`'s own `sync()` already decides whether this
  // window exists/is shown at all -- this loop only has to pick the
  // right glyph for whatever moment it's asked to render, using the
  // same snapshot the chat window already polls (no bubble-specific
  // IPC): "pending" beats everything, otherwise the most recent log
  // entry's role tells reply from failure.
  async function refresh(): Promise<void> {
    const state = await invoke<ChatStateSnapshot>("get_chat_state");
    let text = "";
    if (state.is_pending) {
      text = BUBBLE_THINKING;
    } else {
      const last = state.entries[state.entries.length - 1];
      text =
        last?.role === "error" ? BUBBLE_FAILURE : last?.role === "assistant" ? BUBBLE_REPLY : "";
    }
    bubbleEl.textContent = text;
    bubbleEl.hidden = text === "";
    setTimeout(() => void refresh(), BUBBLE_POLL_MS);
  }

  await refresh();
}

async function mainChat(): Promise<void> {
  try {
    await loadDictionary();
    await renderChatWindow();
    await initWindowOpacity();
  } catch (err) {
    console.error("chat window failed to initialize:", err);
    document.querySelector<HTMLDivElement>("#app")!.innerHTML =
      `<p class="error">${String(err)}</p>`;
  }
}

async function renderChatWindow(): Promise<void> {
  const app = document.querySelector<HTMLDivElement>("#app")!;
  app.innerHTML = `
    <main class="container-fluid chat-window">
      <div id="chat-transcript" class="chat-transcript"></div>
      <div id="chat-status"></div>
      <form id="chat-form" class="chat-form">
        <input type="text" id="chat-input" autocomplete="off" placeholder="${t("chat.input_placeholder")}" />
        <button type="submit" id="chat-send-button">${t("chat.send_button")}</button>
        <button type="button" id="chat-stop-button" class="secondary" hidden>${t("chat.stop_button")}</button>
      </form>
      <button type="button" id="chat-new-button" class="secondary outline">${t("chat.new_chat_button")}</button>
    </main>
  `;

  const transcriptEl = app.querySelector<HTMLElement>("#chat-transcript")!;
  const statusEl = app.querySelector<HTMLElement>("#chat-status")!;
  const form = app.querySelector<HTMLFormElement>("#chat-form")!;
  const input = app.querySelector<HTMLInputElement>("#chat-input")!;
  const sendButton = app.querySelector<HTMLButtonElement>("#chat-send-button")!;
  const stopButton = app.querySelector<HTMLButtonElement>("#chat-stop-button")!;
  const newButton = app.querySelector<HTMLButtonElement>("#chat-new-button")!;

  // The canonical transcript, replaced wholesale by every `refresh()`.
  // A just-sent message is appended here optimistically (not through a
  // refresh) so the streamed reply's partial text never has to
  // reconcile against a server snapshot that might already be ahead of
  // it -- see chat_commands.rs's own note on this simplification.
  let committedEntries: LogEntry[] = [];

  function renderTranscript(pendingText: string | null): void {
    const pendingHtml =
      pendingText !== null
        ? `<p class="chat-entry chat-entry-assistant chat-entry-pending">${escapeHtml(pendingText)}</p>`
        : "";
    transcriptEl.innerHTML = committedEntries.map(entryHtml).join("") + pendingHtml;
    transcriptEl.scrollTop = transcriptEl.scrollHeight;
  }

  function setPendingUi(pending: boolean): void {
    input.disabled = pending;
    sendButton.hidden = pending;
    stopButton.hidden = !pending;
  }

  function updateReadiness(ready: boolean, reason: NotReadyReason | null): void {
    if (!ready) {
      statusEl.innerHTML = reason ? `<p class="hint">${t(`chat.not_ready.${reason}`)}</p>` : "";
      input.disabled = true;
      sendButton.disabled = true;
    } else {
      statusEl.innerHTML = "";
      sendButton.disabled = false;
    }
  }

  // A generation that outlives this window (started before it was
  // (re)opened, or before a close/reopen) has no channel this window
  // instance ever attached to -- Tauri's Channel<T> is scoped to the
  // invocation that created it, with no reattachment mechanism. Rather
  // than inventing one, a still-pending state just polls this snapshot
  // until it resolves; the common case (window stays open throughout)
  // never touches this path at all, since the channel handles it live.
  async function refresh(): Promise<void> {
    const state = await invoke<ChatStateSnapshot>("get_chat_state");
    committedEntries = state.entries;
    renderTranscript(state.is_pending ? state.partial_text : null);
    setPendingUi(state.is_pending);
    updateReadiness(state.ai_ready, state.not_ready_reason);
    if (state.is_pending) {
      setTimeout(() => void refresh(), 1000);
    }
  }

  form.addEventListener("submit", (e) => {
    e.preventDefault();
    const message = input.value.trim();
    if (!message || sendButton.disabled) return;
    input.value = "";

    committedEntries = [
      ...committedEntries,
      { role: "user", content: message, timestamp: new Date().toISOString() },
    ];
    renderTranscript("");
    setPendingUi(true);

    const channel = new Channel<ChatEvent>();
    let partial = "";
    channel.onmessage = (event) => {
      if (event.type === "chunk") {
        partial += event.delta;
        renderTranscript(partial);
      } else {
        // "done" and "error" both resolve to the same next step: the
        // backend has already appended the final entry (assistant
        // reply or error) to the session log, so re-fetch the
        // canonical state rather than trying to reconstruct it here.
        void refresh();
      }
    };

    invoke("send_chat_message", { channel, message }).catch((err: unknown) => {
      setPendingUi(false);
      statusEl.innerHTML = `<p class="error">${String(err)}</p>`;
    });
  });

  stopButton.addEventListener("click", () => {
    void invoke("stop_generation").then(() => refresh());
  });

  newButton.addEventListener("click", () => {
    void invoke("new_chat_session").then(() => refresh());
  });

  await refresh();
}

// Built with the DOM API rather than an `innerHTML` template for the
// per-item rows specifically -- `tool_name`/`summary` come from a tool
// call's own arguments (model-influenced, not authored content like
// every other rendered string in this file), so this avoids ever
// having to escape them for HTML injection at all, and keeps a live
// reference to each row's own checkboxes instead of re-querying by an
// interpolated id later.
async function mainToolConfirmation(): Promise<void> {
  await loadDictionary();
  const app = document.querySelector<HTMLDivElement>("#app")!;
  const items = await invoke<PendingConfirmationItem[]>("get_pending_tool_confirmations");

  app.innerHTML = `
    <main class="container-fluid tool-confirmation-window">
      <h3>${t("tool_confirmation.heading")}</h3>
      <div id="tool-confirmation-list"></div>
      <div class="tool-confirmation-actions">
        <button type="button" id="tool-confirmation-approve-all" class="secondary">${t("tool_confirmation.approve_all")}</button>
        <button type="button" id="tool-confirmation-deny-all" class="secondary outline">${t("tool_confirmation.deny_all")}</button>
      </div>
      <button type="button" id="tool-confirmation-submit">${t("tool_confirmation.submit")}</button>
    </main>
  `;

  const list = app.querySelector<HTMLDivElement>("#tool-confirmation-list")!;
  const rows = items.map((item) => {
    const row = document.createElement("div");
    row.className = "tool-confirmation-item";

    const approveLabel = document.createElement("label");
    const approveInput = document.createElement("input");
    approveInput.type = "checkbox";
    approveLabel.appendChild(approveInput);
    approveLabel.appendChild(document.createTextNode(` ${item.tool_name}: ${item.summary}`));
    row.appendChild(approveLabel);

    let rememberInput: HTMLInputElement | null = null;
    if (item.allows_remember) {
      const rememberLabel = document.createElement("label");
      rememberLabel.className = "remember-label";
      rememberInput = document.createElement("input");
      rememberInput.type = "checkbox";
      rememberLabel.appendChild(rememberInput);
      rememberLabel.appendChild(document.createTextNode(` ${t("tool_confirmation.remember")}`));
      row.appendChild(rememberLabel);
    }

    list.appendChild(row);
    return { item, approveInput, rememberInput };
  });

  app.querySelector("#tool-confirmation-approve-all")!.addEventListener("click", () => {
    for (const row of rows) row.approveInput.checked = true;
  });
  app.querySelector("#tool-confirmation-deny-all")!.addEventListener("click", () => {
    for (const row of rows) row.approveInput.checked = false;
  });

  // The window itself is closed from the Rust side once
  // `resolve_tool_confirmations` unblocks the waiting Agent Loop
  // (`tool_confirmation.rs`'s own `confirm_via_popup`) -- nothing to
  // do here beyond sending the batch.
  app.querySelector("#tool-confirmation-submit")!.addEventListener("click", () => {
    const responses: ToolConfirmationResponse[] = rows.map(
      ({ item, approveInput, rememberInput }) => ({
        id: item.id,
        approved: approveInput.checked,
        remember: approveInput.checked && (rememberInput?.checked ?? false),
      }),
    );
    void invoke("resolve_tool_confirmations", { responses });
  });
}

main();
