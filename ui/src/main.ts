import { Channel, invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-shell";
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

interface ProviderSettings {
  base_url: string | null;
  model: string | null;
  disclosure_acknowledged?: boolean;
}

interface AiSettings {
  ai_enabled: boolean;
  active_provider: ProviderKind | null;
  openai: ProviderSettings;
  anthropic: ProviderSettings;
  ollama: ProviderSettings;
  mock: Record<string, never>;
}

interface AiSettingsSnapshot {
  settings: AiSettings;
  openai_key_set: boolean;
  anthropic_key_set: boolean;
  persona_warning: string | null;
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

type NotReadyReason = "disabled" | "no_provider";

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

  try {
    await loadDictionary();
    pendingUpdate = await invoke<UpdateInfo | null>("pending_update");
    if (pendingUpdate) {
      activeTab = "update";
    }
    await render();

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

function providerSettingsFor(settings: AiSettings, provider: ProviderKind): ProviderSettings {
  if (provider === "open_ai") return settings.openai;
  if (provider === "anthropic") return settings.anthropic;
  if (provider === "ollama") return settings.ollama;
  return { base_url: null, model: null };
}

function needsDisclosure(settings: AiSettings, provider: ProviderKind): boolean {
  return (
    (provider === "open_ai" || provider === "anthropic") &&
    !providerSettingsFor(settings, provider).disclosure_acknowledged
  );
}

async function renderAi(): Promise<void> {
  const panel = document.querySelector<HTMLElement>('[data-panel="ai"]')!;
  const snapshot = await invoke<AiSettingsSnapshot>("get_ai_settings");
  renderAiPanel(panel, snapshot);
}

function renderAiPanel(panel: HTMLElement, snapshot: AiSettingsSnapshot): void {
  const { settings, persona_warning } = snapshot;

  const providerOptionsHtml = (
    [
      [null, "ai.provider.none"],
      ["open_ai", "ai.provider.openai"],
      ["anthropic", "ai.provider.anthropic"],
      ["ollama", "ai.provider.ollama"],
      ["mock", "ai.provider.mock"],
    ] as const
  )
    .map(
      ([value, key]) =>
        `<option value="${value ?? ""}" ${value === settings.active_provider ? "selected" : ""}>${t(key)}</option>`,
    )
    .join("");

  panel.innerHTML = `
    ${field(t("ai.enabled_label"), `<input type="checkbox" id="ai-enabled-checkbox" ${settings.ai_enabled ? "checked" : ""} />`)}
    ${field(t("ai.provider_label"), `<select id="ai-provider-select">${providerOptionsHtml}</select>`)}
    <div id="ai-disclosure"></div>
    <div id="ai-provider-config"></div>
    ${persona_warning ? `<p class="error">${t("ai.persona_warning", { error: persona_warning })}</p>` : ""}
  `;

  panel.querySelector<HTMLInputElement>("#ai-enabled-checkbox")!.addEventListener("change", (e) => {
    invoke("set_ai_enabled", { enabled: (e.target as HTMLInputElement).checked });
  });

  const providerSelect = panel.querySelector<HTMLSelectElement>("#ai-provider-select")!;
  providerSelect.addEventListener("change", () => {
    void handleProviderChange(panel, snapshot, providerSelect);
  });

  renderProviderConfig(panel, snapshot, settings.active_provider);
}

// A provider change goes through the disclosure prompt first (only for
// OpenAI/Anthropic, only until acknowledged once -- ai-provider's
// "Cloud provider data disclosure") rather than switching immediately,
// unlike every other live-apply control on this settings window.
async function handleProviderChange(
  panel: HTMLElement,
  snapshot: AiSettingsSnapshot,
  select: HTMLSelectElement,
): Promise<void> {
  const newProvider = (select.value || null) as ProviderKind | null;
  const disclosureEl = panel.querySelector<HTMLElement>("#ai-disclosure")!;

  if (newProvider && needsDisclosure(snapshot.settings, newProvider)) {
    disclosureEl.innerHTML = `
      <p class="hint">${t(`ai.disclosure.${newProvider}`)}</p>
      <button id="ai-disclosure-accept">${t("ai.disclosure.accept")}</button>
      <button id="ai-disclosure-cancel" class="secondary">${t("ai.disclosure.cancel")}</button>
    `;
    disclosureEl
      .querySelector<HTMLButtonElement>("#ai-disclosure-accept")!
      .addEventListener("click", async () => {
        await invoke("acknowledge_provider_disclosure", { provider: newProvider });
        await invoke("set_active_provider", { provider: newProvider });
        await renderAi();
      });
    disclosureEl
      .querySelector<HTMLButtonElement>("#ai-disclosure-cancel")!
      .addEventListener("click", () => {
        select.value = snapshot.settings.active_provider ?? "";
        disclosureEl.innerHTML = "";
      });
    return;
  }

  await invoke("set_active_provider", { provider: newProvider });
  await renderAi();
}

function renderProviderConfig(
  panel: HTMLElement,
  snapshot: AiSettingsSnapshot,
  provider: ProviderKind | null,
): void {
  const configEl = panel.querySelector<HTMLElement>("#ai-provider-config")!;

  if (provider === null) {
    configEl.innerHTML = `<p class="hint">${t("ai.provider.none_hint")}</p>`;
    return;
  }
  if (provider === "mock") {
    configEl.innerHTML = `<p class="hint">${t("ai.provider.mock_hint")}</p>`;
    return;
  }

  const needsKey = provider === "open_ai" || provider === "anthropic";
  const keySet = provider === "open_ai" ? snapshot.openai_key_set : snapshot.anthropic_key_set;
  const current = providerSettingsFor(snapshot.settings, provider);

  configEl.innerHTML = `
    ${
      needsKey
        ? field(
            t("ai.api_key_label"),
            `<input type="password" id="ai-api-key-input" placeholder="${keySet ? t("ai.api_key.set_placeholder") : t("ai.api_key.unset_placeholder")}" />`,
          )
        : ""
    }
    ${field(t("ai.base_url_label"), `<input type="text" id="ai-base-url-input" placeholder="${t("ai.base_url.default_placeholder")}" value="${current.base_url ?? ""}" />`)}
    ${field(t("ai.model_label"), `<input type="text" id="ai-model-input" list="ai-model-list" value="${current.model ?? ""}" />`)}
    <datalist id="ai-model-list"></datalist>
    <button id="ai-fetch-models-button" type="button">${t("ai.fetch_models_button")}</button>
    <div id="ai-fetch-models-result"></div>
  `;

  if (needsKey) {
    configEl.querySelector<HTMLInputElement>("#ai-api-key-input")!.addEventListener("change", (e) => {
      const value = (e.target as HTMLInputElement).value;
      if (value) void invoke("set_provider_api_key", { provider, apiKey: value });
    });
  }
  configEl.querySelector<HTMLInputElement>("#ai-base-url-input")!.addEventListener("change", (e) => {
    invoke("set_provider_base_url", { provider, baseUrl: (e.target as HTMLInputElement).value });
  });
  configEl.querySelector<HTMLInputElement>("#ai-model-input")!.addEventListener("change", (e) => {
    invoke("set_provider_model", { provider, model: (e.target as HTMLInputElement).value });
  });

  const resultEl = configEl.querySelector<HTMLElement>("#ai-fetch-models-result")!;
  configEl
    .querySelector<HTMLButtonElement>("#ai-fetch-models-button")!
    .addEventListener("click", async (e) => {
      const button = e.target as HTMLButtonElement;
      button.disabled = true;
      resultEl.innerHTML = `<p>${t("ai.fetch_models.loading")}</p>`;
      try {
        const result = await invoke<ModelListResult>("fetch_provider_models", { provider });
        if (result.error) {
          resultEl.innerHTML = `<p class="error">${t("ai.fetch_models.error", { error: result.error })}</p>`;
        } else {
          const datalist = configEl.querySelector<HTMLDataListElement>("#ai-model-list")!;
          datalist.innerHTML = result.models
            .map((m) => `<option value="${m.id}">${m.display_name}</option>`)
            .join("");
          resultEl.innerHTML = `<p>${t("ai.fetch_models.success", { count: result.models.length })}</p>`;
        }
      } catch (err) {
        resultEl.innerHTML = `<p class="error">${String(err)}</p>`;
      } finally {
        button.disabled = false;
      }
    });
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
      open(link.href);
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

async function mainChat(): Promise<void> {
  try {
    await loadDictionary();
    await renderChatWindow();
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

main();
