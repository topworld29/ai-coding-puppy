import { invoke } from "@tauri-apps/api/core";
import { listen, emit } from "@tauri-apps/api/event";
import { getCurrentWindow, Window } from "@tauri-apps/api/window";
import { PhysicalPosition, LogicalSize } from "@tauri-apps/api/dpi";
import { PixelGrid, PALETTE, SIZE, drawPet, PetVisualState } from "./sprite";

document.title = "小狗桌宠";

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

// 双窗口固定尺寸架构：pet 窗口只含小狗（56×56），panel 窗口只含会话面板（268×148，默认隐藏）。
// 两个窗口都在配置中固定尺寸、运行时永不 resize（resize 会触发 WebView2 透明丢失），
// 面板开关 = 独立窗口的 show/hide + 定位（仅位置变更，安全）。
const currentWindow = getCurrentWindow();
const isPetWindow = currentWindow.label === "pet";
const PANEL_LABEL = "panel";
const PANEL_W = 268;
const PANEL_H = 148;
const PANEL_GAP = 12;

let aggregate = "sleeping";
let sessions: SessionInfo[] = [];
let hasWorking = false;
let pendingCount = 0;

function fmtElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h > 0) return `${h}小时${m}分`;
  if (m > 0) return `${m}分${sec}秒`;
  return `${sec}秒`;
}

function escapeHtml(s: string) {
  return s.replace(/[&<>"']/g, (ch) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[ch] as string)
  );
}

let onStateRender: (() => void) | null = null;

function onState(payload: PetStatePayload) {
  aggregate = payload.aggregate;
  sessions = payload.sessions;
  hasWorking = payload.has_working;
  pendingCount = payload.pending_count;
  onStateRender?.();
}

if (isPetWindow) {
  const grid = new PixelGrid();
  const off = document.createElement("canvas");
  off.width = SIZE;
  off.height = SIZE;
  const offCtx = off.getContext("2d")!;
  const imgData = offCtx.createImageData(SIZE, SIZE);

  const canvas = document.getElementById("pet") as HTMLCanvasElement;
  document.getElementById("panel")?.remove();
  const ctx = canvas.getContext("2d")!;
  ctx.imageSmoothingEnabled = false;

  canvas.tabIndex = 0;
  canvas.setAttribute("role", "button");
  canvas.setAttribute("aria-label", "打开或关闭会话列表");
  canvas.setAttribute("aria-controls", "panel");
  canvas.setAttribute("aria-expanded", "false");

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

  async function setPanelVisible(visible: boolean) {
    const panelWindow = await Window.getByLabel(PANEL_LABEL);
    if (!panelWindow) return;
    if (visible) {
      const scale = await currentWindow.scaleFactor();
      const pos = await currentWindow.outerPosition();
      const size = await currentWindow.outerSize();
      const panelW = Math.round(PANEL_W * scale);
      const panelH = Math.round(PANEL_H * scale);
      const gap = Math.round(PANEL_GAP * scale);
      await panelWindow.setPosition(
        new PhysicalPosition(pos.x + size.width - panelW, pos.y - gap - panelH)
      );
      await panelWindow.show();
    } else {
      await panelWindow.hide();
    }
    canvas.setAttribute("aria-expanded", String(visible));
  }

  async function togglePanel() {
    if (pendingCount > 0) {
      try {
        onState(await invoke<PetStatePayload>("acknowledge_done"));
      } catch (error) {
        console.error("failed to acknowledge completion", error);
      }
    }
    const panelWindow = await Window.getByLabel(PANEL_LABEL);
    if (!panelWindow) return;
    const visible = await panelWindow.isVisible().catch(() => false);
    await setPanelVisible(!visible);
  }

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
      void setPanelVisible(false);
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

  window.addEventListener("pointercancel", () => {
    downActive = false;
  });

  // 焦点轮廓只在键盘操作时显示：canvas 非原生控件，鼠标点击聚焦也会命中 :focus-visible，
  // 需要用类名门控，避免点击/右键时出现描边框
  window.addEventListener("keydown", () => canvas.classList.add("keyboard-nav"));
  window.addEventListener("pointerdown", () => canvas.classList.remove("keyboard-nav"));

  // 面板窗口自行收起（清除最后一个会话/跳转成功）时同步 aria 状态
  listen<boolean>("panel-visibility", (e) =>
    canvas.setAttribute("aria-expanded", String(e.payload))
  );

  requestAnimationFrame(loop);
} else {
  const panel = document.getElementById("panel") as HTMLDivElement;
  document.getElementById("pet")?.remove();

  let actionMessage = "";
  let actionMessageTimer: number | undefined;

  onStateRender = renderPanelHook;
  function renderPanelHook() {
    renderPanel();
  }

  function renderPanel() {
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

  function showActionMessage(message: string) {
    actionMessage = message;
    if (actionMessageTimer !== undefined) window.clearTimeout(actionMessageTimer);
    renderPanel();
    actionMessageTimer = window.setTimeout(() => {
      actionMessage = "";
      renderPanel();
    }, 2400);
  }

  async function hideSelf() {
    await emit("panel-visibility", false);
    await currentWindow.hide();
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
        if (visibleSessions.length === 0) await hideSelf();
      } else {
        const focused = await invoke<boolean>("focus_session_window", {
          source,
        });
        if (focused) {
          await hideSelf();
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

  setInterval(renderPanel, 1000);
}

// 屏蔽 WebView 默认右键菜单：两个窗口内任何位置都不弹出
document.addEventListener("contextmenu", (event) => event.preventDefault());

document.addEventListener("DOMContentLoaded", () => {
  invoke<PetStatePayload>("get_state").then(onState);
  if (isPetWindow) {
    // Windows 在窗口创建时把 56×56 钳到默认最小 136×39 CSS。等页面首帧绘制完成后
    // （DOMContentLoaded + 延迟）再一次性恢复实际尺寸——初始加载期间 resize 会让
    // WebView2 的渲染面脱离（内容不显示）。这是运行期唯一一次 resize，此后两窗口
    // 永不 resize，面板开关走独立窗口 show/hide。右下角锚定，小狗屏幕位置不变。
    window.setTimeout(() => {
      void (async () => {
        try {
          await currentWindow.setMinSize(new LogicalSize(1, 1));
          const size = await currentWindow.outerSize();
          const pos = await currentWindow.outerPosition();
          const scale = await currentWindow.scaleFactor();
          const targetW = Math.round(56 * scale);
          const targetH = Math.round(56 * scale);
          if (size.width !== targetW || size.height !== targetH) {
            await currentWindow.setSize(new LogicalSize(56, 56));
            await currentWindow.setPosition(
              new PhysicalPosition(pos.x + size.width - targetW, pos.y + size.height - targetH)
            );
          }
        } catch (error) {
          console.error("failed to normalize pet window size", error);
        }
      })();
    }, 250);
  }
});

listen<PetStatePayload>("pet-state", (e) => onState(e.payload));
