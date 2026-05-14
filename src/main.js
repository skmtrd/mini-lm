import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import "./styles.css";

const state = {
  snapshot: null,
  busy: false,
  currentRunId: null,
  currentAssistantBody: null,
  lastHits: [],
  lastFacts: [],
  lastAudit: null,
};

document.querySelector("#app").innerHTML = `
  <div class="shell">
    <aside class="sidebar">
      <header class="brand">
        <div>
          <h1>mini-lm</h1>
          <p>Native local source QA</p>
        </div>
        <span id="appStateBadge" class="badge">起動中</span>
      </header>

      <section class="panel">
        <div class="panel-title">
          <h2>DeepSeek</h2>
          <button id="saveSettingsButton" class="icon-text" type="button">保存</button>
        </div>
        <label class="field">
          <span>API key</span>
          <input id="apiKeyInput" type="password" autocomplete="off" placeholder="保存済みなら空欄でOK" />
        </label>
        <div class="split-actions">
          <button id="clearApiKeyButton" type="button">キー削除</button>
          <span id="apiKeyStatus" class="muted">未確認</span>
        </div>
        <label class="field">
          <span>Model</span>
          <select id="modelInput">
            <option value="deepseek-v4-flash">deepseek-v4-flash</option>
            <option value="deepseek-v4-pro">deepseek-v4-pro</option>
          </select>
        </label>
        <div class="two-cols">
          <label class="field">
            <span>Thinking</span>
            <select id="thinkingInput">
              <option value="false">disabled</option>
              <option value="true">enabled</option>
            </select>
          </label>
          <label class="field">
            <span>Reasoning</span>
            <select id="reasoningInput">
              <option value="high">high</option>
              <option value="max">max</option>
            </select>
          </label>
        </div>
        <div class="two-cols">
          <label class="field">
            <span>Temperature</span>
            <input id="temperatureInput" type="number" min="0" max="1" step="0.1" value="0.1" />
          </label>
          <label class="field">
            <span>Context chars</span>
            <input id="contextCharsInput" type="number" min="3000" max="80000" step="1000" value="12000" />
          </label>
        </div>
        <label class="field">
          <span>Comprehensive batch chars</span>
          <input id="batchCharsInput" type="number" min="2000" max="16000" step="500" value="6000" />
        </label>
        <p class="warning">APIキーはOSのKeychain/Credential Managerへ保存します。ログやDBには保存しません。</p>
      </section>

      <section class="panel source-panel">
        <div class="panel-title">
          <h2>Source</h2>
          <button id="chooseSourceButton" class="icon-text" type="button">選択</button>
        </div>
        <div id="sourcePath" class="path-box">未設定</div>
        <div class="source-actions">
          <button id="indexButton" class="primary" type="button">インデックス作成/更新</button>
          <button id="selectAllButton" type="button">全選択</button>
          <button id="clearSelectionButton" type="button">解除</button>
        </div>
        <div id="fileList" class="file-list"></div>
      </section>
    </aside>

    <main class="main">
      <section class="topbar">
        <div>
          <h2>質問</h2>
          <p>選択されたsourceだけから検索、抽出、検証して回答します。</p>
        </div>
        <div class="mode-switch" role="radiogroup" aria-label="answer mode">
          <label><input type="radio" name="mode" value="normal" checked />通常</label>
          <label><input type="radio" name="mode" value="comprehensive" />高精度網羅</label>
        </div>
      </section>

      <section id="messages" class="messages">
        <article class="message system">
          <div class="meta">system</div>
          <div class="body">sourceディレクトリを選び、インデックスを作成してから質問してください。</div>
        </article>
      </section>

      <section id="runStatus" class="run-status idle">
        <div class="status-dot"></div>
        <div>
          <strong id="runStatusTitle">待機中</strong>
          <span id="runStatusDetail">処理はありません</span>
        </div>
        <button id="cancelButton" type="button" disabled>停止</button>
      </section>

      <form id="questionForm" class="composer">
        <textarea id="questionInput" rows="3" placeholder="例: 扶養手当について、対象者・金額・条件・手続きを網羅して"></textarea>
        <div class="composer-actions">
          <button id="searchOnlyButton" type="button">検索だけ</button>
          <button id="clearMessagesButton" type="button">履歴クリア</button>
          <button id="sendButton" class="primary" type="submit">質問する</button>
        </div>
      </form>
    </main>

    <aside class="inspector">
      <section class="panel">
        <h2>状態</h2>
        <div id="stats" class="stats"></div>
      </section>
      <section class="panel">
        <h2>検索結果</h2>
        <div id="hitList" class="hit-list empty">まだ検索していません</div>
      </section>
      <section class="panel">
        <h2>抽出/検証</h2>
        <div id="factList" class="fact-list empty">高精度網羅の結果がここに出ます</div>
        <pre id="auditBox" class="audit-box"></pre>
      </section>
    </aside>
  </div>
`;

const $ = (selector) => document.querySelector(selector);

const els = {
  appStateBadge: $("#appStateBadge"),
  apiKeyInput: $("#apiKeyInput"),
  apiKeyStatus: $("#apiKeyStatus"),
  modelInput: $("#modelInput"),
  thinkingInput: $("#thinkingInput"),
  reasoningInput: $("#reasoningInput"),
  temperatureInput: $("#temperatureInput"),
  contextCharsInput: $("#contextCharsInput"),
  batchCharsInput: $("#batchCharsInput"),
  saveSettingsButton: $("#saveSettingsButton"),
  clearApiKeyButton: $("#clearApiKeyButton"),
  chooseSourceButton: $("#chooseSourceButton"),
  sourcePath: $("#sourcePath"),
  indexButton: $("#indexButton"),
  selectAllButton: $("#selectAllButton"),
  clearSelectionButton: $("#clearSelectionButton"),
  fileList: $("#fileList"),
  stats: $("#stats"),
  messages: $("#messages"),
  runStatus: $("#runStatus"),
  runStatusTitle: $("#runStatusTitle"),
  runStatusDetail: $("#runStatusDetail"),
  cancelButton: $("#cancelButton"),
  questionForm: $("#questionForm"),
  questionInput: $("#questionInput"),
  searchOnlyButton: $("#searchOnlyButton"),
  clearMessagesButton: $("#clearMessagesButton"),
  sendButton: $("#sendButton"),
  hitList: $("#hitList"),
  factList: $("#factList"),
  auditBox: $("#auditBox"),
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
    state.currentAssistantBody.textContent += payload.delta;
    els.messages.scrollTop = els.messages.scrollHeight;
  });
  await refreshSnapshot();
}

function wireEvents() {
  els.saveSettingsButton.addEventListener("click", () => saveSettings(false));
  els.clearApiKeyButton.addEventListener("click", () => saveSettings(true));
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
    setStatus("停止要求", "現在の処理へキャンセル要求を送りました", true);
  });
  els.searchOnlyButton.addEventListener("click", searchOnly);
  els.clearMessagesButton.addEventListener("click", () => {
    els.messages.innerHTML = "";
    addMessage("system", "履歴をクリアしました。");
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
  els.appStateBadge.textContent = documents.length ? "読込済み" : "未読込";
  els.apiKeyStatus.textContent = settings.apiKeySaved ? "APIキー保存済み" : "APIキー未保存";
  els.modelInput.value = settings.model || "deepseek-v4-flash";
  els.thinkingInput.value = String(Boolean(settings.thinkingEnabled));
  els.reasoningInput.value = settings.reasoningEffort || "high";
  els.temperatureInput.value = settings.temperature ?? 0.1;
  els.contextCharsInput.value = settings.maxContextChars ?? 12000;
  els.batchCharsInput.value = settings.comprehensiveBatchChars ?? 6000;
  els.sourcePath.textContent = settings.sourcePath || "未設定";
  renderStats(stats);
  renderFileList(documents);
}

function renderStats(stats) {
  els.stats.innerHTML = `
    <dl>
      <div><dt>文書</dt><dd>${formatNumber(stats.selectedDocumentCount)} / ${formatNumber(stats.documentCount)}</dd></div>
      <div><dt>文字</dt><dd>${formatNumber(stats.selectedChars)} / ${formatNumber(stats.totalChars)}</dd></div>
      <div><dt>チャンク</dt><dd>${formatNumber(stats.chunkCount)}</dd></div>
      <div><dt>ベクトル</dt><dd>${formatNumber(stats.embeddingCount)}</dd></div>
    </dl>
  `;
}

function renderFileList(documents) {
  if (!documents.length) {
    els.fileList.innerHTML = `<div class="empty-box">まだインデックスがありません</div>`;
    return;
  }
  els.fileList.innerHTML = documents
    .map((doc) => {
      const statusClass = doc.status === "indexed" ? "ok" : doc.status === "error" ? "bad" : "warn";
      return `
        <label class="file-row">
          <input type="checkbox" data-doc-id="${doc.id}" ${doc.selected ? "checked" : ""} />
          <span>
            <strong>${escapeHtml(doc.fileName)}</strong>
            <small>${formatNumber(doc.charCount)}字 / ${formatNumber(doc.chunkCount)} chunks</small>
            ${doc.error ? `<em>${escapeHtml(doc.error)}</em>` : ""}
          </span>
          <b class="${statusClass}">${escapeHtml(doc.status)}</b>
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

async function saveSettings(clearApiKey) {
  const update = {
    model: els.modelInput.value,
    thinkingEnabled: els.thinkingInput.value === "true",
    reasoningEffort: els.reasoningInput.value,
    temperature: Number(els.temperatureInput.value || 0.1),
    maxContextChars: Number(els.contextCharsInput.value || 12000),
    comprehensiveBatchChars: Number(els.batchCharsInput.value || 6000),
    apiKey: clearApiKey ? null : els.apiKeyInput.value.trim() || null,
    clearApiKey,
  };
  await invoke("save_settings", { update });
  els.apiKeyInput.value = "";
  await refreshSnapshot();
  setStatus("設定保存", clearApiKey ? "APIキーを削除しました" : "設定を保存しました", false);
}

async function indexSource() {
  if (state.busy) return;
  setBusy(true);
  try {
    const summary = await invoke("index_source_directory");
    await refreshSnapshot();
    setStatus(
      "インデックス完了",
      `scan ${summary.scannedFiles}, indexed ${summary.indexedFiles}, skipped ${summary.skippedFiles}, failed ${summary.failedFiles}`,
      false,
    );
  } catch (error) {
    setStatus("インデックス停止", String(error), false, "bad");
  } finally {
    setBusy(false);
  }
}

async function searchOnly() {
  const query = els.questionInput.value.trim();
  if (!query) return;
  setBusy(true);
  try {
    const response = await invoke("hybrid_search", {
      request: {
        query,
        selectedDocumentIds: selectedDocumentIds(),
        limit: 16,
      },
    });
    state.lastHits = response.hits || [];
    renderHits(state.lastHits);
    setStatus("検索完了", `${state.lastHits.length}件 / vector scan ${response.scannedVectors}`, false);
  } catch (error) {
    setStatus("検索失敗", String(error), false, "bad");
  } finally {
    setBusy(false);
  }
}

async function answerQuestion() {
  const question = els.questionInput.value.trim();
  if (!question || state.busy) return;
  setBusy(true);
  addMessage("user", question);
  const assistant = addMessage("assistant", "");
  state.currentAssistantBody = assistant.querySelector(".body");
  state.currentRunId = null;
  renderFacts([], null);
  try {
    const mode = document.querySelector("input[name='mode']:checked")?.value || "normal";
    const response = await invoke("answer_question", {
      request: {
        question,
        mode,
        selectedDocumentIds: selectedDocumentIds(),
      },
    });
    state.currentRunId = response.runId;
    if (!state.currentAssistantBody.textContent.trim()) {
      state.currentAssistantBody.textContent = response.answer;
    }
    state.lastHits = response.hits || [];
    state.lastFacts = response.facts || [];
    state.lastAudit = response.audit || null;
    renderHits(state.lastHits);
    renderFacts(state.lastFacts, state.lastAudit);
    setStatus("回答完了", `run ${response.runId}`, false);
  } catch (error) {
    state.currentAssistantBody.textContent += `\n\n回答に失敗しました。処理は停止しました。\n理由: ${String(error)}`;
    setStatus("回答停止", String(error), false, "bad");
  } finally {
    state.currentAssistantBody = null;
    state.currentRunId = null;
    setBusy(false);
  }
}

function selectedDocumentIds() {
  return (state.snapshot?.documents || []).filter((doc) => doc.selected).map((doc) => doc.id);
}

function renderProgress(payload) {
  if (payload.runId) state.currentRunId = payload.runId;
  const total = payload.total ? ` ${payload.completed}/${payload.total}` : "";
  const elapsed = payload.elapsedMs ? ` / ${Math.round(payload.elapsedMs / 1000)}秒` : "";
  setStatus(
    `${payload.stage}: ${payload.status}`,
    `${payload.message}${total}${elapsed}`,
    Boolean(payload.canCancel),
    payload.status === "stopped" || payload.status === "cancelled" ? "bad" : "running",
  );
}

function renderHits(hits) {
  if (!hits.length) {
    els.hitList.className = "hit-list empty";
    els.hitList.textContent = "該当する検索結果はありません";
    return;
  }
  els.hitList.className = "hit-list";
  els.hitList.innerHTML = hits
    .map(
      (hit, index) => `
        <article class="hit">
          <header>
            <strong>S${index + 1}. ${escapeHtml(hit.fileName)}</strong>
            <span>${hit.score.toFixed(4)}</span>
          </header>
          <small>${escapeHtml(hit.headingPath || "見出しなし")} / chars ${hit.charStart}-${hit.charEnd}</small>
          <p>${escapeHtml(hit.snippet)}</p>
          <code>${escapeHtml(hit.debug)}</code>
        </article>
      `,
    )
    .join("");
}

function renderFacts(facts, audit) {
  if (!facts.length) {
    els.factList.className = "fact-list empty";
    els.factList.textContent = "抽出事実はまだありません";
  } else {
    els.factList.className = "fact-list";
    els.factList.innerHTML = facts
      .map(
        (fact, index) => `
          <article class="fact">
            <strong>F${index + 1}. ${escapeHtml(fact.fileName)}</strong>
            <p>${escapeHtml(fact.statement)}</p>
            <small>${escapeHtml(fact.quote || "")}</small>
          </article>
        `,
      )
      .join("");
  }
  els.auditBox.textContent = audit || "";
}

function addMessage(role, text) {
  const article = document.createElement("article");
  article.className = `message ${role}`;
  article.innerHTML = `<div class="meta">${role}</div><div class="body"></div>`;
  article.querySelector(".body").textContent = text;
  els.messages.append(article);
  els.messages.scrollTop = els.messages.scrollHeight;
  return article;
}

function setBusy(busy) {
  state.busy = busy;
  els.indexButton.disabled = busy;
  els.sendButton.disabled = busy;
  els.searchOnlyButton.disabled = busy;
  els.saveSettingsButton.disabled = busy;
}

function setStatus(title, detail, canCancel, tone = "idle") {
  els.runStatus.className = `run-status ${tone}`;
  els.runStatusTitle.textContent = title;
  els.runStatusDetail.textContent = detail;
  els.cancelButton.disabled = !canCancel;
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
