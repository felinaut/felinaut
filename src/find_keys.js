// The find shortcuts go to the page first. This script reports a key that the page did not
// take, and the browser opens its find bar then. A page that prevents the default keeps the key.
(() => {
  addEventListener(
    "keydown",
    (event) => {
      if (!event.isTrusted || !event.metaKey || event.ctrlKey || event.altKey) return;
      const key = event.key.toLowerCase();
      if (key !== "f" && key !== "g") return;
      const step = key === "f" ? 0 : event.shiftKey ? -1 : 1;
      // Read the result after all handlers of the page, also one that stops propagation.
      setTimeout(() => {
        if (!event.defaultPrevented && window.ipc) window.ipc.postMessage(`felifox-find:${step}`);
      }, 0);
    },
    true,
  );
})();
