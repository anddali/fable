"use strict";

/* ------------------------------------------------------------------ state */
const S = { cfg: null, msal: null, account: null, me: null, charts: [], chat: [] };
const $ = (sel, el = document) => el.querySelector(sel);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]
  );

/* ------------------------------------------------------------------- auth */
async function getToken() {
  if (S.cfg.auth_mode !== "entra") return null;
  const req = { scopes: [S.cfg.api_scope], account: S.account };
  try {
    return (await S.msal.acquireTokenSilent(req)).accessToken;
  } catch {
    await S.msal.acquireTokenRedirect(req);
  }
}

async function api(path, opts = {}) {
  const headers = { ...(opts.headers || {}) };
  if (opts.body && !(opts.body instanceof FormData)) {
    headers["content-type"] = "application/json";
    opts.body = JSON.stringify(opts.body);
  }
  const token = await getToken();
  if (token) headers["authorization"] = `Bearer ${token}`;
  const res = await fetch(path, { ...opts, headers });
  if (res.status === 204) return null;
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.detail ? JSON.stringify(data.detail) : res.statusText);
  return data;
}

async function boot() {
  S.cfg = await (await fetch("/ui-config", { headers: { accept: "application/json" } })).json();
  $("#env").textContent = `env · ${S.cfg.environment} · auth ${S.cfg.auth_mode}`;

  if (S.cfg.auth_mode === "entra") {
    S.msal = new msal.PublicClientApplication({
      auth: {
        clientId: S.cfg.spa_client_id,
        authority: `https://login.microsoftonline.com/${S.cfg.tenant_id}`,
        redirectUri: location.origin + "/ui/",
      },
      cache: { cacheLocation: "sessionStorage" },
    });
    await S.msal.initialize();
    const redirect = await S.msal.handleRedirectPromise();
    S.account = redirect?.account || S.msal.getAllAccounts()[0] || null;
    if (!S.account) {
      $("#boot").hidden = true;
      $("#login").hidden = false;
      $("#login-btn").onclick = () =>
        S.msal.loginRedirect({ scopes: [S.cfg.api_scope] });
      return;
    }
    S.msal.setActiveAccount(S.account);
    $("#signout").hidden = false;
    $("#signout").onclick = () => S.msal.logoutRedirect();
  }

  S.me = await api("/me");
  $("#who").innerHTML = `<div class="who-name">${esc(S.me.name || S.me.email || S.me.oid)}</div>
    <div class="who-role">${S.me.is_admin ? "admin" : "user"}</div>`;
  $("#boot").hidden = true;
  $("#app").hidden = false;
  if (!S.cfg.chat_enabled) $("#nav-chat")?.remove();
  window.addEventListener("hashchange", route);
  route();
}

/* ------------------------------------------------------------------ utils */
function toast(msg, err = false) {
  const el = document.createElement("div");
  el.className = "toast" + (err ? " toast-err" : "");
  el.textContent = msg;
  $("#toast-root").appendChild(el);
  setTimeout(() => el.remove(), 4200);
}

function modal(html) {
  const root = $("#modal-root");
  root.innerHTML = `<div class="overlay"><div class="modal">${html}</div></div>`;
  $(".overlay", root).addEventListener("mousedown", (e) => {
    if (e.target.classList.contains("overlay")) closeModal();
  });
  return root;
}
const closeModal = () => ($("#modal-root").innerHTML = "");

const fmtBytes = (n) => {
  if (!n) return "0 B";
  const u = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(u.length - 1, Math.floor(Math.log2(n) / 10));
  return `${(n / 2 ** (10 * i)).toFixed(i ? 1 : 0)} ${u[i]}`;
};
const fmtDate = (s) => (s ? new Date(s).toLocaleString() : "—");
const fmtDay = (s) => (s ? new Date(s).toLocaleDateString() : "—");

function destroyCharts() {
  S.charts.forEach((c) => c.destroy());
  S.charts = [];
}

function chart(canvas, config) {
  const css = getComputedStyle(document.body);
  Chart.defaults.color = css.getPropertyValue("--muted").trim();
  Chart.defaults.borderColor = css.getPropertyValue("--line").trim();
  Chart.defaults.font.family = css.getPropertyValue("--mono").trim();
  Chart.defaults.font.size = 10;
  const c = new Chart(canvas, config);
  S.charts.push(c);
  return c;
}

/* ---------------------------------------------------------------- figures */
// <img src> can't carry the bearer token, so figures are fetched manually
// and attached as object URLs. Cache per session to avoid refetching.
const _figureUrls = {};

const figureImgs = (figures) =>
  (figures || [])
    .map(
      (f) =>
        `<img class="figure-thumb" data-fig="${esc(f.key)}" alt="figure${f.page ? ` p${f.page}` : ""}" title="figure${f.page ? ` · page ${f.page}` : ""} — click to enlarge">`
    )
    .join("");

async function hydrateFigures(root) {
  for (const img of root.querySelectorAll("img.figure-thumb:not([src])")) {
    const key = img.dataset.fig;
    try {
      if (!_figureUrls[key]) {
        const token = await getToken();
        const res = await fetch(`/search/figures?key=${encodeURIComponent(key)}`, {
          headers: token ? { authorization: `Bearer ${token}` } : {},
        });
        if (!res.ok) throw new Error(res.statusText);
        _figureUrls[key] = URL.createObjectURL(await res.blob());
      }
      img.src = _figureUrls[key];
      img.onclick = (e) => {
        e.stopPropagation();
        modal(`<img class="figure-full" src="${_figureUrls[key]}" alt="figure">
        <div class="row spread section-gap"><span class="mono tiny dim">${esc(key)}</span>
        <button class="btn btn-ghost btn-sm" onclick="closeModal()">Close</button></div>`);
      };
    } catch {
      img.remove(); // missing blob (e.g. pre-feature index) — degrade silently
    }
  }
}

const visBadge = (v) =>
  v === "public"
    ? `<span class="badge badge-blue">public</span>`
    : `<span class="badge">private</span>`;
const roleBadge = (r) => (r ? `<span class="badge badge-green">${esc(r)}</span>` : "");
const statusBadge = (s) =>
  ({
    succeeded: `<span class="badge badge-green">succeeded</span>`,
    indexed: `<span class="badge badge-green">indexed</span>`,
    running: `<span class="badge badge-amber">running</span>`,
    pending: `<span class="badge badge-amber">pending</span>`,
    failed: `<span class="badge badge-red">failed</span>`,
    deleted: `<span class="badge">deleted</span>`,
  })[s] || `<span class="badge">${esc(s)}</span>`;

/* --------------------------------------------------------------- pipeline */
// Prefills for the vision config editor; everything stays editable.
const VISION_CFG_PRESETS = {
  "openai-compat": {
    base_url: "http://host.docker.internal:11434/v1",
    model: "gemma3:4b",
    api_key_env: "OLLAMA_API_KEY",
  },
  anthropic: { model: "claude-haiku-4-5-20251001", api_key_env: "ANTHROPIC_API_KEY" },
};

function pipelineFieldsHtml(plugins, pipeline = {}) {
  const parserOpts = [`<option value="">auto — pick by file type</option>`]
    .concat(
      plugins.parser.map(
        (p) =>
          `<option value="${esc(p.name)}" ${pipeline.parser?.name === p.name ? "selected" : ""}>${esc(p.name)}</option>`
      )
    )
    .join("");
  const visionOpts = [`<option value="">off — figures are dropped</option>`]
    .concat(
      plugins.vision.map(
        (p) =>
          `<option value="${esc(p.name)}" ${pipeline.vision_llm?.name === p.name ? "selected" : ""}>${esc(p.name)}</option>`
      )
    )
    .join("");
  const visionCfg = pipeline.vision_llm
    ? JSON.stringify(pipeline.vision_llm.config || {}, null, 2)
    : "";
  return `
  <label class="f"><span>parser</span><select id="pf-parser">${parserOpts}</select></label>
  <label class="f"><span>vision LLM — describes figures &amp; diagrams in PDFs</span>
    <select id="pf-vision">${visionOpts}</select></label>
  <label class="f" id="pf-vision-cfg-wrap" ${pipeline.vision_llm ? "" : "hidden"}>
    <span>vision config (json)</span><textarea id="pf-vision-cfg" rows="5">${esc(visionCfg)}</textarea></label>
  <div id="pf-hint" class="dim tiny mono"></div>`;
}

function wirePipelineFields(plugins) {
  const visionSel = $("#pf-vision");
  const hint = () => {
    const p = plugins.vision.find((x) => x.name === visionSel.value);
    const props = Object.keys(p?.config_schema?.properties || {});
    $("#pf-hint").textContent = props.length ? `vision fields: ${props.join(", ")}` : "";
  };
  visionSel.onchange = () => {
    const wrap = $("#pf-vision-cfg-wrap");
    wrap.hidden = !visionSel.value;
    if (visionSel.value)
      $("#pf-vision-cfg").value = JSON.stringify(
        VISION_CFG_PRESETS[visionSel.value] || {},
        null,
        2
      );
    hint();
  };
  hint();
}

// Merges the form into the existing pipeline so chunker/transformer
// customizations made via the API survive a UI edit.
function readPipelineFields(pipeline = {}) {
  const out = { ...pipeline };
  const parser = $("#pf-parser").value;
  out.parser = parser ? { name: parser, config: pipeline.parser?.config || {} } : null;
  const vision = $("#pf-vision").value;
  out.vision_llm = vision
    ? { name: vision, config: JSON.parse($("#pf-vision-cfg").value || "{}") }
    : null;
  return out;
}

/* ----------------------------------------------------------------- router */
function route() {
  destroyCharts();
  const hash = location.hash || "#/dashboard";
  const [, page, id] = hash.split("/");
  document.querySelectorAll("#nav a").forEach((a) => {
    a.classList.toggle("active", a.dataset.page === page);
  });
  const main = $("#main");
  main.innerHTML = `<div class="empty">loading…</div>`;
  const render =
    page === "collections" && id
      ? () => collectionDetail(main, id)
      : {
          dashboard,
          search: searchPage,
          chat: chatPage,
          collections: collectionsPage,
          keys: keysPage,
          docs: docsPage,
        }[page] || dashboard;
  Promise.resolve(render(main)).catch((e) => {
    main.innerHTML = `<div class="empty">error · ${esc(e.message)}</div>`;
  });
}

/* -------------------------------------------------------------- dashboard */
async function dashboard(main) {
  const [db, q] = await Promise.all([api("/analytics/db"), api("/analytics/queries?days=14")]);
  const d = db.documents || {};
  const docTotal = (d.indexed || 0) + (d.pending || 0) + (d.failed || 0);
  const adminTiles = S.me.is_admin
    ? `<div class="tile"><div class="k">queue jobs</div>
         <div class="v">${(db.queue?.todo || 0) + (db.queue?.doing || 0)}</div>
         <div class="s">${db.queue?.failed || 0} failed · ${db.queue?.succeeded || 0} done</div></div>
       <div class="tile"><div class="k">api keys</div>
         <div class="v">${db.api_keys?.active ?? 0}</div>
         <div class="s">${db.api_keys?.revoked ?? 0} revoked</div></div>`
    : "";

  main.innerHTML = `
  <div class="page-head">
    <div><div class="crumb">01 · overview</div><h1>Dashboard</h1></div>
    <span class="badge badge-green">last ${q.days} days</span>
  </div>
  <div class="grid tiles">
    <div class="tile"><div class="k">collections</div><div class="v">${db.collections.total}</div>
      <div class="s">${db.collections.public} public · ${db.collections.private} private</div></div>
    <div class="tile"><div class="k">documents</div><div class="v">${docTotal}</div>
      <div class="s">${d.indexed || 0} indexed${d.failed ? ` · <span class="bad">${d.failed} failed</span>` : ""}</div></div>
    <div class="tile"><div class="k">chunks</div><div class="v">${db.chunks ?? 0}</div>
      <div class="s">${fmtBytes(db.storage_bytes)} source data</div></div>
    <div class="tile"><div class="k">queries</div><div class="v">${q.totals.queries}</div>
      <div class="s">${q.totals.avg_latency_ms} ms avg · ${q.totals.avg_hits} hits avg</div></div>
    ${adminTiles}
  </div>
  <div class="grid two-col">
    <div class="card"><h2>query volume &amp; latency</h2><div class="chart-box"><canvas id="ch-q"></canvas></div></div>
    <div class="card"><h2>documents by status</h2><div class="chart-box"><canvas id="ch-d"></canvas></div></div>
  </div>
  <div class="grid two-col section-gap">
    <div class="card"><h2>recent queries</h2>${recentQueriesTable(q.recent)}</div>
    <div class="card"><h2>top queries</h2>${topQueriesTable(q.top_queries)}
      <div class="row spread section-gap">
        <span class="dim tiny">Run ad-hoc searches with collection filters &amp; full metadata.</span>
        <a class="btn btn-accent btn-sm" href="#/search">Open search →</a>
      </div>
    </div>
  </div>
  <div class="card section-gap"><h2>collections</h2>${perCollectionTable(db.per_collection)}</div>
  <div class="card section-gap"><h2>recent sync runs</h2>${syncRunsTable(db.sync_runs)}</div>`;

  drawQueryChart(q.per_day);
  drawDocChart(d);
}

function drawQueryChart(perDay) {
  const css = getComputedStyle(document.body);
  chart($("#ch-q"), {
    data: {
      labels: perDay.map((p) => p.day.slice(5)),
      datasets: [
        {
          type: "bar",
          label: "queries",
          data: perDay.map((p) => p.queries),
          backgroundColor: css.getPropertyValue("--accent-dim").trim(),
          borderColor: css.getPropertyValue("--accent").trim(),
          borderWidth: 1,
          yAxisID: "y",
        },
        {
          type: "line",
          label: "avg latency ms",
          data: perDay.map((p) => p.avg_latency_ms),
          borderColor: css.getPropertyValue("--amber").trim(),
          backgroundColor: "transparent",
          tension: 0.3,
          pointRadius: 2,
          yAxisID: "y1",
        },
      ],
    },
    options: {
      maintainAspectRatio: false,
      scales: {
        y: { beginAtZero: true, ticks: { precision: 0 } },
        y1: { beginAtZero: true, position: "right", grid: { drawOnChartArea: false } },
      },
      plugins: { legend: { labels: { boxWidth: 10 } } },
    },
  });
}

function drawDocChart(d) {
  const css = getComputedStyle(document.body);
  const entries = ["indexed", "pending", "failed", "deleted"].filter((k) => d[k]);
  const colors = {
    indexed: css.getPropertyValue("--accent").trim(),
    pending: css.getPropertyValue("--amber").trim(),
    failed: css.getPropertyValue("--red").trim(),
    deleted: css.getPropertyValue("--faint").trim(),
  };
  chart($("#ch-d"), {
    type: "doughnut",
    data: {
      labels: entries,
      datasets: [
        {
          data: entries.map((k) => d[k]),
          backgroundColor: entries.map((k) => colors[k]),
          borderColor: css.getPropertyValue("--panel").trim(),
          borderWidth: 2,
        },
      ],
    },
    options: {
      maintainAspectRatio: false,
      cutout: "62%",
      plugins: { legend: { position: "right", labels: { boxWidth: 10 } } },
    },
  });
}

const recentQueriesTable = (rows) =>
  !rows.length
    ? `<div class="empty">no queries yet</div>`
    : `<table><thead><tr><th>query</th><th>via</th><th class="num">hits</th><th class="num">ms</th><th>when</th></tr></thead>
  <tbody>${rows
    .map(
      (r) => `<tr><td class="truncate" title="${esc(r.query)}">${esc(r.query)}</td>
      <td><span class="badge">${esc(r.auth_method)}</span></td>
      <td class="num">${r.hits}</td><td class="num">${r.latency_ms}</td>
      <td class="dim tiny nowrap">${fmtDate(r.at)}</td></tr>`
    )
    .join("")}</tbody></table>`;

const topQueriesTable = (rows) =>
  !rows.length
    ? `<div class="empty">no data</div>`
    : `<table><thead><tr><th>query</th><th class="num">count</th></tr></thead>
  <tbody>${rows
    .map(
      (r) =>
        `<tr><td class="truncate" title="${esc(r.query)}">${esc(r.query)}</td><td class="num">${r.count}</td></tr>`
    )
    .join("")}</tbody></table>`;

const perCollectionTable = (rows) =>
  !rows.length
    ? `<div class="empty">no collections</div>`
    : `<table><thead><tr><th>name</th><th>visibility</th><th class="num">documents</th><th class="num">failed</th></tr></thead>
  <tbody>${rows
    .map(
      (r) => `<tr class="rowlink" onclick="location.hash='#/collections/${r.id}'">
      <td>${esc(r.name)}</td><td>${visBadge(r.visibility)}</td>
      <td class="num">${r.documents}</td><td class="num">${r.failed || ""}</td></tr>`
    )
    .join("")}</tbody></table>`;

const syncRunsTable = (rows) =>
  !rows.length
    ? `<div class="empty">no sync runs yet</div>`
    : `<table><thead><tr><th>collection</th><th>trigger</th><th>status</th><th>stats</th><th>started</th></tr></thead>
  <tbody>${rows
    .map((r) => {
      const stats = Object.entries(r.stats || {})
        .map(([src, s]) =>
          s.error
            ? `${esc(src)}: error`
            : `${esc(src)}: +${s.added || 0} ~${s.updated || 0} −${s.deleted || 0} =${s.unchanged || 0}`
        )
        .join(" · ");
      return `<tr><td>${esc(r.collection)}</td><td class="dim">${esc(r.trigger)}</td>
      <td>${statusBadge(r.status)}</td><td class="mono tiny dim">${stats || "—"}</td>
      <td class="dim tiny nowrap">${fmtDate(r.started_at)}</td></tr>`;
    })
    .join("")}</tbody></table>`;

/* ------------------------------------------------------------------ search */
async function searchPage(main) {
  // The searchable set mirrors exactly what /search will query: collections
  // the caller can read (direct grants + public). Use it for both the filter
  // checkboxes and to resolve collection ids to names in the results.
  const cols = await api("/search/collections");
  const colName = Object.fromEntries(cols.map((c) => [c.id, c.name]));

  main.innerHTML = `
  <div class="page-head">
    <div><div class="crumb">02 · query</div><h1>Search</h1></div>
    <span class="badge badge-green">${cols.length} collection${cols.length === 1 ? "" : "s"} searchable</span>
  </div>
  <div class="grid two-col">
    <div>
      <div class="card">
        <label class="f"><span>query</span>
          <textarea id="s-q" rows="3" placeholder="ask the index in natural language…"></textarea></label>
        <div class="row section-gap">
          <label class="f inline"><span>mode</span>
            <select id="s-mode">
              <option value="hybrid">hybrid — vector + keyword</option>
              <option value="vector">vector — semantic only</option>
            </select></label>
          <label class="f inline"><span>top k</span>
            <input id="s-topk" type="number" min="1" max="50" value="8"></label>
          <button id="s-go" class="btn btn-accent">Search</button>
        </div>
        <div id="s-status" class="dim tiny section-gap"></div>
      </div>
    </div>
    <div class="card">
      <div class="row spread"><h2>collections to search</h2>
        <div class="row"><button id="s-all" class="btn btn-sm btn-ghost">All</button>
        <button id="s-none" class="btn btn-sm btn-ghost">None</button></div></div>
      <div id="s-cols" class="check-grid">${
        !cols.length
          ? `<div class="empty">no collections you can search</div>`
          : cols
              .map(
                (c) => `<label class="check"><input type="checkbox" value="${c.id}" checked>
            <span>${esc(c.name)} ${visBadge(c.visibility)}</span></label>`
              )
              .join("")
      }</div>
      <p class="dim tiny section-gap">No boxes ticked searches every collection above.</p>
    </div>
  </div>
  <div id="s-out" class="section-gap"></div>`;

  const checks = () => [...main.querySelectorAll("#s-cols input")];
  $("#s-all").onclick = () => checks().forEach((c) => (c.checked = true));
  $("#s-none").onclick = () => checks().forEach((c) => (c.checked = false));
  $("#s-go").onclick = () => runSearch(colName);
  // Ctrl/Cmd+Enter from the textarea fires the search.
  $("#s-q").addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) runSearch(colName);
  });
  $("#s-q").focus();
}

async function runSearch(colName) {
  const query = $("#s-q").value.trim();
  const out = $("#s-out");
  const status = $("#s-status");
  if (!query) {
    status.textContent = "enter a query";
    return;
  }
  // Ticked collections; none ticked = all readable (matches API semantics).
  const picked = [...document.querySelectorAll("#s-cols input:checked")].map((i) => i.value);
  const mode = $("#s-mode").value;
  const top_k = Math.max(1, Math.min(50, Number($("#s-topk").value) || 8));

  status.textContent = "searching…";
  out.innerHTML = `<div class="empty">searching…</div>`;
  const started = performance.now();
  try {
    const res = await api("/search", {
      method: "POST",
      body: { query, top_k, mode, collection_ids: picked },
    });
    const ms = Math.round(performance.now() - started);
    const scope = res.searched_collection_ids
      .map((id) => colName[id] || id)
      .join(", ");
    status.innerHTML = `${res.hits.length} hit${res.hits.length === 1 ? "" : "s"} · ${ms} ms · ${mode} · searched ${esc(scope || "—")}`;
    out.innerHTML = res.hits.length
      ? `<div class="card">${res.hits.map((h, i) => searchHitHtml(h, i, colName)).join("")}</div>`
      : `<div class="card"><div class="empty">no hits — try broadening the query or selecting more collections</div></div>`;
    hydrateFigures(out);
  } catch (e) {
    status.textContent = "";
    out.innerHTML = `<div class="card"><div class="empty">error · ${esc(e.message)}</div></div>`;
  }
}

function searchHitHtml(h, i, colName) {
  const figs = figureImgs(h.metadata?.figures);
  const badges = chunkMetaBadges(h.metadata);
  return `<div class="hit">
    <div class="row spread">
      <div><span class="hit-rank mono">${i + 1}</span><strong>${esc(h.title || "untitled")}</strong></div>
      <span class="badge badge-green" title="relevance score">score ${h.score.toFixed(4)}</span>
    </div>
    ${h.uri ? `<div class="dim tiny truncate" title="${esc(h.uri)}">${esc(h.uri)}</div>` : ""}
    <dl class="kv tiny section-gap">
      <dt>collection</dt><dd>${esc(colName[h.collection_id] || h.collection_id)}</dd>
      <dt>chunk #</dt><dd class="mono">${h.ordinal}</dd>
      <dt>chunk id</dt><dd class="mono tiny">${esc(h.chunk_id)}</dd>
      <dt>document id</dt><dd class="mono tiny">${esc(h.document_id)}</dd>
      <dt>collection id</dt><dd class="mono tiny">${esc(h.collection_id)}</dd>
    </dl>
    ${badges ? `<div class="section-gap">${badges}</div>` : ""}
    <div class="hit-text tiny mono section-gap">${esc(h.text)}</div>
    ${figs}
    <details class="section-gap"><summary class="dim tiny">raw hit (json)</summary>
      <pre class="mono tiny">${esc(JSON.stringify(h, null, 2))}</pre></details>
  </div>`;
}

/* -------------------------------------------------------------------- chat */
async function chatPage(main) {
  if (!S.cfg.chat_enabled) {
    main.innerHTML = `
    <div class="page-head"><div><div class="crumb">03 · converse</div><h1>Chat</h1></div></div>
    <div class="card"><div class="empty">chat is not enabled on this server — set RAG_CHAT__* to configure a model</div></div>`;
    return;
  }
  const cols = await api("/search/collections");

  main.innerHTML = `
  <div class="page-head">
    <div><div class="crumb">03 · converse</div><h1>Chat</h1></div>
    <span class="badge badge-green">grounded in ${cols.length} collection${cols.length === 1 ? "" : "s"}</span>
  </div>
  <div class="grid two-col">
    <div class="card chat-card">
      <div id="chat-log" class="chat-log"></div>
      <div class="composer section-gap">
        <textarea id="chat-q" rows="2" placeholder="ask about your indexed documents… (Enter to send · Shift+Enter for newline)"></textarea>
        <button id="chat-send" class="btn btn-accent">Send</button>
      </div>
    </div>
    <div class="card">
      <div class="row spread"><h2>collections to search</h2>
        <div class="row"><button id="chat-all" class="btn btn-sm btn-ghost">All</button>
        <button id="chat-none" class="btn btn-sm btn-ghost">None</button></div></div>
      <div id="chat-cols" class="check-grid">${
        !cols.length
          ? `<div class="empty">no collections you can search</div>`
          : cols
              .map(
                (c) => `<label class="check"><input type="checkbox" value="${c.id}" checked>
            <span>${esc(c.name)} ${visBadge(c.visibility)}</span></label>`
              )
              .join("")
      }</div>
      <p class="dim tiny section-gap">Answers cite the retrieved snippets. The model
        remembers the last ${S.cfg.chat_history_turns || 3} turns of this conversation.</p>
      <button id="chat-clear" class="btn btn-sm section-gap">Clear conversation</button>
    </div>
  </div>`;

  renderChatLog();
  hydrateFigures($("#chat-log"));

  const checks = () => [...main.querySelectorAll("#chat-cols input")];
  $("#chat-all").onclick = () => checks().forEach((c) => (c.checked = true));
  $("#chat-none").onclick = () => checks().forEach((c) => (c.checked = false));
  $("#chat-clear").onclick = () => {
    S.chat = [];
    renderChatLog();
  };
  $("#chat-send").onclick = sendChat;
  $("#chat-q").addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      sendChat();
    }
  });
  $("#chat-q").focus();
}

function renderChatLog() {
  const log = $("#chat-log");
  if (!log) return;
  log.innerHTML = S.chat.length
    ? S.chat.map(chatBubbleHtml).join("")
    : `<div class="empty">ask a question to start — answers draw on your indexed collections</div>`;
  log.scrollTop = log.scrollHeight;
}

function chatBubbleHtml(m) {
  if (m.role === "user") return `<div class="bubble bubble-user">${esc(m.content)}</div>`;
  const cites = m.hits && m.hits.length ? chatCitations(m.hits) : "";
  return `<div class="bubble bubble-bot${m.error ? " bubble-err" : ""}">${esc(m.content)}${cites}</div>`;
}

function chatCitations(hits) {
  return `<details class="cites"><summary class="dim tiny">${hits.length} source${hits.length === 1 ? "" : "s"}</summary>
    ${hits
      .map(
        (h, i) => `<div class="cite">
      <div><span class="mono tiny dim">[${i + 1}]</span> <strong>${esc(h.title || "untitled")}</strong>
        <span class="dim tiny">· score ${h.score.toFixed(3)}</span></div>
      ${h.uri ? `<div class="dim tiny truncate" title="${esc(h.uri)}">${esc(h.uri)}</div>` : ""}
      ${figureImgs(h.metadata?.figures)}</div>`
      )
      .join("")}</details>`;
}

async function sendChat() {
  const ta = $("#chat-q");
  const text = ta.value.trim();
  if (!text) return;
  const picked = [...document.querySelectorAll("#chat-cols input:checked")].map((i) => i.value);
  // Last few turns as memory; drop error replies so they don't poison context.
  const history = S.chat
    .filter((m) => !m.error)
    .slice(-((S.cfg.chat_history_turns || 3) * 2))
    .map((m) => ({ role: m.role, content: m.content }));

  S.chat.push({ role: "user", content: text });
  ta.value = "";
  renderChatLog();
  const log = $("#chat-log");
  log.insertAdjacentHTML(
    "beforeend",
    `<div class="bubble bubble-bot bubble-pending">thinking…</div>`
  );
  log.scrollTop = log.scrollHeight;

  const sendBtn = $("#chat-send");
  sendBtn.disabled = true;
  try {
    const res = await api("/chat", {
      method: "POST",
      body: { query: text, history, collection_ids: picked },
    });
    S.chat.push({ role: "assistant", content: res.answer, hits: res.hits });
  } catch (e) {
    S.chat.push({ role: "assistant", content: `⚠ ${e.message}`, error: true });
  } finally {
    sendBtn.disabled = false;
    renderChatLog();
    hydrateFigures($("#chat-log"));
    $("#chat-q")?.focus();
  }
}

/* ------------------------------------------------------------ collections */
async function collectionsPage(main) {
  const cols = await api("/collections");
  main.innerHTML = `
  <div class="page-head">
    <div><div class="crumb">04 · corpus</div><h1>Collections</h1></div>
    <button id="new-col" class="btn btn-accent">New collection</button>
  </div>
  <div class="card">${
    !cols.length
      ? `<div class="empty">no collections visible to you — create one</div>`
      : `<table><thead><tr><th>name</th><th>visibility</th><th>your role</th><th>sync</th><th>next sync</th></tr></thead>
    <tbody>${cols
      .map(
        (c) => `<tr class="rowlink" onclick="location.hash='#/collections/${c.id}'">
        <td><strong>${esc(c.name)}</strong><div class="dim tiny">${esc(c.description || "")}</div></td>
        <td>${visBadge(c.visibility)}</td><td>${roleBadge(c.my_role)}</td>
        <td class="mono tiny">${c.sync_enabled ? esc(c.sync_cron) : "off"}</td>
        <td class="dim tiny nowrap">${fmtDate(c.next_sync_at)}</td></tr>`
      )
      .join("")}</tbody></table>`
  }</div>`;
  $("#new-col").onclick = newCollectionModal;
}

async function newCollectionModal() {
  const plugins = await api("/plugins");
  modal(`<h3>New collection</h3>
  <label class="f"><span>name</span><input id="nc-name" type="text" placeholder="engineering-handbook"></label>
  <label class="f"><span>description</span><input id="nc-desc" type="text"></label>
  <label class="f"><span>visibility</span><select id="nc-vis">
    <option value="private">private — only people you grant access</option>
    <option value="public">public — readable by everyone</option></select></label>
  <label class="f"><span>sync schedule (cron)</span><input id="nc-cron" type="text" value="0 2 * * *"></label>
  ${pipelineFieldsHtml(plugins)}
  <div class="row spread"><button class="btn btn-ghost" onclick="closeModal()">Cancel</button>
  <button id="nc-go" class="btn btn-accent">Create</button></div>`);
  wirePipelineFields(plugins);
  $("#nc-go").onclick = async () => {
    try {
      const c = await api("/collections", {
        method: "POST",
        body: {
          name: $("#nc-name").value.trim(),
          description: $("#nc-desc").value.trim(),
          visibility: $("#nc-vis").value,
          sync_cron: $("#nc-cron").value.trim() || "0 2 * * *",
          pipeline: readPipelineFields(),
        },
      });
      closeModal();
      toast(`collection ${c.name} created`);
      location.hash = `#/collections/${c.id}`;
    } catch (e) {
      toast(e.message, true);
    }
  };
}

async function collectionDetail(main, id) {
  const [col, sources, runs] = await Promise.all([
    api(`/collections/${id}`),
    api(`/collections/${id}/sources`),
    api(`/collections/${id}/syncs?limit=8`),
  ]);
  const canEdit = ["owner", "editor"].includes(col.my_role);
  const isOwner = col.my_role === "owner";

  main.innerHTML = `
  <div class="page-head">
    <div><div class="crumb"><a href="#/collections">04 · collections</a> / detail</div>
    <h1>${esc(col.name)} ${visBadge(col.visibility)} ${roleBadge(col.my_role)}</h1></div>
    <div class="row">${
      canEdit ? `<button id="sync-now" class="btn btn-accent">Sync now</button>` : ""
    }${isOwner ? `<button id="col-del" class="btn btn-danger">Delete</button>` : ""}</div>
  </div>
  <div class="grid two-col">
    <div>
      <div class="card"><div class="row spread"><h2>sources</h2>${
        canEdit ? `<button id="add-src" class="btn btn-sm">Add source</button>` : ""
      }</div><div id="src-list"></div></div>
      <div class="card section-gap"><h2>recent sync runs</h2><div id="sync-runs">${syncRunsTable(
        runs.map((r) => ({ ...r, collection: col.name }))
      )}</div></div>
    </div>
    <div>
      <div class="card"><h2>settings</h2>
        <dl class="kv">
          <dt>description</dt><dd>${esc(col.description || "—")}</dd>
          <dt>cron</dt><dd class="mono">${esc(col.sync_cron)} ${col.sync_enabled ? "" : "(disabled)"}${
            isOwner ? ` <a href="#" id="col-cron-edit" class="tiny">edit</a>` : ""
          }</dd>
          <dt>next sync</dt><dd>${fmtDate(col.next_sync_at)}</dd>
          <dt>parser</dt><dd class="mono">${esc(col.pipeline?.parser?.name || "auto")}</dd>
          <dt>vision LLM</dt><dd class="mono">${
            col.pipeline?.vision_llm
              ? `${esc(col.pipeline.vision_llm.name)} · ${esc(col.pipeline.vision_llm.config?.model || "")}`
              : "off"
          }</dd>
          <dt>id</dt><dd class="mono tiny">${col.id}</dd>
        </dl>
        <div class="row section-gap">
          ${isOwner ? `<button id="col-pipe" class="btn btn-sm">Edit pipeline</button>` : ""}
          ${isOwner ? `<button id="col-sync-toggle" class="btn btn-sm">${col.sync_enabled ? "Disable sync" : "Enable sync"}</button>` : ""}
          ${isOwner ? `<button id="col-vis" class="btn btn-sm">Make ${col.visibility === "public" ? "private" : "public"}</button>` : ""}
        </div>
      </div>
      ${isOwner ? `<div class="card section-gap"><div class="row spread"><h2>permissions</h2><button id="acl-add" class="btn btn-sm">Add grant</button></div><div id="acl-list"></div></div>` : ""}
    </div>
  </div>
  <div class="card section-gap">
    <div class="row spread"><h2>documents</h2>
      <div class="row">
        <select id="doc-status">
          <option value="">all statuses</option>
          <option value="indexed">indexed</option>
          <option value="pending">pending</option>
          <option value="failed">failed</option>
          <option value="deleted">deleted</option>
        </select>
        <input id="doc-q" type="search" placeholder="filter by title or uri…">
        <button id="doc-refresh" class="btn btn-sm btn-ghost" title="Reload documents">↻</button>
      </div>
    </div>
    <div id="doc-list"></div>
  </div>`;

  const docs = renderDocuments(col, sources);
  renderSources(col, sources, canEdit);
  if (isOwner) renderAcl(col);

  // Re-render the sync-runs panel in place; returns the runs for liveness checks.
  const reloadRuns = async () => {
    try {
      const r = await api(`/collections/${id}/syncs?limit=8`);
      const box = $("#sync-runs");
      if (box) box.innerHTML = syncRunsTable(r.map((x) => ({ ...x, collection: col.name })));
      return r;
    } catch {
      return [];
    }
  };

  // Live updates: poll docs + runs while a sync is running or docs are still
  // pending, so indexing status fills in without a manual reload. Self-stops
  // when everything settles, on navigation away, or after a 2-minute cap.
  let polling = false;
  const myHash = location.hash;
  const setSyncBusy = (busy) => {
    const b = $("#sync-now");
    if (b) {
      b.disabled = busy;
      b.textContent = busy ? "Syncing…" : "Sync now";
    }
  };
  const startPolling = async () => {
    if (polling) return;
    polling = true;
    setSyncBusy(true);
    const deadline = Date.now() + 120000;
    let delay = 1500;
    try {
      while (location.hash === myHash && document.getElementById("doc-list")) {
        await sleep(delay);
        if (location.hash !== myHash || !document.getElementById("doc-list")) return;
        let runs = [];
        let pending = 0;
        try {
          const [r, p] = await Promise.all([
            reloadRuns(),
            api(`/collections/${id}/documents?status=pending&limit=1`),
          ]);
          runs = r;
          pending = p.total;
        } catch {
          /* transient; keep polling */
        }
        await docs.reload();
        const running = runs.some((r) => r.status === "running");
        if ((!running && pending === 0) || Date.now() > deadline) return;
        delay = Math.min(delay * 1.3, 5000);
      }
    } finally {
      polling = false;
      setSyncBusy(false);
    }
  };

  $("#doc-refresh").onclick = () => {
    docs.reload();
    reloadRuns();
  };

  // Landed on a collection that's already syncing/indexing -> go live.
  if (runs.some((r) => r.status === "running")) {
    startPolling();
  } else {
    api(`/collections/${id}/documents?status=pending&limit=1`)
      .then((p) => p.total > 0 && startPolling())
      .catch(() => {});
  }

  if (canEdit) {
    $("#sync-now").onclick = async () => {
      try {
        await api(`/collections/${id}/syncs`, { method: "POST" });
        toast("sync queued");
        startPolling();
      } catch (e) {
        toast(e.message, true);
      }
    };
  }
  if (isOwner) {
    $("#col-pipe").onclick = () => editPipelineModal(col);
    $("#col-cron-edit").onclick = (e) => {
      e.preventDefault();
      editCronModal(col);
    };
    $("#col-sync-toggle").onclick = async () => {
      try {
        await api(`/collections/${id}`, {
          method: "PATCH",
          body: { sync_enabled: !col.sync_enabled },
        });
        toast(`scheduled sync ${col.sync_enabled ? "disabled" : "enabled"}`);
        route();
      } catch (e) {
        toast(e.message, true);
      }
    };
    $("#col-vis").onclick = async () => {
      const next = col.visibility === "public" ? "private" : "public";
      await api(`/collections/${id}`, { method: "PATCH", body: { visibility: next } });
      route();
    };
    $("#col-del").onclick = () => {
      modal(`<h3>Delete collection?</h3><p class="dim">This removes ${esc(col.name)}, its sources, documents and index. Irreversible.</p>
      <div class="row spread"><button class="btn btn-ghost" onclick="closeModal()">Cancel</button>
      <button id="del-go" class="btn btn-danger">Delete permanently</button></div>`);
      $("#del-go").onclick = async () => {
        await api(`/collections/${id}`, { method: "DELETE" });
        closeModal();
        toast("collection deleted");
        location.hash = "#/collections";
      };
    };
  }
}

/* ---------------------------------------------------------- index inspector */
const DOC_PAGE = 50;

function renderDocuments(col, sources) {
  const srcName = Object.fromEntries(sources.map((s) => [s.id, s.name]));
  const state = { status: "", q: "", offset: 0 };
  const el = $("#doc-list");

  const load = async (quiet = false) => {
    // quiet = in-place refresh (live polling): skip the loading flicker.
    if (!quiet) el.innerHTML = `<div class="empty">loading…</div>`;
    const params = new URLSearchParams({ limit: DOC_PAGE, offset: state.offset });
    if (state.status) params.set("status", state.status);
    if (state.q) params.set("q", state.q);
    let data;
    try {
      data = await api(`/collections/${col.id}/documents?${params}`);
    } catch (e) {
      el.innerHTML = `<div class="empty">error · ${esc(e.message)}</div>`;
      return;
    }
    if (!data.items.length) {
      el.innerHTML = `<div class="empty">no documents${state.q || state.status ? " match" : " indexed yet — add a source and sync"}</div>`;
      return;
    }
    const pages = Math.ceil(data.total / DOC_PAGE);
    const page = state.offset / DOC_PAGE + 1;
    el.innerHTML = `
    <table><thead><tr><th>title</th><th>source</th><th>status</th>
      <th class="num">chunks</th><th class="num">size</th><th>indexed</th></tr></thead>
    <tbody>${data.items
      .map(
        (d) => `<tr class="rowlink" data-doc="${d.id}">
        <td class="truncate" title="${esc(d.uri)}"><strong>${esc(d.title || d.external_id)}</strong></td>
        <td class="dim tiny">${esc(srcName[d.source_id] || "—")}</td>
        <td>${statusBadge(d.status)}</td>
        <td class="num">${d.chunk_count}</td>
        <td class="num dim tiny">${fmtBytes(d.size_bytes)}</td>
        <td class="dim tiny nowrap">${fmtDate(d.indexed_at)}</td></tr>`
      )
      .join("")}</tbody></table>
    <div class="row spread section-gap">
      <span class="dim tiny">${data.total} document${data.total === 1 ? "" : "s"}${pages > 1 ? ` · page ${page}/${pages}` : ""}</span>
      <div class="row">
        ${page > 1 ? `<button id="doc-prev" class="btn btn-sm btn-ghost">‹ Prev</button>` : ""}
        ${page < pages ? `<button id="doc-next" class="btn btn-sm btn-ghost">Next ›</button>` : ""}
      </div>
    </div>`;
    el.querySelectorAll("[data-doc]").forEach((row) => {
      row.onclick = () => chunkInspectorModal(col, row.dataset.doc, srcName);
    });
    const prev = $("#doc-prev"), next = $("#doc-next");
    if (prev) prev.onclick = () => ((state.offset -= DOC_PAGE), load());
    if (next) next.onclick = () => ((state.offset += DOC_PAGE), load());
  };

  $("#doc-status").onchange = () => ((state.status = $("#doc-status").value), (state.offset = 0), load());
  let debounce;
  $("#doc-q").oninput = () => {
    clearTimeout(debounce);
    debounce = setTimeout(() => ((state.q = $("#doc-q").value.trim()), (state.offset = 0), load()), 300);
  };
  load();
  return { reload: () => load(true) };
}

const chunkMetaBadges = (m) =>
  Object.entries(m || {})
    .filter(([k]) => k !== "figures") // rendered as thumbnails, not a badge
    .map(([k, v]) => `<span class="badge" title="${esc(k)}">${esc(k === "heading_path" ? v : `${k}: ${JSON.stringify(v)}`)}</span>`)
    .join(" ");

async function chunkInspectorModal(col, docId, srcName) {
  const CHUNK_PAGE = 20;
  const doc = await api(`/collections/${col.id}/documents/${docId}`);
  modal(`<h3 class="truncate" title="${esc(doc.uri)}">${esc(doc.title || doc.external_id)}</h3>
  <div class="row">${statusBadge(doc.status)}
    <span class="badge">${esc(srcName[doc.source_id] || "source")}</span>
    <span class="badge">${esc(doc.mime_type)}</span>
    <span class="dim tiny">${doc.chunk_count} chunks · ${fmtBytes(doc.size_bytes)} · indexed ${fmtDate(doc.indexed_at)}</span></div>
  ${doc.error ? `<div class="hit"><span class="bad">error</span> <span class="mono tiny">${esc(doc.error)}</span></div>` : ""}
  ${
    Object.keys(doc.doc_metadata || {}).length
      ? `<details class="section-gap"><summary class="dim tiny">document metadata</summary>
       <pre class="mono tiny">${esc(JSON.stringify(doc.doc_metadata, null, 2))}</pre></details>`
      : ""
  }
  <div id="chunk-list" class="section-gap"></div>
  <div class="row spread section-gap">
    <span id="chunk-count" class="dim tiny"></span>
    <div class="row"><button id="chunk-more" class="btn btn-sm" hidden>Load more</button>
    <button class="btn btn-ghost btn-sm" onclick="closeModal()">Close</button></div>
  </div>`);

  let offset = 0;
  const listEl = $("#chunk-list");
  const loadChunks = async () => {
    const data = await api(
      `/collections/${col.id}/documents/${docId}/chunks?limit=${CHUNK_PAGE}&offset=${offset}`
    );
    listEl.insertAdjacentHTML(
      "beforeend",
      data.items
        .map(
          (c) => `<div class="hit">
        <div class="hit-meta">#${c.ordinal} · ${c.token_count} tokens
          ${c.has_embedding ? "" : ` · <span class="bad">no embedding</span>`}
          ${chunkMetaBadges(c.chunk_metadata)}</div>
        <div class="hit-text tiny mono" style="white-space:pre-wrap">${esc(c.text)}</div>
        ${figureImgs(c.chunk_metadata?.figures)}</div>`
        )
        .join("") || `<div class="empty">no chunks — document not indexed</div>`
    );
    hydrateFigures(listEl);
    offset += data.items.length;
    $("#chunk-count").textContent = `${offset} of ${data.total} chunks`;
    $("#chunk-more").hidden = offset >= data.total;
  };
  $("#chunk-more").onclick = loadChunks;
  await loadChunks();
}

async function editPipelineModal(col) {
  const plugins = await api("/plugins");
  modal(`<h3>Indexing pipeline</h3>
  <p class="dim tiny">Applies to every source in this collection. Changes affect documents indexed from now on; re-upload or change a document to re-index it.</p>
  ${pipelineFieldsHtml(plugins, col.pipeline || {})}
  <div class="row spread section-gap"><button class="btn btn-ghost" onclick="closeModal()">Cancel</button>
  <button id="pf-go" class="btn btn-accent">Save</button></div>`);
  wirePipelineFields(plugins);
  $("#pf-go").onclick = async () => {
    try {
      await api(`/collections/${col.id}`, {
        method: "PATCH",
        body: { pipeline: readPipelineFields(col.pipeline || {}) },
      });
      closeModal();
      toast("pipeline updated");
      route();
    } catch (e) {
      toast(e.message, true);
    }
  };
}

const CRON_PRESETS = [
  ["*/5 * * * *", "every 5 minutes"],
  ["*/15 * * * *", "every 15 minutes"],
  ["0 * * * *", "hourly"],
  ["0 2 * * *", "daily at 2am"],
  ["0 2 * * 1", "weekly (Mon 2am)"],
];

function editCronModal(col) {
  modal(`<h3>Sync schedule</h3>
  <p class="dim tiny">Standard 5-field cron (minute hour day month weekday), evaluated in UTC.</p>
  <label class="f"><span>cron expression</span><input id="cron-val" type="text" value="${esc(col.sync_cron)}"></label>
  <div class="row" style="flex-wrap:wrap;gap:6px">${CRON_PRESETS.map(
    ([expr, label]) => `<button type="button" class="btn btn-sm btn-ghost" data-preset="${esc(expr)}">${esc(label)}</button>`
  ).join("")}</div>
  <div id="cron-err" class="tiny" style="color:var(--red)"></div>
  <div class="row spread section-gap"><button class="btn btn-ghost" onclick="closeModal()">Cancel</button>
  <button id="cron-go" class="btn btn-accent">Save</button></div>`);
  document.querySelectorAll("[data-preset]").forEach((b) => {
    b.onclick = () => {
      $("#cron-val").value = b.dataset.preset;
    };
  });
  $("#cron-go").onclick = async () => {
    try {
      await api(`/collections/${col.id}`, {
        method: "PATCH",
        body: { sync_cron: $("#cron-val").value.trim() },
      });
      closeModal();
      toast("sync schedule updated");
      route();
    } catch (e) {
      $("#cron-err").textContent = e.message;
    }
  };
}

function renderSources(col, sources, canEdit) {
  const el = $("#src-list");
  el.innerHTML = !sources.length
    ? `<div class="empty">no sources onboarded</div>`
    : sources
        .map(
          (s) => `<div class="hit">
      <div class="row spread"><div><strong>${esc(s.name)}</strong>
        <span class="badge badge-blue">${esc(s.connector)}</span>
        ${s.enabled ? "" : `<span class="badge">disabled</span>`}</div>
        <div class="row">${
          s.connector === "upload" && canEdit
            ? `<button class="btn btn-sm" data-upload="${s.id}">Upload files</button>`
            : ""
        }${canEdit ? `<button class="btn btn-sm btn-danger" data-del-src="${s.id}">Remove</button>` : ""}</div></div>
      <div class="dim tiny">last synced ${fmtDate(s.last_synced_at)}</div></div>`
        )
        .join("");

  el.querySelectorAll("[data-del-src]").forEach((b) => {
    b.onclick = async () => {
      await api(`/collections/${col.id}/sources/${b.dataset.delSrc}`, { method: "DELETE" });
      route();
    };
  });
  el.querySelectorAll("[data-upload]").forEach((b) => {
    b.onclick = () => uploadModal(col.id, b.dataset.upload);
  });
  if (canEdit) $("#add-src").onclick = () => addSourceModal(col.id);
}

async function addSourceModal(colId) {
  const plugins = await api("/plugins");
  const conns = plugins.connector;
  modal(`<h3>Add source</h3>
  <label class="f"><span>name</span><input id="as-name" type="text" placeholder="team-wiki"></label>
  <label class="f"><span>connector</span><select id="as-conn">${conns
    .map((c) => `<option value="${esc(c.name)}" ${c.name === "upload" ? "selected" : ""}>${esc(c.name)}</option>`)
    .join("")}</select></label>
  <div id="as-body"></div>
  <div id="as-result" class="tiny mono"></div>
  <div class="row spread section-gap"><button class="btn btn-ghost" onclick="closeModal()">Cancel</button>
  <div class="row"><button id="as-test" class="btn btn-ghost">Test</button>
  <button id="as-go" class="btn btn-accent">Add</button></div></div>`);

  const current = () => conns.find((c) => c.name === $("#as-conn").value);
  const setResult = (txt, ok) => {
    const el = $("#as-result");
    el.textContent = txt ? (ok ? "✓ " : "✗ ") + txt : "";
    el.style.color = txt ? (ok ? "var(--accent)" : "var(--red)") : "";
  };
  const setEnabled = (on) => {
    $("#as-test").disabled = !on;
    $("#as-go").disabled = !on;
  };

  const renderBody = () => {
    const c = current();
    setResult("");
    if (c.supports_url_targets) {
      if (!c.ready) {
        $("#as-body").innerHTML =
          `<div class="empty">${esc(c.name)} isn't configured on this server. Ask an admin to set up the connection.</div>`;
        setEnabled(false);
        return;
      }
      setEnabled(true);
      $("#as-body").innerHTML = `
      <label class="f"><span>link (paste a Confluence page, space, or folder URL)</span>
      <input id="as-link" type="text" placeholder="https://org.atlassian.net/wiki/spaces/ENG/pages/12345/Title"></label>
      <label class="row" style="gap:6px;align-items:center"><input id="as-kids" type="checkbox"><span>include child pages</span></label>`;
    } else {
      setEnabled(true);
      const props = Object.keys(c?.config_schema?.properties || {});
      $("#as-body").innerHTML = `
      <label class="f"><span>config (json)</span><textarea id="as-cfg">{}</textarea></label>
      <div class="dim tiny mono">${props.length ? `fields: ${props.join(", ")}` : "no config needed"}</div>`;
    }
  };
  $("#as-conn").onchange = renderBody;
  renderBody();

  const payload = () => {
    const c = current();
    if (c.supports_url_targets) {
      return {
        name: $("#as-name").value.trim(),
        connector: c.name,
        link: ($("#as-link")?.value || "").trim(),
        include_children: !!$("#as-kids")?.checked,
      };
    }
    return {
      name: $("#as-name").value.trim(),
      connector: c.name,
      config: JSON.parse($("#as-cfg")?.value || "{}"),
    };
  };

  $("#as-test").onclick = async () => {
    setResult("testing…", true);
    try {
      const p = payload();
      const r = await api(`/collections/${colId}/sources/test`, {
        method: "POST",
        body: {
          connector: p.connector,
          config: p.config || {},
          link: p.link || null,
          include_children: !!p.include_children,
        },
      });
      setResult(r.detail, r.ok);
    } catch (e) {
      setResult(e.message, false);
    }
  };

  $("#as-go").onclick = async () => {
    try {
      await api(`/collections/${colId}/sources`, { method: "POST", body: payload() });
      closeModal();
      toast("source added");
      route();
    } catch (e) {
      toast(e.message, true);
    }
  };
}

async function uploadModal(colId, srcId) {
  const files = await api(`/collections/${colId}/sources/${srcId}/files`);
  modal(`<h3>Manual uploads</h3>
  <label class="f"><span>add file (pdf · docx · pptx · txt · md · csv · xlsx)</span>
  <input id="up-file" type="file" accept=".pdf,.docx,.pptx,.txt,.md,.markdown,.csv,.xlsx" multiple></label>
  <div id="up-list">${
    !files.length
      ? `<div class="empty">no files</div>`
      : files
          .map(
            (f) => `<div class="row spread hit"><div>${esc(f.filename)}
        <span class="dim tiny">${fmtBytes(f.size_bytes)}</span></div>
        <button class="btn btn-sm btn-danger" data-rm="${f.id}">Remove</button></div>`
          )
          .join("")
  }</div>
  <div class="row spread section-gap"><span class="dim tiny">files index on next sync</span>
  <button class="btn btn-ghost" onclick="closeModal()">Close</button></div>`);
  $("#up-file").onchange = async (e) => {
    for (const file of e.target.files) {
      const fd = new FormData();
      fd.append("file", file);
      try {
        await api(`/collections/${colId}/sources/${srcId}/files`, { method: "POST", body: fd });
        toast(`${file.name} uploaded`);
      } catch (err) {
        toast(`${file.name}: ${err.message}`, true);
      }
    }
    uploadModal(colId, srcId);
  };
  document.querySelectorAll("[data-rm]").forEach((b) => {
    b.onclick = async () => {
      await api(`/collections/${colId}/sources/${srcId}/files/${b.dataset.rm}`, {
        method: "DELETE",
      });
      uploadModal(colId, srcId);
    };
  });
}

async function renderAcl(col) {
  const entries = await api(`/collections/${col.id}/permissions`);
  const el = $("#acl-list");
  el.innerHTML = entries
    .map(
      (e, i) => `<div class="row spread hit"><div>
      <span class="badge">${esc(e.principal_type)}</span>
      <span class="mono tiny">${esc(e.principal_id)}</span> ${roleBadge(e.role)}</div>
      <button class="btn btn-sm btn-danger" data-acl-rm="${i}">Remove</button></div>`
    )
    .join("");
  const save = async (next) => {
    try {
      await api(`/collections/${col.id}/permissions`, { method: "PUT", body: next });
      renderAcl(col);
    } catch (e) {
      toast(e.message, true);
    }
  };
  el.querySelectorAll("[data-acl-rm]").forEach((b) => {
    b.onclick = () => save(entries.filter((_, i) => i !== Number(b.dataset.aclRm)));
  });
  $("#acl-add").onclick = () => {
    modal(`<h3>Grant access</h3>
    <label class="f"><span>principal type</span><select id="g-type"><option>user</option><option>group</option></select></label>
    <label class="f"><span>entra object id</span><input id="g-id" type="text" placeholder="00000000-…"></label>
    <label class="f"><span>role</span><select id="g-role"><option>viewer</option><option>editor</option><option>owner</option></select></label>
    <div class="row spread"><button class="btn btn-ghost" onclick="closeModal()">Cancel</button>
    <button id="g-go" class="btn btn-accent">Grant</button></div>`);
    $("#g-go").onclick = async () => {
      await save([
        ...entries,
        {
          principal_type: $("#g-type").value,
          principal_id: $("#g-id").value.trim(),
          role: $("#g-role").value,
        },
      ]);
      closeModal();
    };
  };
}

/* --------------------------------------------------------------- api keys */
async function keysPage(main) {
  const [keys, cols] = await Promise.all([api("/api-keys"), api("/collections")]);
  main.innerHTML = `
  <div class="page-head">
    <div><div class="crumb">05 · access</div><h1>API keys</h1></div>
  </div>
  <div class="grid two-col">
    <div class="card"><h2>your keys</h2>${keysTable(keys)}</div>
    <div class="card"><h2>create key</h2>
      <label class="f"><span>name</span><input id="k-name" type="text" placeholder="ci-search-bot"></label>
      <label class="f"><span>expires in days (blank = never)</span><input id="k-exp" type="number" min="1" max="730"></label>
      <label class="f"><span>scopes — collections this key may search</span></label>
      <div id="k-scopes">${
        !cols.length
          ? `<div class="empty">no collections available to you</div>`
          : cols
              .map(
                (c) => `<label class="check"><input type="checkbox" value="${c.id}">
          ${esc(c.name)} ${visBadge(c.visibility)} ${roleBadge(c.my_role)}</label>`
              )
              .join("")
      }</div>
      <div class="row section-gap"><button id="k-go" class="btn btn-accent">Create key</button></div>
      <p class="dim tiny section-gap">Keys authenticate with <span class="mono">Authorization: Bearer ragk_…</span> and can only call <span class="mono">/search</span>, restricted to the scoped collections.</p>
    </div>
  </div>`;

  $("#k-go").onclick = async () => {
    const scopes = [...document.querySelectorAll("#k-scopes input:checked")].map((i) => i.value);
    const exp = $("#k-exp").value;
    try {
      const created = await api("/api-keys", {
        method: "POST",
        body: {
          name: $("#k-name").value.trim(),
          collection_ids: scopes,
          expires_in_days: exp ? Number(exp) : null,
        },
      });
      modal(`<h3>Key created — copy it now</h3>
      <p class="dim tiny">This secret is shown once and cannot be retrieved again.</p>
      <div class="secret-box" id="secret">${esc(created.secret)}</div>
      <div class="row spread section-gap">
        <button id="copy-key" class="btn">Copy to clipboard</button>
        <button class="btn btn-accent" onclick="closeModal();location.reload()">Done</button></div>`);
      $("#copy-key").onclick = () => {
        navigator.clipboard.writeText(created.secret);
        toast("copied");
      };
    } catch (e) {
      toast(e.message, true);
    }
  };
  document.querySelectorAll("[data-revoke]").forEach((b) => {
    b.onclick = async () => {
      await api(`/api-keys/${b.dataset.revoke}`, { method: "DELETE" });
      toast("key revoked");
      route();
    };
  });
}

const keysTable = (keys) =>
  !keys.length
    ? `<div class="empty">no keys yet</div>`
    : `<table><thead><tr><th>name</th><th>key</th><th class="num">scopes</th><th>last used</th><th>expires</th><th></th></tr></thead>
  <tbody>${keys
    .map(
      (k) => `<tr>
    <td><strong>${esc(k.name)}</strong>${k.revoked ? ` <span class="badge badge-red">revoked</span>` : ""}</td>
    <td class="mono tiny">${esc(k.prefix)}…</td>
    <td class="num">${k.scopes.length}</td>
    <td class="dim tiny nowrap">${fmtDate(k.last_used_at)}</td>
    <td class="dim tiny nowrap">${k.expires_at ? fmtDay(k.expires_at) : "never"}</td>
    <td>${k.revoked ? "" : `<button class="btn btn-sm btn-danger" data-revoke="${k.id}">Revoke</button>`}</td></tr>`
    )
    .join("")}</tbody></table>`;

/* ------------------------------------------------------------------- docs */
const _snippets = {};

function codeCard(key, title, code) {
  _snippets[key] = code;
  return `<div class="card section-gap"><h2>${esc(title)}</h2>
    <div class="codeblock"><button class="btn btn-sm btn-ghost copy-btn" data-copy="${key}">Copy</button>
    <pre>${esc(code)}</pre></div></div>`;
}

async function docsPage(main) {
  const cols = await api("/collections").catch(() => []);
  const origin = location.origin;

  const curlSnippet = [
    `curl -X POST ${origin}/search \\`,
    `  -H "Authorization: Bearer $RAG_API_KEY" \\`,
    `  -H "Content-Type: application/json" \\`,
    `  -d '{`,
    `    "query": "how do I request vacation?",`,
    `    "top_k": 5,`,
    `    "mode": "hybrid"`,
    `  }'`,
  ].join("\n");

  const pySnippet = [
    `import os`,
    `import requests`,
    ``,
    `RAG_URL = "${origin}"`,
    `API_KEY = os.environ["RAG_API_KEY"]  # ragk_... from the API keys page`,
    ``,
    `def search(query: str, top_k: int = 5, collection_ids: list[str] | None = None):`,
    `    resp = requests.post(`,
    `        f"{RAG_URL}/search",`,
    `        headers={"Authorization": f"Bearer {API_KEY}"},`,
    `        json={`,
    `            "query": query,`,
    `            "top_k": top_k,`,
    `            "mode": "hybrid",  # or "vector"`,
    `            # omit / empty = every collection the key is scoped to`,
    `            "collection_ids": collection_ids or [],`,
    `        },`,
    `        timeout=30,`,
    `    )`,
    `    resp.raise_for_status()`,
    `    return resp.json()["hits"]`,
    ``,
    `for hit in search("how do I request vacation?"):`,
    `    print(f"{hit['score']:.3f}  {hit['title']}  ({hit['uri']})")`,
    `    print(hit["text"][:300], "\\n")`,
  ].join("\n");

  const jsSnippet = [
    `const RAG_URL = "${origin}";`,
    `const API_KEY = process.env.RAG_API_KEY; // ragk_... from the API keys page`,
    ``,
    `async function search(query, { topK = 5, collectionIds = [] } = {}) {`,
    `  const res = await fetch(\`\${RAG_URL}/search\`, {`,
    `    method: "POST",`,
    `    headers: {`,
    `      "Authorization": \`Bearer \${API_KEY}\`,`,
    `      "Content-Type": "application/json",`,
    `    },`,
    `    body: JSON.stringify({`,
    `      query,`,
    `      top_k: topK,`,
    `      mode: "hybrid", // or "vector"`,
    `      collection_ids: collectionIds, // [] = all collections in key scope`,
    `    }),`,
    `  });`,
    `  if (!res.ok) throw new Error(\`search failed: \${res.status} \${await res.text()}\`);`,
    `  const { hits } = await res.json();`,
    `  return hits;`,
    `}`,
    ``,
    `const hits = await search("how do I request vacation?");`,
    `for (const h of hits) console.log(h.score.toFixed(3), h.title, h.uri);`,
  ].join("\n");

  const mcpSnippet = [
    `# Claude Code`,
    `claude mcp add --transport http rag ${origin}/mcp \\`,
    `  --header "Authorization: Bearer $RAG_API_KEY"`,
    ``,
    `# stdio-only clients: bridge with mcp-remote`,
    `npx mcp-remote ${origin}/mcp \\`,
    `  --header "Authorization:Bearer $RAG_API_KEY"`,
  ].join("\n");

  const chunksSample = [
    `GET ${origin}/documents/{document_id}/chunks?from_ordinal=0&limit=20`,
    ``,
    `{`,
    `  "items": [`,
    `    {`,
    `      "id": "b3f1c2d4-…",`,
    `      "ordinal": 0,`,
    `      "token_count": 213,`,
    `      "text": "[Employee Handbook > Vacation policy]\\nFull-time employees …",`,
    `      "chunk_metadata": { "heading_path": "Employee Handbook > Vacation policy" },`,
    `      "has_embedding": true`,
    `    }`,
    `  ],`,
    `  "total": 32`,
    `}`,
  ].join("\n");

  const responseSample = [
    `{`,
    `  "hits": [`,
    `    {`,
    `      "chunk_id": "a6558181-…",`,
    `      "document_id": "0ed45dc3-…",`,
    `      "collection_id": "3ae2fdee-…",`,
    `      "score": 0.0323,`,
    `      "title": "handbook.md",`,
    `      "uri": "upload://…/handbook.md",`,
    `      "ordinal": 0,`,
    `      "text": "[Employee Handbook > Vacation policy]\\nFull-time employees accrue …",`,
    `      "metadata": {`,
    `        "heading_path": "Employee Handbook > Vacation policy",`,
    `        "figures": [ { "key": "3ae2fdee-…/0ed45dc3-…/figures/49109583-0.png", "page": 2 } ]`,
    `      }`,
    `    }`,
    `  ],`,
    `  "searched_collection_ids": ["3ae2fdee-…"]`,
    `}`,
  ].join("\n");

  main.innerHTML = `
  <div class="page-head">
    <div><div class="crumb">06 · reference</div><h1>Querying the API</h1></div>
    <a class="btn btn-sm" href="/docs" target="_blank" rel="noopener">OpenAPI reference</a>
  </div>
  <div class="card">
    <h2>how it works</h2>
    <p>1 · Mint a key on the <a href="#/keys">API keys</a> page, scoped to the collections it may search. The secret (<span class="mono">ragk_…</span>) is shown once — store it as a secret (env var, vault), never in code.</p>
    <p>2 · Call <span class="mono">POST ${esc(origin)}/search</span> with the key as a bearer token. Keys are read-only: they can search, page document chunks and fetch figures — every management endpoint rejects them.</p>
    <p>3 · Feed the returned chunks to your LLM as context. Each hit carries its source <span class="mono">uri</span> for citations.</p>
    <p>4 · Hits derived from document figures (diagrams, charts) include <span class="mono">metadata.figures[].key</span> — fetch the actual image with <span class="mono">GET /search/figures?key=…</span> using the same bearer token (returns <span class="mono">image/png</span>, access-checked like /search).</p>
    <div class="doc-note">Results are permission-filtered server-side: a key only ever sees its scoped collections, and only while its owner still has access to them. Revoke keys anytime from the API keys page.</div>
  </div>

  <div class="card section-gap"><h2>request fields — POST /search</h2>
  <table><thead><tr><th>field</th><th>type</th><th>notes</th></tr></thead><tbody>
    <tr><td class="mono">query</td><td class="dim">string</td><td>required, 1–4000 chars</td></tr>
    <tr><td class="mono">collection_ids</td><td class="dim">uuid[]</td><td>optional — empty/omitted searches every collection in the key's scope; unknown or out-of-scope ids return 404. Discover ids programmatically via <span class="mono">GET /search/collections</span></td></tr>
    <tr><td class="mono">top_k</td><td class="dim">int</td><td>1–50, default 8</td></tr>
    <tr><td class="mono">mode</td><td class="dim">string</td><td><span class="mono">hybrid</span> (vector + full-text, default) or <span class="mono">vector</span></td></tr>
  </tbody></table>
  <div class="doc-note">About <span class="mono">score</span>: in <span class="mono">hybrid</span> mode it is a reciprocal-rank-fusion value (typically 0.01–0.06) — compare hits within one response, don't read it as a percentage or cosine similarity. <span class="mono">vector</span> mode returns plain similarity. <span class="mono">ordinal</span> is the chunk's position in its document — feed it to the chunks endpoint below to read the surrounding passage.</div>
  ${
    cols.length
      ? `<h2 class="section-gap">your collection ids</h2><table><tbody>${cols
          .map(
            (c) =>
              `<tr><td>${esc(c.name)} ${visBadge(c.visibility)}</td><td class="mono tiny">${c.id}</td></tr>`
          )
          .join("")}</tbody></table>`
      : ""
  }</div>

  ${codeCard("curl", "curl", curlSnippet)}
  ${codeCard("python", "python", pySnippet)}
  ${codeCard("js", "javascript / node 18+", jsSnippet)}
  ${codeCard("resp", "response shape", responseSample)}

  <div class="card section-gap"><h2>reading whole documents — GET /documents/{id}/chunks</h2>
  <p>Search hits are short excerpts. To read a document's actual content (summarize it, read the passage around a hit), page its chunks in order using the <span class="mono">document_id</span> from any hit.</p>
  <table><thead><tr><th>param</th><th>type</th><th>notes</th></tr></thead><tbody>
    <tr><td class="mono">from_ordinal</td><td class="dim">int</td><td>start position, default 0 (negatives clamp to 0)</td></tr>
    <tr><td class="mono">limit</td><td class="dim">int</td><td>chunks per page, default 20, max 100</td></tr>
  </tbody></table>
  <div class="doc-note">Whole document: start at <span class="mono">from_ordinal=0</span> and advance by the number of chunks returned until you have all <span class="mono">total</span>. Context around a hit: pass <span class="mono">from_ordinal = hit.ordinal − 2</span>. Access is identical to /search (404 outside the key's scope).</div>
  </div>
  ${codeCard("chunks", "chunks response shape", chunksSample)}

  <div class="card section-gap"><h2>discovering collections — GET /search/collections</h2>
  <p>Returns the collections the key can search, for building <span class="mono">collection_ids</span> filters programmatically. Each entry: <span class="mono">{ id, name, description, visibility }</span>.</p>
  </div>

  <div class="card section-gap"><h2>mcp</h2>
  <p>The API is also an MCP server at <span class="mono">${esc(origin)}/mcp</span> (streamable HTTP) with three tools: <span class="mono">search_documents</span>, <span class="mono">get_document_chunks</span> (read a document's content page by page) and <span class="mono">list_searchable_collections</span> (collection discovery). Auth is the same bearer header — use a scoped API key. Results are permission-filtered exactly like REST.</p>
  </div>
  ${codeCard("mcp", "connect an mcp client", mcpSnippet)}

  <div class="card section-gap"><h2>errors</h2>
  <table><thead><tr><th>status</th><th>meaning</th></tr></thead><tbody>
    <tr><td class="mono">401</td><td>missing, invalid, expired or revoked key</td></tr>
    <tr><td class="mono">403</td><td>key used on a non-search endpoint</td></tr>
    <tr><td class="mono">404</td><td>collection id unknown <span class="dim">or outside the key's scope (indistinguishable by design)</span></td></tr>
    <tr><td class="mono">422</td><td>invalid request body (see response detail)</td></tr>
  </tbody></table></div>`;

  main.querySelectorAll("[data-copy]").forEach((b) => {
    b.onclick = () => {
      navigator.clipboard.writeText(_snippets[b.dataset.copy]);
      toast("copied");
    };
  });
}

/* ------------------------------------------------------------------- init */
window.closeModal = closeModal;
boot().catch((e) => {
  $("#boot .boot-msg").textContent = `failed to start · ${e.message}`;
});
