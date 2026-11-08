// The Studio's browser glue (X-30, spec §20.2 / §25): loads the wasm module the
// Rust -> wasm pipeline emits (`scripts/build-studio.sh`'s artifact), drives the §25
// screens through the module's C-ABI exports, and rides the Core API only
// (`ATH-STUDIO-002`: the data source is `/api/v1`; the module renders what the API
// answered).
//
// Auth face (`ATH-SEC-005`, `ATH-CFG-006`): the page holds no secret on disk. When the
// API runs `auth.mode = "token"` the operator pastes the token once; it lives in
// `sessionStorage` (the tab's lifetime), never in `localStorage` and never in a file
// (the registry stores hashes only). `auth.mode = "none"` (loopback-only) sends no
// header at all. Every cross-origin call rides the API's CORS allow-list (X-30's API
// face): a page the API did not list gets no permission and the fetch fails closed.

const API_ROOT = globalThis.ATHENA_STUDIO_API_ROOT || "";
const SESSION_KEY = "athena.api.token";
let token = sessionStorage.getItem(SESSION_KEY) || "";
let authMode = "unknown";

// --- The Core API client: one fetch wrapper, the Bearer when the mode names one -------

async function api(path, options = {}) {
  const headers = new Headers(options.headers || {});
  headers.set("Accept", "application/json");
  if (options.body !== undefined) headers.set("Content-Type", "application/json");
  if (authMode === "token" && token) headers.set("Authorization", `Bearer ${token}`);
  const response = await fetch(`${API_ROOT}${path}`, { ...options, headers, mode: "cors" });
  if (response.status === 401) await askForToken();
  return response;
}

async function askForToken() {
  if (authMode !== "unknown") return;
  authMode = "token";
  const answer = globalThis.prompt("athena-api authenticates with a bearer token (auth.mode = \"token\"); the value lives in this tab only:");
  if (answer) {
    token = answer;
    sessionStorage.setItem(SESSION_KEY, answer);
  }
}

// --- The wasm module's export face -------------------------------------------------------
//
// The module exposes the C ABI the exports face documents: alloc / free (the input
// pair), athena_studio_last_len (the answer's length; the module's static mut global
// exports as an *immutable* wasm global, so the length rides a function), and the
// render functions. No wasm-bindgen: `WebAssembly.instantiateStreaming` is the loader.

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

function mount(html) {
  document.getElementById("app-root").replaceChildren();
  const section = document.createElement("div");
  section.innerHTML = html;
  document.getElementById("app-root").appendChild(section);
}

// --- The screens (§25): Dashboard / Task / Approvals / Live -------------------------------

async function dashboard() {
  const answer = await api("/api/v1/tasks");
  const rows = (await answer.json())["items"] ?? [];
  mount(call("athena_studio_render_dashboard", JSON.stringify(rows)));
}

async function task(id) {
  const answer = await api(`/api/v1/tasks/${id}`);
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

let stream = null;

async function live() {
  if (stream) stream.close();
  const seeded = (await (await api("/api/v1/tasks")).json())["items"] ?? [];
  let boardTasks = seeded.map((row) => ({
    id: row.id,
    state: row.state,
    title: row.title,
    instruction: "",
  }));
  mount(call("athena_studio_render_live", JSON.stringify({ tasks: boardTasks, chunk: "" })));
  // The live surface (P7-06): the Event Stream's `data:` payloads reduce into the board
  // through the module's own reduction face — no DOM code invents a row.
  stream = new EventSource(`${API_ROOT}/api/v1/events/stream`);
  stream.onmessage = (message) => {
    const chunk = `data: ${message.data}\n\n`;
    mount(call("athena_studio_render_live", JSON.stringify({ tasks: boardTasks, chunk })));
  };
  // The board's task rows refresh from the API on every Event (a state line alone is
  // not the row; the API carries the rest — ATH-STUDIO-002).
  stream.onopen = async () => {
    const fresh = (await (await api("/api/v1/tasks")).json())["items"] ?? [];
    boardTasks = fresh.map((row) => ({ id: row.id, state: row.state, title: row.title, instruction: "" }));
    mount(call("athena_studio_render_live", JSON.stringify({ tasks: boardTasks, chunk: "" })));
  };
}

// --- The navigation ----------------------------------------------------------------------

const SCREENS = {
  dashboard,
  approvals,
  live,
  task: () => task(idOf()),
};

function idOf() {
  return new URLSearchParams(location.search).get("task")?.trim() || "";
}

function navigate() {
  const view = (new URLSearchParams(location.search).get("view") || "dashboard").replace(/\W/g, "");
  (SCREENS[view] ?? dashboard)();
  for (const tab of document.querySelectorAll("nav a[data-view]")) {
    tab.classList.toggle("active", tab.dataset.view === view);
  }
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

// Boot: the shell, then the module, then the screen. The API's auth mode shows itself
// through the first call: a 401 is the `token` mode's refusal (the prompt appears) and
// anything else rides with the mode the page then honors.
(async () => {
  document.getElementById("app-root").innerHTML = `<nav>
    <a data-view="dashboard" href="?view=dashboard">Dashboard</a>
    <a data-view="approvals" href="?view=approvals">Approvals</a>
    <a data-view="live" href="?view=live">Live</a>
  </nav><main></main>`;
  try {
    await bootWasm();
  } catch (fault) {
    document.getElementById("app-root").insertAdjacentHTML("beforeend", `<p class="fault">the wasm module failed to boot: ${String(fault).replace(/</g, "&lt;")}</p>`);
    return;
  }
  navigate();
})();
