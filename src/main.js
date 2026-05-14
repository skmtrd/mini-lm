import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import DOMPurify from "dompurify";
import { marked } from "marked";
import "./styles.css";

marked.setOptions({
  breaks: true,
  gfm: true,
});

const CHAT_STORAGE_KEY = "mini-lm.chatSessions.v2";
const LEGACY_ARCHIVE_STORAGE_KEY = "mini-lm.archivedSessions.v1";
const TYPEWRITER_INTERVAL_MS = 16;
const TYPEWRITER_CHARS_PER_TICK = 12;

const state = {
  snapshot: null,
  busy: false,
  activeChats: [],
  archivedChats: [],
  currentChatId: null,
  currentChatStatus: "active",
  currentMessages: [],
  currentRunId: null,
  currentAssistantArticle: null,
  currentAssistantBody: null,
  currentAssistantRaw: "",
  currentAssistantQueued: "",
  currentAssistantHasAnswer: false,
  currentAssistantMessageId: null,
  typewriterTimer: null,
  typewriterResolvers: [],
  toastSeq: 0,
  autoScroll: true,
  lastProgressStage: "profile",
  openHistoryMenuId: null,
};

document.querySelector("#app").innerHTML = `
  <div class="shell">
    <div id="toastRegion" class="toast-region" aria-live="polite" aria-atomic="false"></div>
    <div class="window-drag-region" data-tauri-drag-region></div>
    <aside class="sidebar source-sidebar">
      <section class="source-panel">
        <div class="source-header">
          <div class="source-title-group">
            <span class="source-kicker">資料</span>
            <div id="stats" class="stats compact"></div>
          </div>
          <div class="source-tool-buttons">
            <button id="chooseSourceButton" class="source-icon-button" type="button" title="フォルダ選択" aria-label="フォルダ選択">
              <svg class="action-icon" aria-hidden="true" viewBox="0 0 24 24">
                <path d="M3 6.5A2.5 2.5 0 0 1 5.5 4H10l2 2h6.5A2.5 2.5 0 0 1 21 8.5v8A2.5 2.5 0 0 1 18.5 19h-13A2.5 2.5 0 0 1 3 16.5z"></path>
              </svg>
            </button>
            <button id="indexButton" class="source-icon-button primary-icon" type="button" title="更新" aria-label="更新">
              <svg class="action-icon" aria-hidden="true" viewBox="0 0 24 24">
                <path d="M21 12a9 9 0 0 1-15.3 6.4"></path>
                <path d="M3 12A9 9 0 0 1 18.3 5.6"></path>
                <path d="M18 2v4h-4"></path>
                <path d="M6 22v-4h4"></path>
              </svg>
            </button>
          </div>
        </div>
        <div id="sourcePath" class="path-box">未設定</div>
        <div class="source-list-head">
          <div class="source-selection-actions">
            <button id="selectAllButton" class="text-button" type="button">全選択</button>
            <button id="clearSelectionButton" class="text-button" type="button">解除</button>
          </div>
        </div>
        <div id="fileList" class="file-list"></div>
      </section>
      <div class="source-footer">
        <button id="settingsButton" class="settings-button" type="button" title="設定" aria-label="設定">
          <svg class="action-icon" aria-hidden="true" viewBox="0 0 24 24">
            <path d="M12 15.5A3.5 3.5 0 1 0 12 8a3.5 3.5 0 0 0 0 7.5z"></path>
            <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1a2 2 0 0 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.6V21a2 2 0 0 1-4 0v-.1a1.7 1.7 0 0 0-1-1.6 1.7 1.7 0 0 0-1.9.3l-.1.1A2 2 0 0 1 4.2 17l.1-.1a1.7 1.7 0 0 0 .3-1.9 1.7 1.7 0 0 0-1.6-1H3a2 2 0 0 1 0-4h.1a1.7 1.7 0 0 0 1.6-1 1.7 1.7 0 0 0-.3-1.9L4.3 7A2 2 0 0 1 7.1 4.2l.1.1a1.7 1.7 0 0 0 1.9.3 1.7 1.7 0 0 0 1-1.6V3a2 2 0 0 1 4 0v.1a1.7 1.7 0 0 0 1 1.6 1.7 1.7 0 0 0 1.9-.3l.1-.1A2 2 0 0 1 19.8 7l-.1.1a1.7 1.7 0 0 0-.3 1.9 1.7 1.7 0 0 0 1.6 1h.1a2 2 0 0 1 0 4H21a1.7 1.7 0 0 0-1.6 1z"></path>
          </svg>
          <span>設定</span>
        </button>
      </div>
    </aside>

    <main class="main">
      <div class="message-area">
        <section id="messages" class="messages"></section>
        <button id="jumpToLatestButton" class="jump-latest" type="button" hidden title="最新へ" aria-label="最新へ">
          <svg class="action-icon" aria-hidden="true" viewBox="0 0 24 24">
            <path d="M12 5v14"></path>
            <path d="M19 12l-7 7-7-7"></path>
          </svg>
        </button>
      </div>

      <form id="questionForm" class="composer">
        <textarea
          id="questionInput"
          rows="3"
          placeholder="質問を入力..."
        ></textarea>
        <div class="composer-actions">
          <button id="cancelButton" class="icon-button" type="button" disabled title="停止" aria-label="停止">
            <svg class="action-icon stop-icon" aria-hidden="true" viewBox="0 0 24 24">
              <rect x="7" y="7" width="10" height="10" rx="1.5"></rect>
            </svg>
          </button>
          <button id="sendButton" class="send-button" type="submit" title="送信" aria-label="送信">
            <svg class="action-icon" aria-hidden="true" viewBox="0 0 24 24">
              <path d="M5 12h13"></path>
              <path d="M12 5l7 7-7 7"></path>
            </svg>
          </button>
        </div>
      </form>
    </main>

    <aside class="history-sidebar">
      <button id="newChatButton" class="new-chat-button" type="button">
        <span aria-hidden="true">＋</span>
        新しいチャット
      </button>
      <div id="historyList" class="history-list"></div>
    </aside>

    <div id="settingsOverlay" class="settings-overlay" hidden>
      <form id="settingsForm" class="settings-dialog" aria-labelledby="settingsTitle">
        <div class="settings-dialog-head">
          <h2 id="settingsTitle">設定</h2>
          <button id="settingsCloseButton" class="modal-close-button" type="button" title="閉じる" aria-label="閉じる">×</button>
        </div>
        <label class="settings-field">
          <span>DeepSeek APIキー</span>
          <input id="apiKeyInput" type="password" autocomplete="off" spellcheck="false" placeholder="sk-..." />
        </label>
        <p id="apiKeyStatus" class="settings-status"></p>
        <div class="settings-dialog-actions">
          <button id="clearApiKeyButton" class="text-button danger-text" type="button">削除</button>
          <button id="saveSettingsButton" class="primary" type="submit">保存</button>
        </div>
      </form>
    </div>
  </div>
`;

const $ = (selector) => document.querySelector(selector);

const els = {
  sidebar: $(".sidebar"),
  chooseSourceButton: $("#chooseSourceButton"),
  sourcePath: $("#sourcePath"),
  settingsButton: $("#settingsButton"),
  indexButton: $("#indexButton"),
  selectAllButton: $("#selectAllButton"),
  clearSelectionButton: $("#clearSelectionButton"),
  fileList: $("#fileList"),
  stats: $("#stats"),
  messages: $("#messages"),
  jumpToLatestButton: $("#jumpToLatestButton"),
  cancelButton: $("#cancelButton"),
  questionForm: $("#questionForm"),
  questionInput: $("#questionInput"),
  newChatButton: $("#newChatButton"),
  sendButton: $("#sendButton"),
  toastRegion: $("#toastRegion"),
  historySidebar: $(".history-sidebar"),
  historyList: $("#historyList"),
  settingsOverlay: $("#settingsOverlay"),
  settingsForm: $("#settingsForm"),
  settingsCloseButton: $("#settingsCloseButton"),
  apiKeyInput: $("#apiKeyInput"),
  apiKeyStatus: $("#apiKeyStatus"),
  clearApiKeyButton: $("#clearApiKeyButton"),
  saveSettingsButton: $("#saveSettingsButton"),
};

init();

async function init() {
  loadChatStore();
  renderCurrentMessages();
  renderChatSidebar();
  wireEvents();
  await listen("task-progress", (event) => {
    renderProgress(event.payload);
  });
  await listen("answer-delta", (event) => {
    const payload = event.payload;
    if (!state.currentAssistantBody || payload.runId !== state.currentRunId) return;
    state.currentAssistantHasAnswer = true;
    enqueueAssistantDelta(payload.delta || "");
  });
  await refreshSnapshot();
}

function wireEvents() {
  els.settingsButton.addEventListener("click", openSettingsDialog);
  els.settingsCloseButton.addEventListener("click", closeSettingsDialog);
  els.settingsOverlay.addEventListener("click", (event) => {
    if (event.target === els.settingsOverlay) closeSettingsDialog();
  });
  els.settingsForm.addEventListener("submit", saveApiKey);
  els.clearApiKeyButton.addEventListener("click", clearApiKey);
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && !els.settingsOverlay.hidden) closeSettingsDialog();
  });
  els.chooseSourceButton.addEventListener("click", chooseSourceDirectory);
  els.indexButton.addEventListener("click", indexSource);
  els.selectAllButton.addEventListener("click", async () => {
    if (state.busy) return;
    await invoke("select_all_documents");
    await refreshSnapshot();
  });
  els.clearSelectionButton.addEventListener("click", async () => {
    if (state.busy) return;
    await invoke("clear_document_selection");
    await refreshSnapshot();
  });
  els.cancelButton.addEventListener("click", async () => {
    await invoke("cancel_current_task");
    renderAssistantStatus("停止しています");
  });
  els.newChatButton.addEventListener("click", startNewChat);
  document.addEventListener("click", (event) => {
    if (!state.openHistoryMenuId) return;
    if (event.target.closest("[data-archived-menu-id]") || event.target.closest(".history-menu")) return;
    state.openHistoryMenuId = null;
    renderChatSidebar();
  });
  els.messages.addEventListener("scroll", () => {
    state.autoScroll = isNearBottom();
    renderJumpButton();
  });
  els.jumpToLatestButton.addEventListener("click", () => {
    state.autoScroll = true;
    scrollMessages({ force: true });
  });
  els.questionForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    await answerQuestion();
  });
  els.historyList.addEventListener("click", (event) => {
    const archiveButton = event.target.closest("[data-archive-chat-id]");
    if (archiveButton) {
      event.preventDefault();
      event.stopPropagation();
      archiveChat(archiveButton.dataset.archiveChatId);
      return;
    }

    const menuButton = event.target.closest("[data-archived-menu-id]");
    if (menuButton) {
      event.preventDefault();
      event.stopPropagation();
      toggleArchivedChatMenu(menuButton.dataset.archivedMenuId);
      return;
    }

    const restoreButton = event.target.closest("[data-restore-archived-chat-id]");
    if (restoreButton) {
      event.preventDefault();
      event.stopPropagation();
      restoreArchivedChat(restoreButton.dataset.restoreArchivedChatId);
      return;
    }

    const deleteButton = event.target.closest("[data-delete-archived-chat-id]");
    if (deleteButton) {
      event.preventDefault();
      event.stopPropagation();
      deleteArchivedChat(deleteButton.dataset.deleteArchivedChatId);
      return;
    }

    const button = event.target.closest("[data-chat-id]");
    if (!button) return;
    loadChatSession(button.dataset.chatId, button.dataset.chatStatus);
  });
}

async function refreshSnapshot() {
  state.snapshot = await invoke("get_app_snapshot");
  renderSnapshot();
}

function renderSnapshot() {
  const { settings, documents, stats } = state.snapshot;
  els.sourcePath.textContent = settings.sourcePath || "未設定";
  els.sourcePath.title = settings.sourcePath || "";
  renderStats(stats);
  renderFileList(documents);
  renderApiKeyStatus();
}

function renderStats(stats) {
  els.stats.innerHTML = `
    <span>選択 ${formatNumber(stats.selectedDocumentCount)} / ${formatNumber(stats.documentCount)} ・ ${formatNumber(stats.selectedChars)}字</span>
  `;
}

function renderFileList(documents) {
  if (!documents.length) {
    els.fileList.innerHTML = `<div class="empty-box">まだ資料がありません</div>`;
    return;
  }

  els.fileList.innerHTML = documents
    .map((doc) => {
      const statusClass = doc.status === "indexed" ? "ok" : doc.status === "error" ? "bad" : "warn";
      const statusText = doc.status === "indexed" ? "読込済み" : doc.status;
      return `
        <label class="file-row">
          <input type="checkbox" data-doc-id="${doc.id}" ${doc.selected ? "checked" : ""} ${state.busy ? "disabled" : ""} />
          <span>
            <strong>${escapeHtml(doc.fileName)}</strong>
            <small>${formatNumber(doc.charCount)}字</small>
            ${doc.error ? `<em>${escapeHtml(doc.error)}</em>` : ""}
          </span>
          <b class="${statusClass}">${escapeHtml(statusText)}</b>
        </label>
      `;
    })
    .join("");

  els.fileList.querySelectorAll("input[type='checkbox']").forEach((input) => {
    input.addEventListener("change", async () => {
      if (state.busy) return;
      await invoke("set_document_selected", {
        id: Number(input.dataset.docId),
        selected: input.checked,
      });
      await refreshSnapshot();
    });
  });
}

async function chooseSourceDirectory() {
  if (state.busy) return;
  const selected = await open({
    directory: true,
    multiple: false,
    title: "sourceディレクトリを選択",
  });
  if (!selected) return;
  state.snapshot = await invoke("set_source_directory", { path: selected });
  renderSnapshot();
}

async function indexSource() {
  if (state.busy) return;
  setBusy(true);
  state.currentAssistantBody = null;
  state.currentAssistantRaw = "";
  state.currentAssistantHasAnswer = false;

  try {
    showToast("資料を更新しています", "ファイルの変更を確認しています。", "running", { persist: true });
    const summary = await invoke("index_source_directory");
    await refreshSnapshot();
    clearPersistentToasts();
    showToast(
      "資料の更新が完了しました",
      `読み込み ${summary.indexedFiles}件 / 変更なし ${summary.skippedFiles}件 / 失敗 ${summary.failedFiles}件`,
      summary.failedFiles ? "warn" : "success",
    );
  } catch (error) {
    clearPersistentToasts();
    showToast("資料の更新に失敗しました", String(error), "error");
  } finally {
    state.currentAssistantBody = null;
    state.currentRunId = null;
    setBusy(false);
  }
}

async function answerQuestion() {
  const question = els.questionInput.value.trim();
  if (!question || state.busy) return;
  if (!state.snapshot?.settings?.apiKeySaved) {
    openSettingsDialog();
    showToast("DeepSeek APIキーを設定してください", "", "info");
    return;
  }

  setBusy(true);
  promoteCurrentArchivedChat();
  addMessage("user", question);
  saveCurrentChat();
  els.questionInput.value = "";

  const answerStartedAt = performance.now();
  const assistant = addMessage("assistant", "", { markdown: true });
  state.currentAssistantArticle = assistant;
  state.currentAssistantBody = assistant.querySelector(".body");
  state.currentAssistantMessageId = assistant.dataset.messageId;
  state.currentRunId = null;
  state.currentAssistantRaw = "";
  state.currentAssistantQueued = "";
  state.currentAssistantHasAnswer = false;
  state.lastProgressStage = "profile";

  try {
    renderAssistantStatus("調べています");
    const response = await invoke("answer_question", {
      request: {
        question,
        mode: "comprehensive",
        selectedDocumentIds: selectedDocumentIds(),
      },
    });
    state.currentRunId = response.runId;
    syncFinalAnswer(response.answer || "");
    await waitForTypewriterIdle();
    const shouldStick = shouldAutoScroll();
    updateMessage(assistant.dataset.messageId, {
      text: state.currentAssistantRaw,
      elapsedMs: performance.now() - answerStartedAt,
      failed: false,
    });
    renderAnswerFooter(assistant, state.currentAssistantRaw, performance.now() - answerStartedAt);
    saveCurrentChat();
    scrollMessages({ force: shouldStick });
  } catch (error) {
    await waitForTypewriterIdle();
    const message = state.currentAssistantRaw
      ? `${state.currentAssistantRaw}\n\n---\n\n回答に失敗しました。処理は停止しました。\n\n理由: ${String(error)}`
      : `回答に失敗しました。処理は停止しました。\n\n理由: ${String(error)}`;
    const shouldStick = shouldAutoScroll();
    renderMarkdown(state.currentAssistantBody, message);
    updateMessage(assistant.dataset.messageId, {
      text: message,
      elapsedMs: performance.now() - answerStartedAt,
      failed: true,
    });
    renderAnswerFooter(assistant, message, performance.now() - answerStartedAt, { failed: true });
    saveCurrentChat();
    scrollMessages({ force: shouldStick });
  } finally {
    state.currentAssistantArticle = null;
    state.currentAssistantBody = null;
    state.currentRunId = null;
    state.currentAssistantRaw = "";
    state.currentAssistantQueued = "";
    state.currentAssistantHasAnswer = false;
    state.currentAssistantMessageId = null;
    setBusy(false);
  }
}

function selectedDocumentIds() {
  return (state.snapshot?.documents || []).filter((doc) => doc.selected).map((doc) => doc.id);
}

function openSettingsDialog() {
  if (state.busy) return;
  renderApiKeyStatus();
  els.apiKeyInput.value = "";
  els.settingsOverlay.hidden = false;
  window.setTimeout(() => els.apiKeyInput.focus(), 0);
}

function closeSettingsDialog() {
  els.settingsOverlay.hidden = true;
  els.apiKeyInput.value = "";
}

async function saveApiKey(event) {
  event.preventDefault();
  const apiKey = els.apiKeyInput.value.trim();
  if (!apiKey) {
    showToast("APIキーを入力してください", "", "info");
    return;
  }

  setSettingsSaving(true);
  try {
    const settings = await invoke("save_settings", {
      update: buildSettingsUpdate({ apiKey }),
    });
    state.snapshot = {
      ...state.snapshot,
      settings,
    };
    renderApiKeyStatus();
    closeSettingsDialog();
    showToast("APIキーを保存しました", "", "success");
  } catch (error) {
    showToast("APIキーの保存に失敗しました", String(error), "error");
  } finally {
    setSettingsSaving(false);
  }
}

async function clearApiKey() {
  if (state.busy || !state.snapshot?.settings?.apiKeySaved) return;

  setSettingsSaving(true);
  try {
    const settings = await invoke("save_settings", {
      update: buildSettingsUpdate({ clearApiKey: true }),
    });
    state.snapshot = {
      ...state.snapshot,
      settings,
    };
    els.apiKeyInput.value = "";
    renderApiKeyStatus();
    showToast("APIキーを削除しました", "", "success");
  } catch (error) {
    showToast("APIキーの削除に失敗しました", String(error), "error");
  } finally {
    setSettingsSaving(false);
  }
}

function buildSettingsUpdate(options = {}) {
  const settings = state.snapshot?.settings || {};
  return {
    model: settings.model || "deepseek-v4-flash",
    thinkingEnabled: false,
    reasoningEffort: settings.reasoningEffort || "high",
    temperature: Number(settings.temperature ?? 0.1),
    maxContextChars: Number(settings.maxContextChars ?? 60_000),
    comprehensiveBatchChars: Number(settings.comprehensiveBatchChars ?? 8_000),
    apiKey: options.apiKey || null,
    clearApiKey: Boolean(options.clearApiKey),
  };
}

function renderApiKeyStatus() {
  if (!els.apiKeyStatus) return;
  const settings = state.snapshot?.settings;
  const saved = Boolean(settings?.apiKeySaved);
  const storage = settings?.apiKeyStorage || "none";
  if (!saved) {
    els.apiKeyStatus.textContent = "APIキーは未設定です。";
  } else if (storage === "os-keychain") {
    els.apiKeyStatus.textContent = "APIキーは保存済みです。";
  } else {
    els.apiKeyStatus.textContent = "APIキーは保存済みです。";
  }
  els.clearApiKeyButton.disabled = state.busy || !saved;
}

function setSettingsSaving(saving) {
  els.apiKeyInput.disabled = saving;
  els.clearApiKeyButton.disabled = saving || !state.snapshot?.settings?.apiKeySaved;
  els.saveSettingsButton.disabled = saving;
  els.settingsCloseButton.disabled = saving;
}

function startNewChat() {
  if (state.busy) return;
  saveCurrentChat();
  selectDraftChat();
  state.openHistoryMenuId = null;
  renderCurrentMessages();
  renderChatSidebar();
}

function archiveChat(id) {
  if (state.busy) return;
  state.openHistoryMenuId = null;
  if (state.currentChatStatus === "active" && state.currentChatId === id) {
    saveCurrentChat();
  }

  const chat = state.activeChats.find((item) => item.id === id);
  if (!chat) return;

  state.activeChats = state.activeChats.filter((item) => item.id !== id);

  if (hasConversationMessages(chat)) {
    const archived = {
      ...chat,
      title: buildChatTitle(chat.messages),
      updatedAt: Date.now(),
    };
    state.archivedChats = [archived, ...state.archivedChats.filter((item) => item.id !== id)].slice(0, 80);
    showToast("アーカイブしました", "", "success");
  } else {
    showToast("空のチャットを削除しました", "", "info");
  }

  if (state.currentChatStatus === "active" && state.currentChatId === id) {
    const next = state.activeChats[0];
    if (next) {
      selectChat(next, "active");
    } else {
      selectDraftChat();
    }
    renderCurrentMessages();
  }

  persistChatStore();
  renderChatSidebar();
}

function toggleArchivedChatMenu(id) {
  if (state.busy) return;
  state.openHistoryMenuId = state.openHistoryMenuId === id ? null : id;
  renderChatSidebar();
}

function restoreArchivedChat(id) {
  if (state.busy) return;
  const chat = state.archivedChats.find((item) => item.id === id);
  if (!chat) return;

  const active = {
    ...chat,
    updatedAt: Date.now(),
  };
  state.openHistoryMenuId = null;
  state.archivedChats = state.archivedChats.filter((item) => item.id !== id);
  state.activeChats = [active, ...state.activeChats.filter((item) => item.id !== id)].slice(0, 80);
  selectChat(active, "active");
  persistChatStore();
  renderCurrentMessages();
  renderChatSidebar();
  showToast("アクティブに戻しました", "", "success");
}

function deleteArchivedChat(id) {
  if (state.busy) return;
  const chat = state.archivedChats.find((item) => item.id === id);
  if (!chat) return;

  state.openHistoryMenuId = null;
  state.archivedChats = state.archivedChats.filter((item) => item.id !== id);
  if (state.currentChatStatus === "archived" && state.currentChatId === id) {
    const next = state.activeChats[0];
    if (next) {
      selectChat(next, "active");
    } else {
      selectDraftChat();
    }
    renderCurrentMessages();
  }

  persistChatStore();
  renderChatSidebar();
  showToast("削除しました", "", "success");
}

function saveCurrentChat() {
  const messages = state.currentMessages
    .filter((message) => message.text && message.text.trim())
    .map((message) => ({ ...message }));
  if (!messages.length) return null;

  let chat = findCurrentChat();
  if (!chat) {
    chat = createChatSession([]);
    state.currentChatId = chat.id;
    state.currentChatStatus = "active";
    state.activeChats = [chat, ...state.activeChats.filter((item) => item.id !== chat.id)].slice(0, 80);
  }

  const now = Date.now();
  const next = {
    ...chat,
    title: buildChatTitle(messages),
    updatedAt: messages.length ? now : chat.updatedAt,
    messages,
  };

  replaceChat(next, state.currentChatStatus);
  persistChatStore();
  renderChatSidebar();
  return next;
}

function loadChatSession(id, status) {
  if (state.busy) return;
  saveCurrentChat();
  const chat = findChat(id, status);
  if (!chat) return;
  state.openHistoryMenuId = null;
  selectChat(chat, status);
  renderCurrentMessages();
  renderChatSidebar();
}

function promoteCurrentArchivedChat() {
  if (state.currentChatStatus !== "archived") return;
  const chat = state.archivedChats.find((item) => item.id === state.currentChatId);
  if (!chat) {
    ensureCurrentChat();
    return;
  }

  const active = {
    ...chat,
    messages: state.currentMessages.map((message) => ({ ...message })),
    updatedAt: Date.now(),
  };
  state.archivedChats = state.archivedChats.filter((item) => item.id !== chat.id);
  state.activeChats = [active, ...state.activeChats.filter((item) => item.id !== chat.id)].slice(0, 80);
  state.currentChatStatus = "active";
  state.currentChatId = active.id;
  state.openHistoryMenuId = null;
  persistChatStore();
  renderChatSidebar();
}

function ensureCurrentChat() {
  const existing = findCurrentChat();
  if (existing) {
    if (!state.currentMessages.length && existing.messages.length) {
      state.currentMessages = existing.messages.map((message) => ({ ...message }));
    }
    return existing;
  }

  if (!state.currentMessages.some((message) => message.text && message.text.trim())) {
    selectDraftChat();
    return null;
  }

  const chat = createChatSession(state.currentMessages);
  state.activeChats = [chat, ...state.activeChats.filter((item) => item.id !== chat.id)].slice(0, 80);
  state.currentChatId = chat.id;
  state.currentChatStatus = "active";
  return chat;
}

function selectChat(chat, status) {
  resetTypewriter();
  state.currentChatId = chat.id;
  state.currentChatStatus = status === "archived" ? "archived" : "active";
  state.currentMessages = chat.messages.map((message) => ({ ...message }));
  state.autoScroll = true;
}

function selectDraftChat() {
  resetTypewriter();
  state.currentChatId = null;
  state.currentChatStatus = "active";
  state.currentMessages = [];
  state.autoScroll = true;
}

function findCurrentChat() {
  return findChat(state.currentChatId, state.currentChatStatus);
}

function findChat(id, status) {
  if (!id) return null;
  const list = status === "archived" ? state.archivedChats : state.activeChats;
  return list.find((chat) => chat.id === id) || null;
}

function replaceChat(chat, status) {
  const key = status === "archived" ? "archivedChats" : "activeChats";
  state[key] = state[key].map((item) => (item.id === chat.id ? chat : item));
}

function renderChatSidebar() {
  if (!els.historyList) return;
  els.historyList.innerHTML = `
    ${renderChatSection("アクティブなチャット", state.activeChats, "active")}
    ${renderChatSection("アーカイブ済み", state.archivedChats, "archived")}
  `;
  updateHistoryDisabledState();
}

function renderChatSection(title, chats, status) {
  const visibleChats = chats.filter(hasConversationMessages);
  const emptyText = status === "active" ? "アクティブなチャットはありません" : "アーカイブ済みのチャットはありません";
  return `
    <section class="history-section">
      <h3>${escapeHtml(title)}</h3>
      ${
        visibleChats.length
          ? visibleChats.map((chat) => renderChatItem(chat, status)).join("")
          : `<div class="history-empty">${escapeHtml(emptyText)}</div>`
      }
    </section>
  `;
}

function renderChatItem(chat, status) {
  const active = chat.id === state.currentChatId && status === state.currentChatStatus ? " active" : "";
  const menuOpen = status === "archived" && state.openHistoryMenuId === chat.id;
  const archiveAction =
    status === "active" && hasConversationMessages(chat)
      ? `
        <button class="history-archive-button" type="button" data-archive-chat-id="${escapeHtml(chat.id)}" title="アーカイブ" aria-label="アーカイブ">
          <svg class="action-icon" aria-hidden="true" viewBox="0 0 24 24">
            <rect x="3" y="4" width="18" height="5" rx="1.5"></rect>
            <path d="M5 9v9a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V9"></path>
            <path d="M10 13h4"></path>
          </svg>
        </button>
      `
      : status === "archived"
        ? `
        <button class="history-menu-button" type="button" data-archived-menu-id="${escapeHtml(chat.id)}" title="メニュー" aria-label="メニュー" aria-haspopup="menu" aria-expanded="${menuOpen ? "true" : "false"}">
          <svg class="action-icon" aria-hidden="true" viewBox="0 0 24 24">
            <circle cx="12" cy="5" r="1.8"></circle>
            <circle cx="12" cy="12" r="1.8"></circle>
            <circle cx="12" cy="19" r="1.8"></circle>
          </svg>
        </button>
        ${
          menuOpen
            ? `
          <div class="history-menu" role="menu">
            <button type="button" role="menuitem" data-restore-archived-chat-id="${escapeHtml(chat.id)}">アクティブに戻す</button>
            <button class="danger" type="button" role="menuitem" data-delete-archived-chat-id="${escapeHtml(chat.id)}">削除</button>
          </div>
        `
            : ""
        }
      `
      : "";
  return `
    <div class="history-row${active}${menuOpen ? " has-menu-open" : ""}">
      <button class="history-item${active}" type="button" data-chat-id="${escapeHtml(chat.id)}" data-chat-status="${escapeHtml(status)}">
        <strong>${escapeHtml(chat.title || buildChatTitle(chat.messages))}</strong>
        <span>${escapeHtml(formatDateTime(chat.updatedAt || chat.createdAt))}</span>
      </button>
      ${archiveAction}
    </div>
  `;
}

function renderCurrentMessages() {
  resetTypewriter();
  els.messages.innerHTML = "";
  if (!state.currentMessages.length) {
    renderEmptyConversation();
    return;
  }
  for (const message of state.currentMessages) {
    const article = addMessage(message.role, message.text, {
      markdown: message.role === "assistant",
      persist: false,
      messageId: message.id,
    });
    if (message.role === "assistant" && message.text) {
      renderAnswerFooter(article, message.text, message.elapsedMs || 0, { failed: message.failed });
    }
  }
  scrollMessages({ force: true });
}

function renderEmptyConversation() {
  els.messages.innerHTML = "";
  scrollMessages({ force: true });
}

function loadChatStore() {
  try {
    const raw = localStorage.getItem(CHAT_STORAGE_KEY);
    const parsed = raw ? JSON.parse(raw) : null;
    if (parsed && typeof parsed === "object") {
      state.activeChats = normalizeChatList(parsed.activeChats);
      state.archivedChats = normalizeChatList(parsed.archivedChats);
    } else {
      state.activeChats = [];
      state.archivedChats = loadLegacyArchivedChats();
    }
  } catch {
    state.activeChats = [];
    state.archivedChats = loadLegacyArchivedChats();
  }

  state.currentChatStatus = "active";
  state.currentChatId = state.activeChats[0]?.id || null;
  if (state.currentChatId) {
    state.currentMessages = state.activeChats[0].messages.map((message) => ({ ...message }));
  }
}

function loadLegacyArchivedChats() {
  try {
    const raw = localStorage.getItem(LEGACY_ARCHIVE_STORAGE_KEY);
    const parsed = raw ? JSON.parse(raw) : [];
    return normalizeChatList(parsed);
  } catch {
    return [];
  }
}

function persistChatStore() {
  try {
    const payload = {
      activeChats: state.activeChats.filter(hasConversationMessages).slice(0, 80),
      archivedChats: state.archivedChats.filter(hasConversationMessages).slice(0, 80),
    };
    localStorage.setItem(CHAT_STORAGE_KEY, JSON.stringify(payload));
  } catch (error) {
    showToast("履歴の保存に失敗しました", String(error), "error");
  }
}

function normalizeChatList(value) {
  if (!Array.isArray(value)) return [];
  const seen = new Set();
  return value
    .map(normalizeChatSession)
    .filter((chat) => {
      if (!chat || seen.has(chat.id)) return false;
      seen.add(chat.id);
      return true;
    })
    .slice(0, 80);
}

function normalizeChatSession(value) {
  if (!value || typeof value !== "object") return null;
  const now = Date.now();
  const messages = Array.isArray(value.messages) ? value.messages.map(normalizeMessage).filter(Boolean) : [];
  const chat = {
    id: String(value.id || createId()),
    title: typeof value.title === "string" && value.title.trim() ? value.title.trim() : buildChatTitle(messages),
    createdAt: Number(value.createdAt) || now,
    updatedAt: Number(value.updatedAt) || Number(value.createdAt) || now,
    messages,
  };
  return chat;
}

function normalizeMessage(value) {
  if (!value || typeof value !== "object") return null;
  const text = String(value.text || "");
  if (!text.trim()) return null;
  return {
    id: String(value.id || createId()),
    role: value.role === "assistant" ? "assistant" : "user",
    text,
    createdAt: Number(value.createdAt) || Date.now(),
    elapsedMs: Number.isFinite(Number(value.elapsedMs)) ? Number(value.elapsedMs) : null,
    failed: Boolean(value.failed),
  };
}

function createChatSession(messages) {
  const now = Date.now();
  return {
    id: createId(),
    title: buildChatTitle(messages),
    createdAt: now,
    updatedAt: now,
    messages: messages.map((message) => ({ ...message })),
  };
}

function hasConversationMessages(chat) {
  return Boolean(chat?.messages?.some((message) => message.text && message.text.trim()));
}

function buildChatTitle(messages) {
  const firstUser = messages.find((message) => message.role === "user" && message.text.trim());
  const title = firstUser?.text.trim().replace(/\s+/g, " ") || "新しいチャット";
  return title.length > 34 ? `${title.slice(0, 34)}...` : title;
}

function renderProgress(payload) {
  if (payload.runId) state.currentRunId = payload.runId;
  if (!state.currentAssistantBody || state.currentAssistantHasAnswer) return;

  if (payload.stage && payload.stage !== "api") {
    state.lastProgressStage = payload.stage;
  }
  const title = progressTitle(payload);
  renderAssistantStatus(title, payload);
}

function progressTitle(payload) {
  const { stage, status, message = "", completed = 0, total = 0 } = payload;
  if (status === "cancelled") return "停止しました";
  if (status === "stopped") return "停止しました";
  if (status === "complete") return "仕上げています";

  if (stage === "profile") return "質問の意図を整理しています";
  if (stage === "retrieve") return "関連しそうな資料を探しています";
  if (stage === "extract") {
    if (message.includes("追加検索") || message.includes("再確認")) return "見落としがないか再確認しています";
    if (total > 0) {
      const ratio = completed / total;
      if (ratio < 0.25) return "資料を読み始めています";
      if (ratio < 0.7) return "根拠を拾い集めています";
      return "条件や例外を照合しています";
    }
    return "根拠を抽出しています";
  }
  if (stage === "synthesize") return "根拠を統合しています";
  if (stage === "audit") return "回答のズレを点検しています";
  if (stage === "answer") return "回答を整えています";
  if (stage === "api") {
    if (message.includes("制限")) return "API制限の回復を待っています";
    if (message.includes("停止")) return "API応答で停止しました";
    return apiStageTitle(state.lastProgressStage);
  }
  if (stage === "index") return "資料を読み込んでいます";
  return "処理しています";
}

function apiStageTitle(stage) {
  const labels = {
    profile: "質問の意図を整理しています",
    retrieve: "資料検索を準備しています",
    extract: "根拠抽出を進めています",
    synthesize: "回答を組み立てています",
    audit: "回答のズレを点検しています",
    answer: "回答を整えています",
  };
  return labels[stage] || "処理しています";
}

function renderAssistantStatus(title, payload = {}) {
  if (!state.currentAssistantBody) return;
  const shouldStick = shouldAutoScroll();
  const percent = payload.total ? Math.min(100, Math.round((payload.completed / payload.total) * 100)) : 0;
  const stageClass = payload.stage ? `stage-${payload.stage}` : "stage-idle";
  state.currentAssistantBody.classList.add("status-body");
  state.currentAssistantBody.innerHTML = `
    <div class="thinking ${stageClass}">
      <span class="thinking-dot"></span>
      <div>
        <strong>${escapeHtml(title)}</strong>
        ${
          payload.total
            ? `<div class="progress-track" aria-hidden="true"><span style="width: ${percent}%"></span></div>`
            : ""
        }
      </div>
    </div>
  `;
  scrollMessages({ force: shouldStick });
}

function showToast(title, detail = "", tone = "info", options = {}) {
  const id = `toast-${++state.toastSeq}`;
  const toast = document.createElement("div");
  toast.className = `toast ${tone}`;
  toast.dataset.toastId = id;
  if (options.persist) toast.dataset.persist = "true";
  toast.innerHTML = `
    <div class="toast-copy">
      <strong>${escapeHtml(title)}</strong>
    </div>
    <button class="toast-close" type="button" aria-label="閉じる" title="閉じる">×</button>
  `;
  toast.querySelector(".toast-close").addEventListener("click", () => dismissToast(toast));
  els.toastRegion.append(toast);
  requestAnimationFrame(() => toast.classList.add("visible"));
  if (!options.persist) {
    window.setTimeout(() => dismissToast(toast), options.duration ?? 2000);
  }
  return id;
}

function dismissToast(toast) {
  if (!toast || !toast.isConnected) return;
  toast.classList.remove("visible");
  window.setTimeout(() => toast.remove(), 180);
}

function clearPersistentToasts() {
  els.toastRegion.querySelectorAll("[data-persist='true']").forEach(dismissToast);
}

function addMessage(role, text, options = {}) {
  const article = document.createElement("article");
  article.className = `message ${role}`;
  const messageId = options.messageId || createId();
  article.dataset.messageId = messageId;
  article.innerHTML = `<div class="body ${options.markdown ? "markdown-body" : ""}"></div>`;
  const body = article.querySelector(".body");
  if (options.markdown) {
    renderMarkdown(body, text);
  } else {
    body.textContent = text;
  }
  if (options.persist !== false) {
    state.currentMessages.push({
      id: messageId,
      role,
      text,
      createdAt: Date.now(),
      elapsedMs: null,
      failed: false,
    });
  }
  els.messages.append(article);
  scrollMessages({ force: role === "user" || shouldAutoScroll() });
  return article;
}

function updateMessage(id, patch) {
  const message = state.currentMessages.find((item) => item.id === id);
  if (message) Object.assign(message, patch);
}

function renderMarkdown(element, source) {
  element.classList.remove("status-body");
  element.classList.add("markdown-body");
  element.innerHTML = DOMPurify.sanitize(marked.parse(source || ""));
}

function renderAnswerFooter(article, text, elapsedMs, options = {}) {
  article.querySelector(".message-footer")?.remove();
  const footer = document.createElement("div");
  footer.className = "message-footer";
  footer.innerHTML = `
    <span>${escapeHtml(options.failed ? `停止まで ${formatElapsed(elapsedMs)}` : `回答時間 ${formatElapsed(elapsedMs)}`)}</span>
    <button class="copy-answer-button" type="button" title="コピー" aria-label="回答をコピー">
      <svg class="action-icon" aria-hidden="true" viewBox="0 0 24 24">
        <rect x="9" y="9" width="10" height="10" rx="2"></rect>
        <path d="M5 15H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v1"></path>
      </svg>
    </button>
  `;
  footer.querySelector(".copy-answer-button").addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(text || "");
      showToast("コピーしました", "回答をクリップボードへコピーしました。", "success");
    } catch (error) {
      showToast("コピーに失敗しました", String(error), "error");
    }
  });
  article.append(footer);
}

function enqueueAssistantDelta(delta) {
  if (!delta) return;
  state.currentAssistantQueued += delta;
  startTypewriter();
}

function startTypewriter() {
  if (state.typewriterTimer || !state.currentAssistantBody) return;
  state.typewriterTimer = window.setInterval(() => {
    if (!state.currentAssistantBody) {
      resetTypewriter();
      return;
    }
    if (!state.currentAssistantQueued) {
      stopTypewriter();
      resolveTypewriterWaiters();
      return;
    }

    const shouldStick = shouldAutoScroll();
    const next = state.currentAssistantQueued.slice(0, TYPEWRITER_CHARS_PER_TICK);
    state.currentAssistantQueued = state.currentAssistantQueued.slice(next.length);
    state.currentAssistantRaw += next;
    renderMarkdown(state.currentAssistantBody, state.currentAssistantRaw);
    scrollMessages({ force: shouldStick });

    if (!state.currentAssistantQueued) {
      stopTypewriter();
      resolveTypewriterWaiters();
    }
  }, TYPEWRITER_INTERVAL_MS);
}

function syncFinalAnswer(answer) {
  if (!answer) return;
  const known = state.currentAssistantRaw + state.currentAssistantQueued;
  if (answer === known) return;
  if (answer.startsWith(state.currentAssistantRaw)) {
    state.currentAssistantQueued = answer.slice(state.currentAssistantRaw.length);
  } else {
    state.currentAssistantRaw = "";
    state.currentAssistantQueued = answer;
    if (state.currentAssistantBody) renderMarkdown(state.currentAssistantBody, "");
  }
  startTypewriter();
}

function waitForTypewriterIdle() {
  if (!state.currentAssistantQueued && !state.typewriterTimer) return Promise.resolve();
  return new Promise((resolve) => {
    state.typewriterResolvers.push(resolve);
    startTypewriter();
  });
}

function stopTypewriter() {
  if (state.typewriterTimer) {
    window.clearInterval(state.typewriterTimer);
    state.typewriterTimer = null;
  }
}

function resetTypewriter() {
  stopTypewriter();
  state.currentAssistantQueued = "";
  resolveTypewriterWaiters();
}

function resolveTypewriterWaiters() {
  const resolvers = state.typewriterResolvers.splice(0);
  resolvers.forEach((resolve) => resolve());
}

function setBusy(busy) {
  state.busy = busy;
  if (busy) state.openHistoryMenuId = null;
  els.sidebar.classList.toggle("is-locked", busy);
  els.historySidebar.classList.toggle("is-locked", busy);
  els.settingsButton.disabled = busy;
  els.chooseSourceButton.disabled = busy;
  els.indexButton.disabled = busy;
  els.selectAllButton.disabled = busy;
  els.clearSelectionButton.disabled = busy;
  els.newChatButton.disabled = busy;
  els.sendButton.disabled = busy;
  els.cancelButton.disabled = !busy;
  els.questionInput.disabled = busy;
  els.fileList.querySelectorAll("input[type='checkbox']").forEach((input) => {
    input.disabled = busy;
  });
  updateHistoryDisabledState();
}

function updateHistoryDisabledState() {
  if (!els.historyList) return;
  els.historyList.querySelectorAll("button").forEach((button) => {
    button.disabled = state.busy;
  });
}

function shouldAutoScroll() {
  return state.autoScroll || isNearBottom();
}

function isNearBottom() {
  if (!canScrollMessages()) return true;
  return distanceFromBottom() <= 4;
}

function canScrollMessages() {
  return els.messages.scrollHeight > els.messages.clientHeight + 2;
}

function distanceFromBottom() {
  return Math.max(0, els.messages.scrollHeight - els.messages.scrollTop - els.messages.clientHeight);
}

function scrollMessages(options = {}) {
  if (options.force) {
    els.messages.scrollTop = els.messages.scrollHeight;
    state.autoScroll = true;
  }
  renderJumpButton();
}

function renderJumpButton() {
  const show = canScrollMessages() && distanceFromBottom() > 4;
  els.jumpToLatestButton.hidden = !show;
}

function formatNumber(value) {
  return new Intl.NumberFormat("ja-JP").format(value || 0);
}

function formatElapsed(ms) {
  const seconds = Math.max(0, Math.round((ms || 0) / 1000));
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  return minutes ? `${minutes}分${rest}秒` : `${rest}秒`;
}

function formatDateTime(timestamp) {
  return new Intl.DateTimeFormat("ja-JP", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(timestamp || Date.now()));
}

function createId() {
  if (crypto.randomUUID) return crypto.randomUUID();
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}
