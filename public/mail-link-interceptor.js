// CrystalMail's trusted mail-viewer bridge.
//
// This file is deliberately loaded as an external script. Tauri adds a
// nonce/hash to the app-shell CSP in production, which correctly blocks
// inline scripts in the sandboxed mail document. Keeping this tiny bridge in
// the application bundle preserves that CSP without making mail HTML trusted.
(function () {
  document.addEventListener(
    "click",
    function (event) {
      var element = event.target;
      while (element && element.nodeName !== "A") {
        element = element.parentNode;
      }
      if (!element || !element.href) return;

      var href = element.getAttribute("href") || "";
      if (!/^(https?:|mailto:)/i.test(href)) return;

      // Never let a mail link navigate the sandbox. The parent validates the
      // URL again before handing it to the OS-default browser.
      event.preventDefault();
      event.stopPropagation();
      if (typeof event.stopImmediatePropagation === "function") {
        event.stopImmediatePropagation();
      }

      window.parent.postMessage(
        { type: "cm:open-url", href: element.href },
        "*",
      );
    },
    true,
  );
})();
