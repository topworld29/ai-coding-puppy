import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { PixelGrid, PALETTE, SIZE, drawPet, PetVisualState } from "./sprite";

interface SessionInfo {
  id: string;
  source: string;
  name: string;
  state: string;
  detail: string;
  updated_ms: number;
  created_ms: number;
  needs_ack: boolean;
}

interface PetStatePayload {
  aggregate: string;
  sessions: SessionInfo[];
  has_working: boolean;
  pending_count: number;
}

let aggregate = "sleeping";
let sessions: SessionInfo[] = [];
let hasWorking = false;
let pendingCount = 0;
let actionMessage = "";
let actionMessageTimer: number | undefined;

const grid = new PixelGrid();
const off = document.createElement("canvas");
off.width = SIZE;
off.height = SIZE;
const offCtx = off.getContext("2d")!;
const imgData = offCtx.createImageData(SIZE, SIZE);

const canvas = document.getElementById("pet") as HTMLCanvasElement;
const ctx = canvas.getContext("2d")!;
ctx.imageSmoothingEnabled = false;

const panel = document.getElementById("panel") as HTMLDivElement;
canvas.tabIndex = 0;
canvas.setAttribute("role", "button");
canvas.setAttribute("aria-label", "打开或关闭会话列表");
canvas.setAttribute("aria-controls", "panel");

function setPanelHidden(hidden: boolean) {
  panel.classList.toggle("hidden", hidden);
  canvas.setAttribute("aria-expanded", String(!hidden));
}

function paintGrid() {
  const px = imgData.data;
  for (let i = 0; i < SIZE * SIZE; i++) {
    const c = grid.data[i];
    const p = i * 4;
    if (c === 0) {
      px[p + 3] = 0;
    } else {
      const hex = PALETTE[c];
      px[p] = parseInt(hex.slice(1, 3), 16);
      px[p + 1] = parseInt(hex.slice(3, 5), 16);
      px[p + 2] = parseInt(hex.slice(5, 7), 16);
      px[p + 3] = 255;
    }
  }
  offCtx.putImageData(imgData, 0, 0);
}

function visualState(): PetVisualState {
  if (hasWorking) return "working";
  return aggregate as PetVisualState;
}

function fmtElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h > 0) return `${h}小时${m}分`;
  if (m > 0) return `${m}分${sec}秒`;
  return `${sec}秒`;
}

function renderPanel() {
  canvas.setAttribute("aria-expanded", String(!panel.classList.contains("hidden")));
  if (panel.classList.contains("hidden")) return;
  const previousScrollTop = panel.scrollTop;
  const visible = sessions.filter((s) => s.state !== "idle");
  if (visible.length === 0) {
    panel.innerHTML =
      '<div class="panel-title">0个会话</div><div class="empty">Claude Code / OpenCode / Codex / ZCode<br />跑起来我就会醒啦</div>';
    panel.scrollTop = 0;
    return;
  }
  const bySource = new Map<string, SessionInfo[]>();
  for (const s of visible) {
    const list = bySource.get(s.source) ?? [];
    list.push(s);
    bySource.set(s.source, list);
  }
  const rows: string[] = [];
  for (const [source, list] of bySource) {
    list.sort((a, b) => a.created_ms - b.created_ms);
    const base = source === "claude"
      ? "claude code"
      : source === "codex"
        ? "codex"
        : source === "zcode"
          ? "zcode"
          : "opencode";
    list.forEach((s) => {
      const status = s.state === "working"
        ? `运行中 · ${fmtElapsed(Date.now() - s.created_ms)}`
        : s.state === "error"
          ? "需要检查"
          : s.state === "waiting"
            ? "等待输入"
            : "已完成";
      const st = s.state;
      const sessionName = s.name.trim() || "未命名会话";
      const encodedSource = encodeURIComponent(s.source);
      const encodedSession = encodeURIComponent(s.id);
      const mute = `<button class="session-action mute-action" data-action="mute" data-source="${encodedSource}" data-session="${encodedSession}" title="隐藏并静音此会话" aria-label="隐藏并静音此会话">&#10005;</button>`;
      const action = s.state !== "done"
        ? `<button class="session-action open-action" data-action="open" data-source="${encodedSource}" data-session="${encodedSession}" title="打开对应窗口" aria-label="打开对应窗口">↗</button>`
        : `<button class="session-action clear-action" data-action="clear" data-source="${encodedSource}" data-session="${encodedSession}" title="清除这条消息" aria-label="清除这条消息">&#10003;</button>`;
      rows.push(`<div class="session">
        ${mute}
        <span class="dot state-${st}"></span>
        <span class="meta">
          <span class="line1"><span class="src">${base}</span><span class="state state-${st}">${status}</span></span>
          <span class="session-name" title="${escapeHtml(sessionName)}">${escapeHtml(sessionName)}</span>
        </span>
        ${action}
      </div>`);
    });
  }
  const hint = actionMessage ? `<span class="panel-message">${escapeHtml(actionMessage)}</span>` : "";
  panel.innerHTML = `<div class="panel-title"><span>${visible.length}个会话</span>${hint}</div>${rows.join("")}`;
  panel.scrollTop = previousScrollTop;
}

function escapeHtml(s: string) {
  return s.replace(/[&<>"']/g, (ch) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[ch] as string)
  );
}

function onState(payload: PetStatePayload) {
  aggregate = payload.aggregate;
  sessions = payload.sessions;
  hasWorking = payload.has_working;
  pendingCount = payload.pending_count;
  renderPanel();
}

function showActionMessage(message: string) {
  actionMessage = message;
  if (actionMessageTimer !== undefined) window.clearTimeout(actionMessageTimer);
  renderPanel();
  actionMessageTimer = window.setTimeout(() => {
    actionMessage = "";
    renderPanel();
  }, 2400);
}

panel.addEventListener("click", async (event) => {
  const target = event.target as HTMLElement;
  const button = target.closest<HTMLButtonElement>("button.session-action");
  if (!button) return;
  event.stopPropagation();

  const source = decodeURIComponent(button.dataset.source ?? "");
  const sessionId = decodeURIComponent(button.dataset.session ?? "");
  button.disabled = true;

  try {
    if (button.dataset.action === "mute" || button.dataset.action === "clear") {
      const command = button.dataset.action === "mute" ? "mute_session" : "dismiss_session";
      const nextState = await invoke<PetStatePayload>(command, {
        source,
        sessionId,
      });
      onState(nextState);
      const visibleSessions = nextState.sessions.filter((session) => session.state !== "idle");
       if (visibleSessions.length === 0) setPanelHidden(true);
    } else {
      const focused = await invoke<boolean>("focus_session_window", {
        source,
      });
      if (focused) {
        setPanelHidden(true);
      } else {
        showActionMessage("没有找到对应窗口");
      }
    }
  } catch (error) {
    console.error("session action failed", error);
    showActionMessage("操作失败，请再试一次");
  } finally {
    button.disabled = false;
  }
});

function loop(t: number) {
  drawPet(grid, visualState(), t, pendingCount > 0);
  paintGrid();
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  ctx.drawImage(off, 0, 0, canvas.width, canvas.height);
  requestAnimationFrame(loop);
}

let downX = 0;
let downY = 0;
let downActive = false;
let moved = false;

async function togglePanel() {
  if (pendingCount > 0) {
    try {
      onState(await invoke<PetStatePayload>("acknowledge_done"));
    } catch (error) {
      console.error("failed to acknowledge completion", error);
    }
  }
  setPanelHidden(!panel.classList.contains("hidden"));
  renderPanel();
}

canvas.addEventListener("pointerdown", (e) => {
  if (e.button !== 0) return;
  downX = e.clientX;
  downY = e.clientY;
  downActive = true;
  moved = false;
});

window.addEventListener("pointermove", (e) => {
  if (!downActive || moved) return;
  if (Math.abs(e.clientX - downX) > 3 || Math.abs(e.clientY - downY) > 3) {
    moved = true;
    getCurrentWindow().startDragging();
  }
});

window.addEventListener("pointerup", async (e) => {
  if (!downActive) return;
  downActive = false;
  if (
    !moved &&
    e.target === canvas &&
    Math.abs(e.clientX - downX) < 4 &&
    Math.abs(e.clientY - downY) < 4
  ) {
    await togglePanel();
  }
});

canvas.addEventListener("keydown", (event) => {
  if (event.key !== "Enter" && event.key !== " ") return;
  event.preventDefault();
  void togglePanel();
});

// 屏蔽 WebView 默认右键菜单：窗口内任何位置（含透明区域与小狗 canvas）都不弹出
document.addEventListener("contextmenu", (event) => event.preventDefault());

window.addEventListener("pointercancel", () => {
  downActive = false;
});

document.addEventListener("DOMContentLoaded", () => {
  invoke<PetStatePayload>("get_state").then(onState);
});

listen<PetStatePayload>("pet-state", (e) => onState(e.payload));

setInterval(renderPanel, 1000);

requestAnimationFrame(loop);
