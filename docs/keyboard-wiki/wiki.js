(function () {
  const SCROLL_Y_KEY = "sift.wiki.scroll.y";
  const TAB_SCROLL_KEY = "sift.wiki.tab.scrollLeft";

  function saveScrollPosition() {
    try {
      sessionStorage.setItem(SCROLL_Y_KEY, String(window.scrollY || 0));
      const tabs = document.querySelector(".doc-tabs");
      if (tabs) {
        sessionStorage.setItem(TAB_SCROLL_KEY, String(tabs.scrollLeft || 0));
      }
    } catch {
      // Session storage can fail in restricted environments.
    }
  }

  function restoreScrollPosition() {
    const savedY = parseInt(sessionStorage.getItem(SCROLL_Y_KEY) || "", 10);
    if (Number.isFinite(savedY) && savedY > 0) {
      const maxY = Math.max(0, document.documentElement.scrollHeight - innerHeight);
      window.scrollTo(0, Math.min(savedY, maxY));
    }

    const tabs = document.querySelector(".doc-tabs");
    if (!tabs) return;

    const savedTabs = parseInt(sessionStorage.getItem(TAB_SCROLL_KEY) || "", 10);
    if (Number.isFinite(savedTabs) && savedTabs > 0) {
      tabs.scrollLeft = Math.max(0, savedTabs);
    }
  }

  window.addEventListener("beforeunload", saveScrollPosition);
  window.addEventListener("pagehide", saveScrollPosition);
  window.addEventListener("pageshow", restoreScrollPosition);

  function setupTabListeners() {
    const docTabs = document.querySelector(".doc-tabs");
    if (!docTabs) {
      return;
    }
    for (const link of docTabs.querySelectorAll("a[href]")) {
      link.addEventListener("click", saveScrollPosition);
    }
    restoreScrollPosition();
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", setupTabListeners, { once: true });
  } else {
    setupTabListeners();
  }
})();
