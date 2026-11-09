// Hands the native side the site's icon as a tiny PNG. Only same-origin or CORS-enabled
// icons can be read; anything else leaves the site with its monogram.
(() => {
  if (window.top !== window) return;
  const candidates = () => {
    const links = [
      ...document.querySelectorAll(
        'link[rel~="icon"], link[rel="shortcut icon"], link[rel="apple-touch-icon"]',
      ),
    ]
      .map((link) => link.href)
      .filter(Boolean);
    links.push(new URL("/favicon.ico", location.href).href);
    return [...new Set(links)];
  };
  // Decoding a blob directly sidesteps the page's img-src policy, which often bans blob:
  // URLs; the element route remains for formats createImageBitmap rejects (some SVGs).
  const decode = async (blob) => {
    try {
      return await createImageBitmap(blob);
    } catch (_) {
      const image = new Image();
      const url = URL.createObjectURL(blob);
      try {
        image.src = url;
        await image.decode();
        return image;
      } finally {
        URL.revokeObjectURL(url);
      }
    }
  };
  const send = async () => {
    for (const href of candidates()) {
      try {
        const response = await fetch(href, { credentials: "omit" });
        if (!response.ok) continue;
        const blob = await response.blob();
        if (!blob.size || blob.size > 512 * 1024) continue;
        const canvas = document.createElement("canvas");
        canvas.width = canvas.height = 32;
        canvas.getContext("2d").drawImage(await decode(blob), 0, 0, 32, 32);
        const data = canvas.toDataURL("image/png");
        if (window.ipc && data.length < 64 * 1024)
          window.ipc.postMessage(`felifox-icon:${data}`);
        return;
      } catch (_) {
        /* Not readable or not an image: try the next candidate. */
      }
    }
  };
  if (document.readyState === "complete") send();
  else window.addEventListener("load", send, { once: true });
})();
