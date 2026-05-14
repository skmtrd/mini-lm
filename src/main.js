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
};

document.querySelector("#app").innerHTML = `
  <div class="shell">
    <div id="toastRegion" class="toast-region" aria-live="polite" aria-atomic="false"></div>
    <aside class="sidebar source-sidebar">
      <section class="source-panel">
        <div class="source-head">
          <h2>資料</h2>
          <button id="chooseSourceButton" type="button">フォルダ選択</button>
        </div>
        <div id="sourcePath" class="path-box">未設定</div>
        <div id="stats" class="stats compact"></div>
        <div class="source-actions">
          <button id="indexButton" class="primary" type="button">更新</button>
          <button id="selectAllButton" type="button">全選択</button>
          <button id="clearSelectionButton" type="button">解除</button>
        </div>
        <div id="fileList" class="file-list"></div>
      </section>
    </aside>

    <main class="main">
      <div class="message-area">
        <section id="messages" class="messages">
          <article class="message assistant">
            <div class="body markdown-body">
              <p>資料を選択してインデックスを作成すると、選択中の資料だけを根拠に回答します。</p>
            </div>
          </article>
        </section>
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
      <div class="history-head">
        <h2>履歴</h2>
      </div>
      <button id="newChatButton" class="new-chat-button" type="button">
        <span aria-hidden="true">＋</span>
        新しいチャット
      </button>
      <div id="historyList" class="history-list"></div>
    </aside>
  </div>
`;

const $ = (selector) => document.querySelector(selector);

const els = {
  sidebar: $(".sidebar"),
  chooseSourceButton: $("#chooseSourceButton"),
  sourcePath: $("#sourcePath"),
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
};

init();

async function init() {
  loadChatStore();
  ensureCurrentChat();
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
      archiveChat(archiveButton.dataset.archiveChatId);
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
  renderStats(stats);
  renderFileList(documents);
}

function renderStats(stats) {
  els.stats.innerHTML = `
    <span>選択 ${formatNumber(stats.selectedDocumentCount)} / ${formatNumber(stats.documentCount)}</span>
    <span>${formatNumber(stats.selectedChars)}字</span>
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

  setBusy(true);
  ensureCurrentChat();
  promoteCurrentArchivedChat();
  addMessage("user", question);
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

function startNewChat() {
  if (state.busy) return;
  saveCurrentChat();

  if (state.currentChatStatus === "active" && !state.currentMessages.some((message) => message.text?.trim())) {
    renderCurrentMessages();
    renderChatSidebar();
    return;
  }

  const chat = createChatSession([]);
  state.activeChats = [chat, ...state.activeChats.filter((item) => item.id !== chat.id)].slice(0, 80);
  selectChat(chat, "active");
  persistChatStore();
  renderCurrentMessages();
  renderChatSidebar();
}

function archiveChat(id) {
  if (state.busy) return;
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
    const next = state.activeChats[0] || createChatSession([]);
    if (!state.activeChats.some((item) => item.id === next.id)) {
      state.activeChats.unshift(next);
    }
    selectChat(next, "active");
    renderCurrentMessages();
  }

  persistChatStore();
  renderChatSidebar();
}

function saveCurrentChat() {
  const chat = findCurrentChat();
  if (!chat) return null;

  const messages = state.currentMessages
    .filter((message) => message.text && message.text.trim())
    .map((message) => ({ ...message }));
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

  const chat = state.activeChats[0] || createChatSession([]);
  if (!state.activeChats.some((item) => item.id === chat.id)) {
    state.activeChats.unshift(chat);
  }
  selectChat(chat, "active");
  return chat;
}

function selectChat(chat, status) {
  resetTypewriter();
  state.currentChatId = chat.id;
  state.currentChatStatus = status === "archived" ? "archived" : "active";
  state.currentMessages = chat.messages.map((message) => ({ ...message }));
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
  const visibleChats = chats.filter((chat) => status === "active" || hasConversationMessages(chat));
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
      : "";
  return `
    <div class="history-row${active}">
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
  els.messages.innerHTML = `
    <article class="message assistant">
      <div class="body markdown-body">
        <p>資料を選択してインデックスを作成すると、選択中の資料だけを根拠に回答します。</p>
      </div>
    </article>
  `;
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
  els.sidebar.classList.toggle("is-locked", busy);
  els.historySidebar.classList.toggle("is-locked", busy);
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
