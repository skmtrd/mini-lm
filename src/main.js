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

const state = {
  snapshot: null,
  busy: false,
  currentRunId: null,
  currentAssistantBody: null,
  currentAssistantRaw: "",
  currentAssistantHasAnswer: false,
  toastSeq: 0,
  autoScroll: true,
};

document.querySelector("#app").innerHTML = `
  <div class="shell">
    <div id="toastRegion" class="toast-region" aria-live="polite" aria-atomic="false"></div>
    <aside class="sidebar">
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
      <section id="messages" class="messages">
        <article class="message assistant">
          <div class="body markdown-body">
            <p>資料を選択してインデックスを作成すると、選択中の資料だけを根拠に回答します。</p>
          </div>
        </article>
      </section>
      <button id="jumpToLatestButton" class="jump-latest" type="button" hidden>最新へ</button>

      <form id="questionForm" class="composer">
        <textarea
          id="questionInput"
          rows="3"
          placeholder="質問を入力..."
        ></textarea>
        <div class="composer-actions">
          <button id="clearMessagesButton" type="button">履歴クリア</button>
          <button id="cancelButton" class="icon-button" type="button" disabled title="停止" aria-label="停止">
            <svg class="action-icon" aria-hidden="true" viewBox="0 0 24 24">
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
  </div>
`;

const $ = (selector) => document.querySelector(selector);

const els = {
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
  clearMessagesButton: $("#clearMessagesButton"),
  sendButton: $("#sendButton"),
  toastRegion: $("#toastRegion"),
};

init();

async function init() {
  wireEvents();
  await listen("task-progress", (event) => {
    renderProgress(event.payload);
  });
  await listen("answer-delta", (event) => {
    const payload = event.payload;
    if (!state.currentAssistantBody || payload.runId !== state.currentRunId) return;
    const shouldStick = shouldAutoScroll();
    state.currentAssistantHasAnswer = true;
    state.currentAssistantRaw += payload.delta;
    renderMarkdown(state.currentAssistantBody, state.currentAssistantRaw);
    scrollMessages({ force: shouldStick });
  });
  await refreshSnapshot();
}

function wireEvents() {
  els.chooseSourceButton.addEventListener("click", chooseSourceDirectory);
  els.indexButton.addEventListener("click", indexSource);
  els.selectAllButton.addEventListener("click", async () => {
    await invoke("select_all_documents");
    await refreshSnapshot();
  });
  els.clearSelectionButton.addEventListener("click", async () => {
    await invoke("clear_document_selection");
    await refreshSnapshot();
  });
  els.cancelButton.addEventListener("click", async () => {
    await invoke("cancel_current_task");
    renderAssistantStatus("停止しています", "現在の処理へキャンセル要求を送りました。");
  });
  els.clearMessagesButton.addEventListener("click", () => {
    clearMessages();
    showToast("履歴をクリアしました", "チャット欄を空にしました。", "info");
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
    <span>${formatNumber(stats.chunkCount)}チャンク</span>
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
          <input type="checkbox" data-doc-id="${doc.id}" ${doc.selected ? "checked" : ""} />
          <span>
            <strong>${escapeHtml(doc.fileName)}</strong>
            <small>${formatNumber(doc.charCount)}字 / ${formatNumber(doc.chunkCount)}チャンク</small>
            ${doc.error ? `<em>${escapeHtml(doc.error)}</em>` : ""}
          </span>
          <b class="${statusClass}">${escapeHtml(statusText)}</b>
        </label>
      `;
    })
    .join("");

  els.fileList.querySelectorAll("input[type='checkbox']").forEach((input) => {
    input.addEventListener("change", async () => {
      await invoke("set_document_selected", {
        id: Number(input.dataset.docId),
        selected: input.checked,
      });
      await refreshSnapshot();
    });
  });
}

async function chooseSourceDirectory() {
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
  addMessage("user", question);
  els.questionInput.value = "";

  const assistant = addMessage("assistant", "", { markdown: true });
  state.currentAssistantBody = assistant.querySelector(".body");
  state.currentRunId = null;
  state.currentAssistantRaw = "";
  state.currentAssistantHasAnswer = false;

  try {
    renderAssistantStatus("調べています", "資料全体を確認し、回答に必要な根拠を抽出しています。");
    const response = await invoke("answer_question", {
      request: {
        question,
        mode: "comprehensive",
        selectedDocumentIds: selectedDocumentIds(),
      },
    });
    state.currentRunId = response.runId;
    state.currentAssistantRaw = response.answer || state.currentAssistantRaw;
    const shouldStick = shouldAutoScroll();
    renderMarkdown(state.currentAssistantBody, state.currentAssistantRaw);
    scrollMessages({ force: shouldStick });
  } catch (error) {
    const message = state.currentAssistantRaw
      ? `${state.currentAssistantRaw}\n\n---\n\n回答に失敗しました。処理は停止しました。\n\n理由: ${String(error)}`
      : `回答に失敗しました。処理は停止しました。\n\n理由: ${String(error)}`;
    const shouldStick = shouldAutoScroll();
    renderMarkdown(state.currentAssistantBody, message);
    scrollMessages({ force: shouldStick });
  } finally {
    state.currentAssistantBody = null;
    state.currentRunId = null;
    state.currentAssistantRaw = "";
    state.currentAssistantHasAnswer = false;
    setBusy(false);
  }
}

function selectedDocumentIds() {
  return (state.snapshot?.documents || []).filter((doc) => doc.selected).map((doc) => doc.id);
}

function clearMessages() {
  els.messages.innerHTML = `
    <article class="message assistant">
      <div class="body markdown-body">
        <p>資料を選択してインデックスを作成すると、選択中の資料だけを根拠に回答します。</p>
      </div>
    </article>
  `;
  scrollMessages({ force: true });
}

function renderProgress(payload) {
  if (payload.runId) state.currentRunId = payload.runId;
  if (!state.currentAssistantBody || state.currentAssistantHasAnswer) return;

  const total = payload.total ? `${formatNumber(payload.completed)}/${formatNumber(payload.total)}` : "";
  const elapsed = payload.elapsedMs ? `${Math.round(payload.elapsedMs / 1000)}秒` : "";
  const detail = [payload.message, total, elapsed].filter(Boolean).join(" ・ ");
  const title = progressTitle(payload);
  renderAssistantStatus(title, detail, payload);
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
    return "AIに処理を依頼しています";
  }
  if (stage === "index") return "資料を読み込んでいます";
  return "処理しています";
}

function renderAssistantStatus(title, detail, payload = {}) {
  if (!state.currentAssistantBody) return;
  const shouldStick = shouldAutoScroll();
  const percent = payload.total ? Math.min(100, Math.round((payload.completed / payload.total) * 100)) : 0;
  const stageClass = payload.stage ? `stage-${payload.stage}` : "stage-idle";
  state.currentAssistantBody.innerHTML = `
    <div class="thinking ${stageClass}">
      <span class="thinking-dot"></span>
      <div>
        <strong>${escapeHtml(title)}</strong>
        <p>${escapeHtml(detail || "")}</p>
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
    <div class="toast-mark" aria-hidden="true"></div>
    <div class="toast-copy">
      <strong>${escapeHtml(title)}</strong>
      ${detail ? `<p>${escapeHtml(detail)}</p>` : ""}
    </div>
    <button class="toast-close" type="button" aria-label="閉じる" title="閉じる">×</button>
  `;
  toast.querySelector(".toast-close").addEventListener("click", () => dismissToast(toast));
  els.toastRegion.append(toast);
  requestAnimationFrame(() => toast.classList.add("visible"));
  if (!options.persist) {
    window.setTimeout(() => dismissToast(toast), options.duration ?? 4200);
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
  article.innerHTML = `<div class="body ${options.markdown ? "markdown-body" : ""}"></div>`;
  const body = article.querySelector(".body");
  if (options.markdown) {
    renderMarkdown(body, text);
  } else {
    body.textContent = text;
  }
  els.messages.append(article);
  scrollMessages({ force: role === "user" || shouldAutoScroll() });
  return article;
}

function renderMarkdown(element, source) {
  element.classList.add("markdown-body");
  element.innerHTML = DOMPurify.sanitize(marked.parse(source || ""));
}

function setBusy(busy) {
  state.busy = busy;
  els.indexButton.disabled = busy;
  els.sendButton.disabled = busy;
  els.cancelButton.disabled = !busy;
  els.questionInput.disabled = busy;
}

function shouldAutoScroll() {
  return state.autoScroll || isNearBottom();
}

function isNearBottom() {
  const distance = els.messages.scrollHeight - els.messages.scrollTop - els.messages.clientHeight;
  return distance < 96;
}

function scrollMessages(options = {}) {
  if (options.force) {
    els.messages.scrollTop = els.messages.scrollHeight;
    state.autoScroll = true;
  }
  renderJumpButton();
}

function renderJumpButton() {
  const show = !state.autoScroll && !isNearBottom();
  els.jumpToLatestButton.hidden = !show;
}

function formatNumber(value) {
  return new Intl.NumberFormat("ja-JP").format(value || 0);
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}
