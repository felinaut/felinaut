// Tells the browser when a sideways trackpad scroll was the page's: it scrolled something, or
// the page handled the wheel event itself (a map, a carousel). Such a swipe must not also go
// back or forward. The message carries nothing about the page.
(() => {
  if (window.top !== window) return;
  // What could scroll sideways under the pointer, with its offset at the last wheel event.
  let watched = [];
  let reported = 0;
  const offset = (target) => (target === window ? window.scrollX : target.scrollLeft);
  const report = () => {
    const now = performance.now();
    if (now - reported < 100) return;
    reported = now;
    if (window.ipc) window.ipc.postMessage("felifox-swipe:scrolled");
  };
  const check = () => {
    if (watched.some(([target, left]) => offset(target) !== left)) {
      watched = [];
      report();
    }
  };
  addEventListener(
    "wheel",
    (event) => {
      if (Math.abs(event.deltaX) <= Math.abs(event.deltaY)) return;
      check();
      watched = [[window, window.scrollX]];
      for (let node = event.target; node instanceof Element; node = node.parentElement) {
        if (node.scrollWidth > node.clientWidth) watched.push([node, node.scrollLeft]);
      }
      // After every handler of the page ran: did one of them take the event?
      setTimeout(() => {
        if (event.defaultPrevented) report();
      }, 0);
    },
    { capture: true, passive: true },
  );
  addEventListener("scroll", check, { capture: true, passive: true });
})();
