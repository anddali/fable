/*
 * Mockup helper for docs/design/*.html (not production code).
 * - theme: ?theme=light|dark, else localStorage, else prefers-color-scheme (UX-1)
 * - icons: <i data-icon="name"></i> → inline SVG
 * - shell: <body data-nav="search" data-crumbs="Workspace|Search"> wraps
 *   <main class="page"> in the app shell (sidebar, top bar, breadcrumbs, Cmd+K).
 *   Admin nav appears only when data-admin="true" (default true in mockups).
 */
(function () {
  "use strict";

  var params = new URLSearchParams(location.search);
  var stored = null;
  try { stored = localStorage.getItem("fable-theme"); } catch (e) { /* file:// */ }
  var theme = params.get("theme") || stored ||
    (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
  document.documentElement.dataset.theme = theme;

  var P = {
    home: '<path d="M3 10.5 12 3l9 7.5V20a1 1 0 0 1-1 1h-5v-6H9v6H4a1 1 0 0 1-1-1z"/>',
    search: '<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>',
    chat: '<path d="M21 12a8 8 0 0 1-11.6 7.1L4 20l1-4.6A8 8 0 1 1 21 12z"/>',
    folder: '<path d="M3 6.5A1.5 1.5 0 0 1 4.5 5H9l2 2.5h8.5A1.5 1.5 0 0 1 21 9v9.5a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5z"/>',
    key: '<circle cx="8" cy="15" r="4"/><path d="m11 12 9-9m-4 4 3 3m-5-1 2 2"/>',
    book: '<path d="M4 4.5A1.5 1.5 0 0 1 5.5 3H20v15H5.5A1.5 1.5 0 0 0 4 19.5zM4 19.5A1.5 1.5 0 0 0 5.5 21H20"/>',
    gauge: '<path d="M12 14 16 9"/><path d="M3.5 18a9 9 0 1 1 17 0z"/>',
    plug: '<path d="M9 3v5m6-5v5M6 8h12v3a6 6 0 0 1-12 0zM12 17v4"/>',
    cpu: '<rect x="6" y="6" width="12" height="12" rx="2"/><path d="M9 2v3m6-3v3M9 19v3m6-3v3M2 9h3m-3 6h3m14-6h3m-3 6h3M10 10h4v4h-4z"/>',
    sliders: '<path d="M4 6h10m4 0h2M4 12h4m4 0h8M4 18h12m4 0h0"/><circle cx="16" cy="6" r="2"/><circle cx="10" cy="12" r="2"/><circle cx="18" cy="18" r="2"/>',
    layers: '<path d="m12 3 9 5-9 5-9-5z"/><path d="m3 13 9 5 9-5"/>',
    gaugeLimit: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 3"/>',
    shield: '<path d="M12 3 4 6v6c0 5 3.5 8 8 9 4.5-1 8-4 8-9V6z"/>',
    list: '<path d="M8 6h13M8 12h13M8 18h13M3.5 6h.01M3.5 12h.01M3.5 18h.01"/>',
    scroll: '<path d="M8 3h11a2 2 0 0 1 2 2v2h-4M8 3a2 2 0 0 0-2 2v14a2 2 0 0 1-2 2h11a2 2 0 0 0 2-2V5a2 2 0 0 0-2-2M10 9h4m-4 4h4"/>',
    check: '<path d="m4.5 12.5 5 5 10-11"/>',
    x: '<path d="M6 6l12 12M18 6 6 18"/>',
    alert: '<path d="M12 3 2 20h20z"/><path d="M12 10v4m0 3h.01"/>',
    info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v5m0-8h.01"/>',
    error: '<circle cx="12" cy="12" r="9"/><path d="m9 9 6 6m0-6-6 6"/>',
    okc: '<circle cx="12" cy="12" r="9"/><path d="m8 12.5 3 3 5-6"/>',
    right: '<path d="m9 5 7 7-7 7"/>',
    down: '<path d="m5 9 7 7 7-7"/>',
    up: '<path d="m5 15 7-7 7 7"/>',
    left: '<path d="m15 5-7 7 7 7"/>',
    sortdown: '<path d="M12 5v14m-5-5 5 5 5-5"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
    refresh: '<path d="M20 11a8 8 0 0 0-14.3-4.9L4 8m0-4v4h4M4 13a8 8 0 0 0 14.3 4.9L20 16m0 4v-4h-4"/>',
    thumbup: '<path d="M7 11v9H4v-9zM7 11l4-8a2 2 0 0 1 2.5 2.2L13 9h5.5a2 2 0 0 1 2 2.4l-1.4 7A2 2 0 0 1 17 20H7"/>',
    thumbdown: '<path d="M7 13V4H4v9zM7 13l4 8a2 2 0 0 0 2.5-2.2L13 15h5.5a2 2 0 0 0 2-2.4l-1.4-7A2 2 0 0 0 17 4H7"/>',
    link: '<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>',
    sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M2 12h2m16 0h2M4.9 4.9l1.4 1.4m11.4 11.4 1.4 1.4M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>',
    moon: '<path d="M20 14.5A8 8 0 0 1 9.5 4 8 8 0 1 0 20 14.5z"/>',
    filter: '<path d="M3 5h18l-7 8v6l-4 2v-8z"/>',
    more: '<circle cx="5" cy="12" r="1.3"/><circle cx="12" cy="12" r="1.3"/><circle cx="19" cy="12" r="1.3"/>',
    star: '<path d="m12 3 2.8 5.7 6.2.9-4.5 4.4 1 6.2L12 17.3 6.5 20.2l1-6.2L3 9.6l6.2-.9z"/>',
    play: '<path d="M7 4v16l13-8z"/>',
    external: '<path d="M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"/>',
    clock: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
    lock: '<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V8a4 4 0 0 1 8 0v3"/>',
    sparkle: '<path d="M12 3v4m0 10v4M3 12h4m10 0h4M6 6l2.5 2.5m7 7L18 18M6 18l2.5-2.5m7-7L18 6"/>',
    file: '<path d="M6 3h8l5 5v12a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z"/><path d="M14 3v5h5"/>',
    page: '<path d="M6 3h12a1 1 0 0 1 1 1v16a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1zM8.5 8h7m-7 4h7m-7 4h4"/>',
    tree: '<path d="M5 4v12a2 2 0 0 0 2 2h4M5 9h6"/><rect x="11" y="6" width="8" height="6" rx="1"/><rect x="11" y="15" width="8" height="6" rx="1"/>',
    copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V5a1 1 0 0 0-1-1H5a1 1 0 0 0-1 1v10a1 1 0 0 0 1 1h3"/>',
    send: '<path d="M4 12 20 4l-6 16-3-7z"/>',
    trash: '<path d="M4 7h16M9 7V4h6v3m-9 0 1 13h10l1-13"/>',
    edit: '<path d="M4 20h4L19 9l-4-4L4 16z"/>',
    eye: '<path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z"/><circle cx="12" cy="12" r="3"/>',
    target: '<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="5"/><circle cx="12" cy="12" r="1"/>',
    zap: '<path d="M13 2 4 14h7l-1 8 9-12h-7z"/>',
    users: '<circle cx="9" cy="8" r="3.5"/><path d="M2.5 20a6.5 6.5 0 0 1 13 0M16 4.5a3.5 3.5 0 0 1 0 7M18 14a6 6 0 0 1 3.5 6"/>',
    globe: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3c3 3.5 3 14.5 0 18M12 3c-3 3.5-3 14.5 0 18"/>',
    upload: '<path d="M12 16V4m-5 5 5-5 5 5M4 16v3a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-3"/>',
    beaker: '<path d="M9 3h6M10 3v6L4.5 19a1.3 1.3 0 0 0 1.1 2h12.8a1.3 1.3 0 0 0 1.1-2L14 9V3M7 15h10"/>',
    message: '<path d="M4 5h16v11H9l-5 4z"/>',
    rocket: '<path d="M5 15c-1.5 1.5-2 5-2 5s3.5-.5 5-2M9 15l-3-3c1-4 4.5-8 12-9-1 7.5-5 11-9 12z"/><circle cx="14.5" cy="9.5" r="1.5"/>'
  };

  function svg(name, cls) {
    var body = P[name] || P.info;
    return '<svg class="icon ' + (cls || "") + '" viewBox="0 0 24 24" fill="none" stroke="currentColor" ' +
      'stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">' +
      body + "</svg>";
  }
  window.fableIcon = svg;

  function hydrateIcons(root) {
    root.querySelectorAll("i[data-icon]").forEach(function (el) {
      var cls = el.className || "";
      el.outerHTML = svg(el.dataset.icon, cls);
    });
  }

  var NAV = [
    { section: "Workspace" },
    { id: "home", label: "Home", icon: "home", href: "#" },
    { id: "search", label: "Search", icon: "search", href: "search.html" },
    { id: "chat", label: "Chat", icon: "chat", href: "chat.html" },
    { id: "collections", label: "Collections", icon: "folder", href: "collections.html", count: "1,284" },
    { id: "keys", label: "API keys", icon: "key", href: "#" },
    { id: "docs", label: "Docs", icon: "book", href: "#" },
    { section: "Admin", admin: true },
    { id: "a-overview", label: "Overview", icon: "gauge", href: "#", admin: true },
    { id: "a-integrations", label: "Integrations", icon: "plug", href: "admin-integrations.html", admin: true },
    { id: "a-providers", label: "Model providers", icon: "cpu", href: "admin-model-providers.html", admin: true },
    { id: "a-roles", label: "Model roles", icon: "sliders", href: "admin-model-roles.html", admin: true },
    { id: "a-indexing", label: "Indexing defaults", icon: "layers", href: "#", admin: true },
    { id: "a-limits", label: "Limits", icon: "gaugeLimit", href: "#", admin: true },
    { id: "a-collections", label: "Collections", icon: "folder", href: "#", admin: true },
    { id: "a-access", label: "Access", icon: "shield", href: "#", admin: true },
    { id: "a-jobs", label: "Jobs", icon: "list", href: "#", admin: true },
    { id: "a-audit", label: "Audit log", icon: "scroll", href: "#", admin: true }
  ];

  function buildShell() {
    var body = document.body;
    var main = document.querySelector("main.page");
    if (!main || body.dataset.shell === "none") return;
    var admin = body.dataset.admin !== "false";
    var current = body.dataset.nav || "";
    var crumbs = (body.dataset.crumbs || "").split("|").filter(Boolean);

    var nav = NAV.filter(function (n) { return admin || !n.admin; }).map(function (n) {
      if (n.section) return '<div class="nav-section">' + n.section + "</div>";
      return '<a href="' + n.href + '"' + (n.id === current ? ' aria-current="page"' : "") + ">" +
        svg(n.icon) + "<span>" + n.label + "</span>" +
        (n.count ? '<span class="count faint num">' + n.count + "</span>" : "") + "</a>";
    }).join("");

    var crumbHtml = crumbs.map(function (c, i) {
      var last = i === crumbs.length - 1;
      return (i ? svg("right") : "") + (last ? '<span aria-current="page">' + c + "</span>" : '<a href="#">' + c + "</a>");
    }).join("");

    var app = document.createElement("div");
    app.className = "app";
    app.innerHTML =
      '<aside class="sidebar" aria-label="Main navigation">' +
        '<div class="brand"><span class="brand-mark" aria-hidden="true">F</span><span class="name">Fable</span>' +
        '<span class="env pill pill-outline">prod</span></div>' +
        '<nav class="nav">' + nav + "</nav>" +
        '<div class="sidebar-foot xs muted">v2.0.0 · <a href="#">What\'s new</a></div>' +
      "</aside>" +
      '<div class="main">' +
        (body.dataset.banner ? '<div class="banner" role="status">' + svg("alert") + "<span>" + body.dataset.banner + "</span></div>" : "") +
        '<header class="topbar">' +
          '<nav class="crumbs" aria-label="Breadcrumb">' + crumbHtml + "</nav>" +
          '<div class="spacer"></div>' +
          '<button class="cmdk" type="button" aria-label="Search or jump to (Ctrl+K)">' + svg("search") +
            "<span>Search or jump to…</span><span class=\"kbd\">Ctrl K</span></button>" +
          '<button class="btn btn-ghost btn-icon" type="button" id="theme-toggle" aria-label="Switch to ' +
            (theme === "dark" ? "light" : "dark") + ' theme">' + svg(theme === "dark" ? "sun" : "moon") + "</button>" +
          '<div class="user"><span class="avatar" aria-hidden="true">DK</span><span><strong>Dana Kowalski</strong><br>' +
            '<span class="xs muted">' + (admin ? "Admin" : "User") + "</span></span></div>" +
        "</header>" +
      "</div>";
    body.insertBefore(app, body.firstChild);
    app.querySelector(".main").appendChild(main);
  }

  function wireThemeToggle() {
    var b = document.getElementById("theme-toggle");
    if (!b) return;
    b.addEventListener("click", function () {
      var next = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
      try { localStorage.setItem("fable-theme", next); } catch (e) { /* ignore */ }
      var u = new URL(location.href);
      u.searchParams.set("theme", next);
      location.href = u.toString();
    });
  }

  document.addEventListener("DOMContentLoaded", function () {
    buildShell();
    hydrateIcons(document);
    wireThemeToggle();
  });
})();
