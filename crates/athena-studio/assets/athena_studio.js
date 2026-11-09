// The Studio's browser glue (X-30, spec §20.2 / §25): loads the wasm module the
// Rust → wasm pipeline emits (`scripts/build-studio.sh`'s artifact), drives the §25
// screens through the module's C-ABI exports, and rides the Core API only
// (`ATH-STUDIO-002`: the data source is `/api/v1`; the module renders what the API
// answered).
//
// Auth face (`ATH-SEC-005`, `ATH-CFG-006`): the page holds no secret on disk. When the
// API runs `auth.mode = "token"` the operator pastes the token once per tab — it lives
// in `sessionStorage`, never in `localStorage` and never in a file (the registry stores
// hashes only). `auth.mode = "none"` (loopback-only) sends no header at all. Every
// cross-origin call rides the API's CORS allow-list: a page the API did not list gets
// no permission and the fetch fails closed.
//
// The six defect faces session 013 recorded, closed by this rewrite:
// (c) the navigation shell is the page's own `#tabs` element, outside `#app-root` and
//     re-drawn by every render — the first screen's `replaceChildren` can no longer
//     consume it, so in-tab navigation survives every screen;
// (d) the token is adopted from this tab's own `sessionStorage` at boot, and the 401
//     asks through the page's own panel, once per page load — `window.prompt` is gone,
//     and a tab that already pasted re-rides its token without a panel;
// (e) the live surface streams through a Bearer-bearing fetch replay over
//     `/api/v1/events/stream` with `Last-Event-ID` as the gap anchor (`ATH-EVENT-020`)
//     — an `EventSource` cannot carry the token header, so the EventSource leg is gone;
//     the frames (the §23.1 envelope's `type` / `task_id` / `occurred_at` face) reduce
//     through the module's own render export and the tail lives here, capped to the
//     board's own count;
// (f) no Task is ever created from the page — the workspace is the operator's own
//     act (`R-DATA-005`, the one-lease discipline: the CLI's `athena task run` creates
//     the Task in the operator's workspace, the page only renders what the API holds),
//     so the page carries no create affordance at all.

const API_ROOT = globalThis.ATHENA_STUDIO_API_ROOT || "";
const SESSION_KEY = "athena.api.token";
const TOKEN_PANEL_ID = "athena-token-panel";
const NAV_ID = "tabs";
const ROOT_ID = "app-root";
const TAIL_CAP = 100; // the board's own tail count (live.rs), mirrored by the accumulating glue

// (d)'s repair, first half: the token is adopted from the tab's own `sessionStorage`
// at boot, so `authMode` never initializes to a lying "unknown" when this tab has
// already pasted — the first fetch rides the Bearer.
let token = sessionStorage.getItem(SESSION_KEY) || "";
let authMode = token ? "token" : "unknown";

// --- The Core API client: one fetch wrapper, the Bearer when the mode names one -------

async function api(path, options = {}, retried = false) {
  const headers = new Headers(options.headers || {});
  headers.set("Accept", "application/json");
  if (options.body !== undefined) headers.set("Content-Type", "application/json");
  if (authMode === "token" && token) headers.set("Authorization", `Bearer ${token}`);
  const response = await fetch(`${API_ROOT}${path}`, { ...options, headers, mode: "cors" });
  if (response.status === 401 && !retried) {
    // (d)'s repair, second half: the panel is the page's own DOM, once per page
    // load; the retry rides the token the tab now holds, at most once per call.
    if (await askForToken()) return api(path, options, true);
  }
  return response;
}

// The panel is a once-per-page face: `tokenPanelDone` is the module-level guard — the
// first 401 opens it, a paste adopts the token, a decline (or a later 401 after a
// paste) never re-opens it; the operator refreshes the page to go again.
let tokenPanelDone = false;

async function askForToken() {
  if (authMode === "token" && token) return true;
  if (tokenPanelDone) return false;
  tokenPanelDone = true;
  const pasted = await tokenPanel();
  if (pasted) {
    token = pasted;
    sessionStorage.setItem(SESSION_KEY, pasted);
    authMode = "token";
    return true;
  }
  return false;
}

// The page's own token panel: one line of form, no native dialog, Enter submits.
function tokenPanel() {
  return new Promise((resolve) => {
    let panel = document.getElementById(TOKEN_PANEL_ID);
    if (!panel) {
      panel = document.createElement("div");
      panel.id = TOKEN_PANEL_ID;
      panel.innerHTML = `
        <label for="athena-token-input" style="display:block">athena-api authenticates with a bearer token (auth.mode = "token"); the value lives in this tab only:</label>
        <input id="athena-token-input" type="password" style="width:24rem" autocomplete="off">
        <button id="athena-token-submit" type="button">submit</button>`;
      panel.querySelector("#athena-token-submit").addEventListener("click", done);
      panel.querySelector("#athena-token-input").addEventListener("keydown", (event) => {
        if (event.key === "Enter") done(event);
      });
      document.body.appendChild(panel);
    }
    const input = panel.querySelector("#athena-token-input");
    input.focus();
    function done(event) {
      event?.preventDefault();
      const answer = input.value.trim();
      panel.remove();
      resolve(answer);
    }
  });
}

// --- The wasm module's export face -------------------------------------------------------
//
// The module exposes the C ABI the exports face documents: alloc / free (the input
// pair), `athena_studio_last_len` (the answer's length — a *function*: the module's
// static-mut global exports as an immutable global, so the length rides a call), and
// the render functions. No wasm-bindgen: `WebAssembly.instantiateStreaming` is the
// loader.

let wasm = null;

async function bootWasm() {
  const stream = fetch("/athena_studio.wasm");
  const { instance } = await WebAssembly.instantiateStreaming(stream, {});
  wasm = instance.exports;
  if (!wasm.athena_studio_alloc) throw new Error("the wasm module carries no athena_studio exports");
}

function call(name, text) {
  const input = new TextEncoder().encode(text);
  const inPtr = wasm.athena_studio_alloc(input.length);
  new Uint8Array(wasm.memory.buffer).set(input, inPtr);
  const outPtr = wasm[name](inPtr, input.length);
  const outLen = wasm.athena_studio_last_len();
  const body = new TextDecoder().decode(
    new Uint8Array(wasm.memory.buffer).subarray(outPtr, outPtr + outLen),
  );
  wasm.athena_studio_free(inPtr, input.length);
  return body;
}

// --- The shell: the navigation lives outside the screen's reach (c) ----------------------

function ensureNav() {
  let nav = document.getElementById(NAV_ID);
  if (!nav) {
    nav = document.createElement("nav");
    nav.id = NAV_ID;
    (document.getElementById(ROOT_ID) ?? document.body).before(nav);
  }
  nav.replaceChildren();
  for (const [view, label] of [
    ["dashboard", "Dashboard"],
    ["approvals", "Approvals"],
    ["live", "Live"],
  ]) {
    const tab = document.createElement("a");
    tab.dataset.view = view;
    tab.href = `?view=${view}`;
    tab.textContent = label;
    nav.appendChild(tab);
  }
  for (const tab of nav.querySelectorAll("a[data-view]")) {
    tab.classList.toggle("active", tab.dataset.view === viewOf());
  }
}

function mount(html) {
  const root = document.getElementById(ROOT_ID);
  root.replaceChildren();
  const section = document.createElement("div");
  section.innerHTML = html;
  root.appendChild(section);
  ensureNav();
}

// --- The screens (§25): Dashboard / Task / Approvals / Live -------------------------------

async function dashboard() {
  const answer = await api("/api/v1/tasks");
  const rows = (await answer.json())["items"] ?? [];
  mount(call("athena_studio_render_dashboard", JSON.stringify(rows)));
}

async function task(id) {
  const answer = await api(`/api/v1/tasks/${encodeURIComponent(id)}`);
  if (!answer.ok) {
    mount(`<p>the task answered HTTP ${answer.status}</p>`);
    return;
  }
  mount(call("athena_studio_render_task", JSON.stringify(await answer.json())));
}

async function approvals() {
  const answer = await api("/api/v1/approvals");
  const rows = (await answer.json())["items"] ?? [];
  mount(call("athena_studio_render_approvals", JSON.stringify(rows)));
  // The actionable plans (P7-05): every rendered control posts its own endpoint, then
  // the screen re-fetches (the canonical descriptor stays the screen's first face).
  for (const button of document.querySelectorAll("#app-root button[data-endpoint]")) {
    button.addEventListener("click", async () => {
      await api(button.dataset.endpoint, { method: "POST" });
      approvals();
    });
  }
}

// The live surface (P7-06, R-STU-03): the board seeds from the Tasks API; a
// Bearer-bearing fetch replay rides `/api/v1/events/stream`, the frames' `data:` lines
// accumulate into the glue's own capped tail, and each render hands the module the
// board rows plus the accumulated chunk — the module reduces and renders, the glue
// keeps no state a second face could contradict. `Last-Event-ID` (the sequence the API
// stamps on every frame, `ATH-EVENT-020`) anchors the reconnect across a gap.
let liveHandle = null;

async function live() {
  const handle = { stopped: false };
  if (liveHandle) liveHandle.stopped = true;
  liveHandle = handle;

  const seeded = (await (await api("/api/v1/tasks")).json())["items"] ?? [];
  const boardTasks = seeded.map((row) => ({
    id: row.id,
    state: row.state,
    title: row.title,
    instruction: "",
  }));
  const tail = [];
  let since = 0;

  const render = () => {
    if (handle.stopped) return;
    mount(call("athena_studio_render_live", JSON.stringify({ tasks: boardTasks, chunk: tail.join("\n") + "\n" })));
  };

  // The reconnect (and the boot): the Tasks API refreshes the rows the board holds —
  // a Task first seen through the stream keeps only its observed facts until the API
  // names it (`ATH-API-001`; the render's own minimal row comes back from the chunk).
  const refresh = async () => {
    const answer = await api("/api/v1/tasks");
    if (!answer.ok || handle.stopped) return;
    const fresh = (await answer.json())["items"] ?? [];
    boardTasks.splice(
      0,
      boardTasks.length,
      ...fresh.map((row) => ({ id: row.id, state: row.state, title: row.title, instruction: "" })),
    );
    render();
  };

  // One replay round: stream until the socket closes, then wait and re-ride with the
  // gap anchor; a refusal (401 — the panel flow api() already ran) backs off.
  const worker = (async () => {
    while (!handle.stopped) {
      try {
        const headers = new Headers({ Accept: "text/event-stream" });
        if (authMode === "token" && token) headers.set("Authorization", `Bearer ${token}`);
        if (since) headers.set("Last-Event-ID", String(since));
        const answer = await fetch(`${API_ROOT}/api/v1/events/stream`, { headers, mode: "cors" });
        if (!answer.ok || !answer.body) {
          if (handle.stopped) return;
          await wait(answer.status === 401 ? 5000 : 1000);
          continue;
        }
        await readFrames(answer, (frame) => {
          const line = frame.split("\n").find((row) => row.startsWith("data: "));
          if (!line) return;
          tail.push(line);
          if (tail.length > TAIL_CAP) tail.splice(0, tail.length - TAIL_CAP);
          try {
            const face = JSON.parse(line.slice(6));
            if (typeof face.sequence === "number") since = Math.max(since, face.sequence);
          } catch {
            // the board's own discipline: a malformed frame is skipped, not invented
          }
        });
        if (handle.stopped) return;
        await refresh();
        await wait(1000);
      } catch {
        if (handle.stopped) return;
        await wait(1000);
      }
    }
  })();

  render();
  await worker;
}

async function readFrames(answer, onFrame) {
  const reader = answer.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    const frames = buffer.split("\n\n");
    buffer = frames.pop() ?? "";
    for (const frame of frames) {
      if (frame.trim()) onFrame(frame);
    }
  }
}

function wait(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// --- The navigation ----------------------------------------------------------------------

const SCREENS = {
  dashboard,
  approvals,
  live,
  task: () => task(taskIdOf()),
};

function viewOf() {
  return new URLSearchParams(location.search).get("view")?.replace(/\W/g, "") || "dashboard";
}

function taskIdOf() {
  return new URLSearchParams(location.search).get("task")?.trim() || "";
}

function navigate() {
  (SCREENS[viewOf()] ?? dashboard)();
}

document.addEventListener("click", (event) => {
  const tab = event.target.closest("nav a[data-view]");
  if (!tab) return;
  event.preventDefault();
  const url = new URL(location.href);
  url.searchParams.set("view", tab.dataset.view);
  history.pushState({}, "", url);
  navigate();
});

globalThis.addEventListener("popstate", navigate);

// Boot: the shell's tabs first (they ride outside `#app-root`, (c)), then the module,
// then the screen. The API's auth mode shows itself through the first call: a 401 is
// the `token` mode's refusal and answers through the page's own panel ((d)).
(async () => {
  ensureNav();
  try {
    await bootWasm();
  } catch (fault) {
    const root = document.getElementById(ROOT_ID);
    if (root) {
      root.innerHTML = `<p class="fault">the wasm module failed to boot: ${String(fault).replace(/</g, "&lt;")}</p>`;
    }
    return;
  }
  navigate();
})();
