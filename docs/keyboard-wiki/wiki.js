(function () {
  const SCROLL_KEY_PREFIX = "sift.wiki.scroll.";
  const PAGE_CONTENT_CACHE = new Map();
  const LEGACY_PATHS = {
    "/index.html": "/keyboard",
    "/configuration.html": "/configuration",
    "/hosting.html": "/hosting",
    "/shared-rooms.html": "/shared-rooms",
  };
  const WIKI_ROUTES = new Set(["/", "/keyboard", "/configuration", "/hosting", "/shared-rooms"]);

  const parser = new DOMParser();
  const state = {
    activePath: canonicalizePath(location.pathname),
  };

  function canonicalizePath(path) {
    const normalized = LEGACY_PATHS[path] || (path || "/");
    if (normalized.length > 1 && normalized.endsWith("/")) {
      return normalized.slice(0, -1);
    }
    return normalized;
  }

  function isWikiRoute(path) {
    return WIKI_ROUTES.has(canonicalizePath(path));
  }

  function saveScroll(path) {
    try {
      sessionStorage.setItem(`${SCROLL_KEY_PREFIX}${path}`, String(Math.max(0, Math.round(window.scrollY || 0))));
    } catch {
      // Session storage unavailable in restricted browser contexts.
    }
  }

  function restoreScroll(path) {
    try {
      const saved = sessionStorage.getItem(`${SCROLL_KEY_PREFIX}${path}`);
      const y = parseInt(saved || "", 10);
      if (!Number.isFinite(y) || y <= 0) {
        return;
      }
      const maxY = Math.max(
        0,
        Math.ceil(document.documentElement.scrollHeight - window.innerHeight),
      );
      window.scrollTo(0, Math.min(Math.max(0, y), maxY));
    } catch {
      // Session storage unavailable in restricted browser contexts.
    }
  }

  async function loadRoute(path) {
    const normalized = canonicalizePath(path);
    const cached = PAGE_CONTENT_CACHE.get(normalized);
    if (cached) {
      return cached;
    }

    const response = await fetch(normalized, {
      credentials: "same-origin",
      headers: { "X-Requested-With": "sift-wiki-ajax" },
      cache: "force-cache",
    });
    if (!response.ok) {
      throw new Error(`wiki route failed: ${response.status}`);
    }

    const text = await response.text();
    const doc = parser.parseFromString(text, "text/html");
    const nextMain = doc.querySelector("main");
    if (!nextMain) {
      throw new Error("wiki route returned no main content");
    }

    const payload = {
      title: doc.title || "Sift",
      content: nextMain.innerHTML,
    };
    PAGE_CONTENT_CACHE.set(normalized, payload);
    return payload;
  }

  function replaceMain(payload) {
    const main = document.querySelector("main");
    if (!main) {
      return;
    }

    main.innerHTML = payload.content;
    if (payload.title) {
      document.title = payload.title;
    }
  }

  async function swapRoute(path, options = {}) {
    const normalized = canonicalizePath(path);
    if (state.activePath === normalized && !options.force) {
      return;
    }

    saveScroll(state.activePath);
    const payload = await loadRoute(normalized);

    const previous = state.activePath;
    replaceMain(payload);
    state.activePath = normalized;

    if (options.push && previous !== normalized) {
      window.history.pushState({ path: normalized }, "", normalized);
    } else if (options.replace) {
      window.history.replaceState({ path: normalized }, "", normalized);
    }

    restoreScroll(normalized);
    schedulePrefetch();
  }

  function isTabNavigationAnchor(anchor) {
    if (!anchor || !anchor.getAttribute) {
      return false;
    }

    if (anchor.target && anchor.target.toLowerCase() !== "_self") {
      return false;
    }

    const href = anchor.getAttribute("href");
    if (!href || href.startsWith("#") || href.startsWith("javascript:")) {
      return false;
    }

    let url;
    try {
      url = new URL(href, location.href);
    } catch {
      return false;
    }

    if (url.origin !== location.origin) {
      return false;
    }

    const targetPath = canonicalizePath(url.pathname);
    if (!isWikiRoute(targetPath)) {
      return false;
    }

    if (url.hash && url.pathname === location.pathname) {
      return false;
    }

    return true;
  }

  async function onDocumentClick(event) {
    const target = event.target;
    const anchor = target && target.closest ? target.closest("a") : null;
    if (!isTabNavigationAnchor(anchor)) {
      return;
    }

    event.preventDefault();
    event.stopPropagation();
    const targetPath = canonicalizePath(new URL(anchor.href, location.href).pathname);
    if (targetPath === state.activePath) {
      return;
    }

    await swapRoute(targetPath, { push: true }).catch(() => {
      window.location.href = anchor.href;
    });
  }

  function preload(path) {
    if (PAGE_CONTENT_CACHE.has(path)) {
      return;
    }

    void loadRoute(path).catch(() => {
      PAGE_CONTENT_CACHE.delete(path);
    });
  }

  function enqueuePrefetch() {
    const links = Array.from(document.querySelectorAll(".doc-tabs a[href]"));
    for (const link of links) {
      const url = new URL(link.getAttribute("href"), location.href);
      const path = canonicalizePath(url.pathname);
      if (!isWikiRoute(path) || PAGE_CONTENT_CACHE.has(path) || path === state.activePath) {
        continue;
      }
      preload(path);
    }
  }

  function schedulePrefetch() {
    const run = () => enqueuePrefetch();
    if (typeof window.requestIdleCallback === "function") {
      window.requestIdleCallback(run, { timeout: 1200 });
    } else {
      setTimeout(run, 300);
    }
  }

  function onPopState() {
    const targetPath = canonicalizePath(location.pathname);
    if (targetPath === state.activePath) {
      return;
    }
    void swapRoute(targetPath, { replace: true, force: true }).catch(() => {
      window.location.reload();
    });
  }

  async function init() {
    const currentPath = canonicalizePath(location.pathname);
    if (currentPath !== location.pathname) {
      window.history.replaceState({ path: currentPath }, "", currentPath);
    }

    state.activePath = currentPath;
    restoreScroll(currentPath);
    document.addEventListener("click", onDocumentClick);
    window.addEventListener("popstate", onPopState);
    window.addEventListener("beforeunload", () => saveScroll(state.activePath));
    window.addEventListener("pagehide", () => saveScroll(state.activePath));

    schedulePrefetch();
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", () => {
      void init();
    }, { once: true });
  } else {
    void init();
  }
})();
