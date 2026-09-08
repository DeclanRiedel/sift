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
