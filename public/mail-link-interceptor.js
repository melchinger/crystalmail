// CrystalMail's trusted mail-viewer bridge.
//
// This file is deliberately loaded as an external script. Tauri adds a
// nonce/hash to the app-shell CSP in production, which correctly blocks
// inline scripts in the sandboxed mail document. Keeping this tiny bridge in
// the application bundle preserves that CSP without making mail HTML trusted.
(function () {
  // Keep the sandboxed mail document from navigating away even for links
  // that are not safe to open externally (relative, ftp:, protocol-relative)
  // and for form submissions. The parent only handles http/https/mailto.
  window.addEventListener("beforeunload", function (event) {
    event.preventDefault();
    event.returnValue = "";
  });

  document.addEventListener(
    "submit",
    function (event) {
      event.preventDefault();
      event.stopPropagation();
    },
    true,
  );

  document.addEventListener(
    "click",
    function (event) {
      var element = event.target;
      while (element && element.nodeName !== "A") {
        element = element.parentNode;
      }
      if (!element || !element.href) return;

      // Never let a mail link navigate the sandbox. The parent validates the
      // URL again before handing it to the OS-default browser.
      event.preventDefault();
      event.stopPropagation();
      if (typeof event.stopImmediatePropagation === "function") {
        event.stopImmediatePropagation();
      }

      var href = element.getAttribute("href") || "";
      if (!/^(https?:|mailto:)/i.test(href)) return;

      window.parent.postMessage(
        { type: "cm:open-url", href: element.href },
        "*",
      );
    },
    true,
  );
})();
