(() => {
  const routes = new Set(["/", "/keyboard", "/configuration", "/hosting", "/shared-rooms"]);
  const legacy = { "/index.html": "/keyboard", "/configuration.html": "/configuration", "/hosting.html": "/hosting", "/shared-rooms.html": "/shared-rooms" };
  const pages = new Map();
  const pending = new Map();
  const positions = new Map();
  const canonical = path => legacy[path] || path;
  let active = canonical(location.pathname);
  let navigation = 0;
  let content;
  let tabs;
  let copyStatus;

  function copyButton(target, label) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "copy-button";
    button.title = label;
    button.setAttribute("aria-label", label);
    button.innerHTML = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true"><rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V4H4v12h4"/></svg>';
    button.copyTarget = target;
    return button;
  }

  function enhanceContent(root, path) {
    for (const pre of root.querySelectorAll("pre")) {
      if (pre.parentElement?.classList.contains("copy-block")) continue;
      const block = document.createElement("div");
      block.className = "copy-block";
      pre.replaceWith(block);
      block.append(pre, copyButton(pre.querySelector("code") || pre, "Copy code block"));
    }
    if (path !== "/configuration") return;
    for (const cell of root.querySelectorAll("table th, table td:first-child")) {
      if (!cell.querySelector("code") || cell.classList.contains("copy-field")) continue;
      cell.classList.add("copy-field");
      cell.append(copyButton(cell, `Copy ${cell.textContent.trim()}`));
    }
  }

  async function copyCode(event) {
    const button = event.target.closest?.(".copy-button");
    if (!button || button.disabled) return;
    const target = button.copyTarget;
    button.disabled = true;
    try {
      await navigator.clipboard.writeText(target.textContent);
      copyStatus.textContent = "Copied to clipboard.";
      button.dataset.copied = "true";
      clearTimeout(button.copyTimer);
      button.copyTimer = setTimeout(() => delete button.dataset.copied, 1600);
    } catch {
      const range = document.createRange();
      range.selectNodeContents(target);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      copyStatus.textContent = "Clipboard access unavailable. Text selected; press Ctrl+C or Command+C to copy.";
    } finally {
      button.disabled = false;
    }
  }

  function saveScroll() {
    positions.set(active, window.scrollY);
    try { sessionStorage.setItem(`sift.wiki.scroll.${active}`, String(window.scrollY)); } catch {}
  }

  function restoreScroll(url) {
    if (url.hash) {
      let id;
      try { id = decodeURIComponent(url.hash.slice(1)); } catch { id = url.hash.slice(1); }
      const target = document.getElementById(id);
      if (target) { target.scrollIntoView(); return; }
    }
    let y = positions.get(active);
    if (y === undefined) {
      try { y = Number(sessionStorage.getItem(`sift.wiki.scroll.${active}`)); } catch {}
    }
    window.scrollTo(0, Number.isFinite(y) ? y : 0);
  }

  function load(path) {
    if (pages.has(path)) return Promise.resolve(pages.get(path));
    if (pending.has(path)) return pending.get(path);
    const request = fetch(`/fragments/${path === "/" ? "overview" : path.slice(1)}`)
      .then(async response => {
        if (!response.ok || !response.headers.has("X-Wiki-Title")) throw new Error("Invalid wiki fragment");
        const template = document.createElement("template");
        template.innerHTML = await response.text();
        enhanceContent(template.content, path);
        const page = { title: response.headers.get("X-Wiki-Title"), nodes: template.content };
        pages.set(path, page);
        return page;
      }).finally(() => pending.delete(path));
    pending.set(path, request);
    return request;
  }

  function updateTabs() {
    for (const link of tabs.querySelectorAll("a")) {
      const selected = canonical(new URL(link.href).pathname) === active;
      link.classList.toggle("active", selected);
      if (selected) link.setAttribute("aria-current", "page");
      else link.removeAttribute("aria-current");
    }
  }

  async function navigate(url, push) {
    const ticket = ++navigation;
    const path = canonical(url.pathname);
    try {
      const page = await load(path);
      if (ticket !== navigation) return;
      saveScroll();
      if (path !== active) {
        const previous = pages.get(active);
        previous.nodes.append(...content.childNodes);
        content.append(page.nodes);
        active = path;
        document.title = page.title;
        updateTabs();
      }
      if (push) history.pushState(null, "", url);
      restoreScroll(url);
    } catch {
      if (ticket === navigation) location.assign(url.href);
    }
  }

  function localLink(event) {
    const link = event.target.closest?.("a[href]");
    if (!link || link.hasAttribute("download") || (link.target && link.target !== "_self")) return;
    const url = new URL(link.href);
    if (url.origin !== location.origin || url.search || !routes.has(canonical(url.pathname))) return;
    return url;
  }

  function init() {
    const main = document.querySelector("main");
    tabs = main?.querySelector(".doc-tabs");
    if (!tabs || !routes.has(active)) return;
    content = document.createElement("div");
    content.id = "wiki-content";
    while (tabs.nextSibling) content.append(tabs.nextSibling);
    main.append(content);
    enhanceContent(content, active);
    copyStatus = document.createElement("p");
    copyStatus.className = "visually-hidden";
    copyStatus.setAttribute("role", "status");
    document.body.append(copyStatus);
    document.addEventListener("click", copyCode);
    pages.set(active, { title: document.title, nodes: document.createDocumentFragment() });
    history.scrollRestoration = "manual";
    updateTabs();
    // Keep hash targets visible beneath the header, including when tabs wrap.
    const sizeHeader = () => document.documentElement.style.setProperty("--tabs-height", `${tabs.offsetHeight}px`);
    sizeHeader();
    new ResizeObserver(sizeHeader).observe(tabs);
    restoreScroll(new URL(location.href));

    document.addEventListener("click", event => {
      if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      const url = localLink(event);
      if (!url) return;
      if (canonical(url.pathname) === active && url.hash) {
        ++navigation;
        return; // Native same-page anchors and history.
      }
      event.preventDefault();
      if (canonical(url.pathname) === active && !url.hash) { ++navigation; return; }
      void navigate(url, true);
    });
    window.addEventListener("popstate", () => { void navigate(new URL(location.href), false); });
    window.addEventListener("pagehide", saveScroll);

    const connection = navigator.connection;
    if (connection?.saveData || /(^|-)2g$/.test(connection?.effectiveType || "")) return;
    const prefetch = event => {
      const url = localLink(event);
      if (url) void load(canonical(url.pathname)).catch(() => {});
    };
    tabs.addEventListener("pointerover", prefetch);
    tabs.addEventListener("focusin", prefetch);
    if (connection && connection.effectiveType !== "4g") return;
    const warm = async () => {
      for (const path of routes) {
        if (document.hidden) break;
        try { await load(path); } catch {}
      }
    };
    // Let the initial page and its assets finish before warming other tabs.
    const schedule = () => {
      if ("requestIdleCallback" in window) requestIdleCallback(warm);
      else setTimeout(warm, 300);
    };
    if (document.readyState === "complete") schedule();
    else window.addEventListener("load", schedule, { once: true });
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init, { once: true });
  else init();
})();
