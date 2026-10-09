"use strict";

// This is a return-to-app page, not an OAuth exchange endpoint. Never display,
// persist, or forward callback codes, state, provider descriptions, or tokens.
const denied = new URLSearchParams(window.location.search).has("error");
if (window.location.search || window.location.hash) {
  window.history.replaceState(null, "", window.location.pathname);
}
if (denied) {
  document.title = "Sift · GitHub sign-in not completed";
  document.getElementById("authorization-title").textContent = "GitHub sign-in wasn't completed";
  document.getElementById("authorization-message").textContent = "Return to Sift and try again when you're ready.";
  document.getElementById("authorization-symbol").setAttribute("d", "m6 6 12 12 M18 6 6 18");
  document.querySelector(".authorization-mark").classList.add("authorization-denied");
}
