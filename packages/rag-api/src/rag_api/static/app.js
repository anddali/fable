"use strict";

/* ------------------------------------------------------------------ state */
const S = { cfg: null, msal: null, account: null, me: null, charts: [] };
const $ = (sel, el = document) => el.querySelector(sel);
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
      : { dashboard, collections: collectionsPage, keys: keysPage, docs: docsPage }[page] ||
        dashboard;
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
      <h2 class="section-gap">try a query</h2>
      <div class="row"><input id="try-q" type="search" placeholder="search the index…">
      <button id="try-btn" class="btn btn-accent btn-sm">Run</button></div>
      <div id="try-out"></div>
    </div>
  </div>
  <div class="card section-gap"><h2>collections</h2>${perCollectionTable(db.per_collection)}</div>
  <div class="card section-gap"><h2>recent sync runs</h2>${syncRunsTable(db.sync_runs)}</div>`;

  drawQueryChart(q.per_day);
  drawDocChart(d);

  $("#try-btn").onclick = runTryQuery;
  $("#try-q").addEventListener("keydown", (e) => e.key === "Enter" && runTryQuery());
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

async function runTryQuery() {
  const qEl = $("#try-q");
  const out = $("#try-out");
  if (!qEl.value.trim()) return;
  out.innerHTML = `<div class="empty">searching…</div>`;
  try {
    const res = await api("/search", {
      method: "POST",
      body: { query: qEl.value.trim(), top_k: 3, mode: "hybrid" },
    });
    out.innerHTML = res.hits.length
      ? res.hits
          .map(
            (h) => `<div class="hit">
        <div class="hit-meta">${esc(h.title)} · score ${h.score.toFixed(4)}</div>
        <div class="hit-text tiny">${esc(h.text.slice(0, 320))}${h.text.length > 320 ? "…" : ""}</div></div>`
          )
          .join("")
      : `<div class="empty">no hits</div>`;
  } catch (e) {
    out.innerHTML = `<div class="empty">error · ${esc(e.message)}</div>`;
  }
}

/* ------------------------------------------------------------ collections */
async function collectionsPage(main) {
  const cols = await api("/collections");
  main.innerHTML = `
  <div class="page-head">
    <div><div class="crumb">02 · corpus</div><h1>Collections</h1></div>
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

function newCollectionModal() {
  modal(`<h3>New collection</h3>
  <label class="f"><span>name</span><input id="nc-name" type="text" placeholder="engineering-handbook"></label>
  <label class="f"><span>description</span><input id="nc-desc" type="text"></label>
  <label class="f"><span>visibility</span><select id="nc-vis">
    <option value="private">private — only people you grant access</option>
    <option value="public">public — readable by everyone</option></select></label>
  <label class="f"><span>sync schedule (cron)</span><input id="nc-cron" type="text" value="0 2 * * *"></label>
  <div class="row spread"><button class="btn btn-ghost" onclick="closeModal()">Cancel</button>
  <button id="nc-go" class="btn btn-accent">Create</button></div>`);
  $("#nc-go").onclick = async () => {
    try {
      const c = await api("/collections", {
        method: "POST",
        body: {
          name: $("#nc-name").value.trim(),
          description: $("#nc-desc").value.trim(),
          visibility: $("#nc-vis").value,
          sync_cron: $("#nc-cron").value.trim() || "0 2 * * *",
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
    <div><div class="crumb"><a href="#/collections">02 · collections</a> / detail</div>
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
      <div class="card section-gap"><h2>recent sync runs</h2>${syncRunsTable(
        runs.map((r) => ({ ...r, collection: col.name }))
      )}</div>
    </div>
    <div>
      <div class="card"><h2>settings</h2>
        <dl class="kv">
          <dt>description</dt><dd>${esc(col.description || "—")}</dd>
          <dt>cron</dt><dd class="mono">${esc(col.sync_cron)} ${col.sync_enabled ? "" : "(disabled)"}</dd>
          <dt>next sync</dt><dd>${fmtDate(col.next_sync_at)}</dd>
          <dt>id</dt><dd class="mono tiny">${col.id}</dd>
        </dl>
        ${isOwner ? `<div class="row section-gap"><button id="col-vis" class="btn btn-sm">Make ${col.visibility === "public" ? "private" : "public"}</button></div>` : ""}
      </div>
      ${isOwner ? `<div class="card section-gap"><div class="row spread"><h2>permissions</h2><button id="acl-add" class="btn btn-sm">Add grant</button></div><div id="acl-list"></div></div>` : ""}
    </div>
  </div>`;

  renderSources(col, sources, canEdit);
  if (isOwner) renderAcl(col);

  if (canEdit)
    $("#sync-now").onclick = async () => {
      try {
        await api(`/collections/${id}/syncs`, { method: "POST" });
        toast("sync queued");
      } catch (e) {
        toast(e.message, true);
      }
    };
  if (isOwner) {
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
  const connectors = plugins.connector.map((p) => p.name);
  modal(`<h3>Add source</h3>
  <label class="f"><span>name</span><input id="as-name" type="text" placeholder="team-wiki"></label>
  <label class="f"><span>connector</span><select id="as-conn">${connectors
    .map((c) => `<option ${c === "upload" ? "selected" : ""}>${esc(c)}</option>`)
    .join("")}</select></label>
  <label class="f"><span>config (json — see schema hint below)</span><textarea id="as-cfg">{}</textarea></label>
  <div id="as-hint" class="dim tiny mono"></div>
  <div class="row spread section-gap"><button class="btn btn-ghost" onclick="closeModal()">Cancel</button>
  <button id="as-go" class="btn btn-accent">Add</button></div>`);
  const hint = () => {
    const p = plugins.connector.find((x) => x.name === $("#as-conn").value);
    const props = Object.keys(p?.config_schema?.properties || {});
    $("#as-hint").textContent = props.length ? `fields: ${props.join(", ")}` : "no config needed";
  };
  $("#as-conn").onchange = hint;
  hint();
  $("#as-go").onclick = async () => {
    try {
      await api(`/collections/${colId}/sources`, {
        method: "POST",
        body: {
          name: $("#as-name").value.trim(),
          connector: $("#as-conn").value,
          config: JSON.parse($("#as-cfg").value || "{}"),
        },
      });
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
  <label class="f"><span>add file (pdf · txt · md · csv · xlsx)</span>
  <input id="up-file" type="file" accept=".pdf,.txt,.md,.markdown,.csv,.xlsx" multiple></label>
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
    <div><div class="crumb">03 · access</div><h1>API keys</h1></div>
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
    `      "metadata": { "heading_path": "Employee Handbook > Vacation policy" }`,
    `    }`,
    `  ],`,
    `  "searched_collection_ids": ["3ae2fdee-…"]`,
    `}`,
  ].join("\n");

  main.innerHTML = `
  <div class="page-head">
    <div><div class="crumb">04 · reference</div><h1>Querying the API</h1></div>
    <a class="btn btn-sm" href="/docs" target="_blank" rel="noopener">OpenAPI reference</a>
  </div>
  <div class="card">
    <h2>how it works</h2>
    <p>1 · Mint a key on the <a href="#/keys">API keys</a> page, scoped to the collections it may search. The secret (<span class="mono">ragk_…</span>) is shown once — store it as a secret (env var, vault), never in code.</p>
    <p>2 · Call <span class="mono">POST ${esc(origin)}/search</span> with the key as a bearer token. Keys can <em>only</em> search: every management endpoint rejects them.</p>
    <p>3 · Feed the returned chunks to your LLM as context. Each hit carries its source <span class="mono">uri</span> for citations.</p>
    <div class="doc-note">Results are permission-filtered server-side: a key only ever sees its scoped collections, and only while its owner still has access to them. Revoke keys anytime from the API keys page.</div>
  </div>

  <div class="card section-gap"><h2>request fields — POST /search</h2>
  <table><thead><tr><th>field</th><th>type</th><th>notes</th></tr></thead><tbody>
    <tr><td class="mono">query</td><td class="dim">string</td><td>required, 1–4000 chars</td></tr>
    <tr><td class="mono">collection_ids</td><td class="dim">uuid[]</td><td>optional — empty/omitted searches every collection in the key's scope; unknown or out-of-scope ids return 404</td></tr>
    <tr><td class="mono">top_k</td><td class="dim">int</td><td>1–50, default 8</td></tr>
    <tr><td class="mono">mode</td><td class="dim">string</td><td><span class="mono">hybrid</span> (vector + full-text, default) or <span class="mono">vector</span></td></tr>
  </tbody></table>
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
