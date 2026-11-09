// Browser-owned link affordances. The native side accepts only bounded web URLs.
(() => {
  const post = (kind, value = "") => {
    if (window.ipc) window.ipc.postMessage(`felifox-link:${kind}:${value}`);
  };
  const linkAt = (event) => {
    for (const node of event.composedPath()) {
      if (node instanceof Element) {
        const anchor = node.closest("a[href], area[href]");
        if (anchor) {
          try {
            const url = new URL(anchor.href, document.baseURI);
            if (url.protocol === "http:" || url.protocol === "https:")
              return url.href;
          } catch (_) {
            /* An invalid href isn't a navigable web link. */
          }
        }
      }
    }
    return null;
  };

  let hovered = null;
  const hover = (url) => {
    if (hovered === url) return;
    hovered = url;
    post("hover", url || "");
  };
  window.addEventListener("pointerover", (event) => hover(linkAt(event)), true);
  // A click in the page ends editing in the address bar, as in other browsers.
  window.addEventListener("pointerdown", () => post("focus"), true);
  window.addEventListener(
    "pointerout",
    (event) => {
      const target = event.relatedTarget;
      const anchor =
        target instanceof Element && target.closest("a[href], area[href]");
      let url = null;
      if (anchor) {
        try {
          const resolved = new URL(anchor.href, document.baseURI);
          if (/^https?:$/.test(resolved.protocol)) url = resolved.href;
        } catch (_) {
          /* Clear the old status on invalid links. */
        }
      }
      hover(url);
    },
    true,
  );
  document.addEventListener("mouseleave", () => hover(null), true);
  window.addEventListener("blur", () => hover(null));
  window.addEventListener("pagehide", () => hover(null));
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) hover(null);
  });

  window.addEventListener(
    "click",
    (event) => {
      if (event.button !== 0 || !event.metaKey) return;
      const url = linkAt(event);
      if (!url) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      post(event.shiftKey ? "open" : "background", url);
    },
    true,
  );
  // Also support the standard mouse-wheel click convention.
  window.addEventListener(
    "auxclick",
    (event) => {
      if (event.button !== 1) return;
      const url = linkAt(event);
      if (!url) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      post("background", url);
    },
    true,
  );

  // A shadow-root menu avoids a GPUI overlay being covered by the native WebKit view.
  // It is document-local: no unrestricted native API or filesystem bridge is exposed.
  let dismiss = () => {};
  window.addEventListener(
    "contextmenu",
    (event) => {
      const url = linkAt(event);
      if (!url) return; // Preserve WebKit's normal menu for non-links.
      event.preventDefault();
      event.stopImmediatePropagation();
      dismiss();
      hover(url);
      const previousFocus = document.activeElement;
      const host = document.createElement("div");
      host.setAttribute("data-felifox-link-menu", "");
      host.style.cssText =
        "all:initial!important;position:fixed!important;inset:0!important;z-index:2147483647!important;pointer-events:none!important;";
      const root = host.attachShadow({ mode: "closed" });
      const style = document.createElement("style");
      style.textContent = `
      :host { color-scheme: light; }
      div { position:fixed; min-width:190px; padding:5px; background:#fff;
        border:1px solid #d7cce5; border-radius:10px; box-shadow:0 8px 30px #17052d35;
        font:13px -apple-system,BlinkMacSystemFont,sans-serif; pointer-events:auto; }
      button { display:block; width:100%; border:0; background:transparent; color:#2b0a54;
        text-align:left; padding:9px 12px; border-radius:6px; font:inherit; cursor:pointer; }
      button:hover, button:focus-visible { background:#efe5ff; outline:none; }
    `;
      const menu = document.createElement("div");
      menu.setAttribute("role", "menu");
      menu.setAttribute("aria-label", "Link actions");
      const button = document.createElement("button");
      button.type = "button";
      button.setAttribute("role", "menuitem");
      button.textContent = "Open Link in New Tab";
      menu.append(button);
      root.append(style, menu);
      document.documentElement.append(host);
      const width = menu.getBoundingClientRect().width;
      const height = menu.getBoundingClientRect().height;
      menu.style.left = `${Math.max(4, Math.min(event.clientX, innerWidth - width - 4))}px`;
      menu.style.top = `${Math.max(4, Math.min(event.clientY, innerHeight - height - 4))}px`;
      const controller = new AbortController();
      dismiss = () => {
        controller.abort();
        host.remove();
        if (previousFocus && previousFocus.isConnected)
          previousFocus.focus({ preventScroll: true });
        dismiss = () => {};
      };
      const options = { capture: true, signal: controller.signal };
      button.addEventListener("click", (event) => {
        event.preventDefault();
        event.stopPropagation();
        dismiss();
        post("background", url);
      });
      window.addEventListener(
        "pointerdown",
        (event) => {
          if (!event.composedPath().includes(host)) dismiss();
        },
        options,
      );
      window.addEventListener(
        "keydown",
        (event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            event.stopImmediatePropagation();
            button.click();
          } else if (event.key === "Escape" || event.key === "Tab") {
            event.preventDefault();
            event.stopImmediatePropagation();
            dismiss();
          }
        },
        options,
      );
      window.addEventListener("blur", () => dismiss(), {
        once: true,
        signal: controller.signal,
      });
      window.addEventListener("scroll", () => dismiss(), options);
      button.focus({ preventScroll: true });
    },
    true,
  );
})();
