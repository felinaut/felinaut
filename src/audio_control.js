// Browser-owned per-tab volume, mute and EQ. Runs at document start in every frame.
//
// Native code only calls `__felifoxAudio(gain, preset)` in the main frame, which relays the
// settings to child frames. Frames report two booleans back. A page can change its own (and
// its embedded frames') audio either way, so the relay grants it nothing new.
//
// While a tab uses the default settings, nothing here touches media elements. Once the user
// changes volume or EQ, playing media is routed through Web Audio:
//   media element ─┐
//   page AudioContext.destination ─┴─► low shelf ► presence ► high shelf ► gain ► (limiter) ► speakers
// Media from another origin without CORS would be silenced by Web Audio, so it falls back to
// scaling `volume` (0–100 %, no EQ) and is reported as "limited".
(() => {
  if (window.__felifoxAudio) return;
  const KEY = "__felifoxAudioSettings";
  const HELLO = "__felifoxAudioHello";
  const PRESETS = {
    flat: [0, 0, 0],
    voice: [-7, 6, 2],
    bass: [9, -1, 0],
  };
  const frameId = Math.random().toString(36).slice(2, 12) || "frame";
  const isTop = window === window.top;
  const NativeContext = window.AudioContext || window.webkitAudioContext;
  const post = (message) => {
    try {
      if (window.ipc) window.ipc.postMessage(`felifox-audio:${message}`);
    } catch (_) {
      /* The bridge can vanish while a frame unloads. */
    }
  };

  let settings = { gain: 1, preset: "flat" };
  const isDefault = () => settings.gain === 1 && settings.preset === "flat";

  // ---- Shared processing chain ------------------------------------------------------------
  const chains = new Set();
  const makeChain = (context, output) => {
    const input = context.createGain();
    const low = context.createBiquadFilter();
    low.type = "lowshelf";
    low.frequency.value = 160;
    const mid = context.createBiquadFilter();
    mid.type = "peaking";
    mid.frequency.value = 2600;
    mid.Q.value = 0.9;
    const high = context.createBiquadFilter();
    high.type = "highshelf";
    high.frequency.value = 8000;
    const gain = context.createGain();
    const limiter = context.createDynamicsCompressor();
    limiter.threshold.value = -2;
    limiter.knee.value = 0;
    limiter.ratio.value = 20;
    limiter.attack.value = 0.002;
    limiter.release.value = 0.2;
    input.connect(low);
    low.connect(mid);
    mid.connect(high);
    high.connect(gain);
    gain.connect(output);
    limiter.connect(output);
    let limiting = false;
    const chain = {
      context,
      input,
      apply() {
        const now = context.currentTime;
        const [lowDb, midDb, highDb] = PRESETS[settings.preset] || PRESETS.flat;
        low.gain.setTargetAtTime(lowDb, now, 0.03);
        mid.gain.setTargetAtTime(midDb, now, 0.03);
        high.gain.setTargetAtTime(highDb, now, 0.03);
        gain.gain.setTargetAtTime(settings.gain, now, 0.03);
        // The limiter tames clipping when boosting; at natural levels it stays out of the path.
        const wantLimiter = settings.gain > 1 || settings.preset === "bass";
        if (wantLimiter !== limiting) {
          limiting = wantLimiter;
          gain.disconnect();
          gain.connect(limiting ? limiter : output);
        }
      },
    };
    chains.add(chain);
    chain.apply();
    return chain;
  };
  const applyChains = () => {
    for (const chain of chains) {
      if (chain.context.state === "closed") chains.delete(chain);
      else chain.apply();
    }
  };

  // ---- Page-created Web Audio -------------------------------------------------------------
  // Pages see a gain node that behaves like their destination; it feeds the tab's chain.
  const contextProto = (window.BaseAudioContext || NativeContext || {}).prototype;
  const destination =
    contextProto && Object.getOwnPropertyDescriptor(contextProto, "destination");
  let ownContext = null;
  if (destination && destination.get && NativeContext) {
    const realDestination = destination.get;
    const pageChains = new WeakMap();
    Object.defineProperty(contextProto, "destination", {
      configurable: true,
      enumerable: destination.enumerable,
      get() {
        const real = realDestination.call(this);
        // Offline rendering is not playback, and our own context feeds the speakers directly.
        if (!(this instanceof NativeContext) || this === ownContext) return real;
        let chain = pageChains.get(this);
        if (!chain) {
          try {
            chain = makeChain(this, real);
            const input = chain.input;
            input.channelCount = real.channelCount;
            input.channelCountMode = "explicit";
            input.channelInterpretation = real.channelInterpretation;
            Object.defineProperty(input, "maxChannelCount", {
              get: () => real.maxChannelCount,
            });
          } catch (_) {
            return real;
          }
          pageChains.set(this, chain);
        }
        return chain.input;
      },
    });
  }
  const outputOf = (context) =>
    destination && destination.get ? destination.get.call(context) : context.destination;

  // ---- Media elements -----------------------------------------------------------------------
  const media = new Set(); // WeakRefs, pruned on use
  const seen = new WeakSet();
  const routed = new WeakSet();
  const scaled = new WeakMap(); // element -> { pageVolume, applied }
  let mediaChain = null;

  const elements = () => {
    const live = [];
    for (const ref of media) {
      const element = ref.deref();
      if (element) live.push(element);
      else media.delete(ref);
    }
    return live;
  };

  const ensureContext = () => {
    if (!NativeContext) return null;
    if (!ownContext || ownContext.state === "closed") {
      try {
        ownContext = new NativeContext({ latencyHint: "playback" });
      } catch (_) {
        ownContext = null;
        return null;
      }
      mediaChain = makeChain(ownContext, outputOf(ownContext));
      ownContext.addEventListener("statechange", () => {
        if (ownContext.state !== "running" && ownContext.state !== "closed")
          ownContext.resume().catch(() => {});
        refresh();
      });
    }
    if (ownContext.state !== "running") ownContext.resume().catch(() => {});
    return ownContext;
  };

  // Web Audio silences cross-origin media fetched without CORS, so only route media we can hear.
  const routable = (element) => {
    if (element.srcObject) return false;
    const source = element.currentSrc || element.src;
    if (!source) return false;
    try {
      const url = new URL(source, document.baseURI);
      if (url.protocol === "blob:" || url.protocol === "data:") return true;
      if (url.origin === location.origin) return true;
    } catch (_) {
      return false;
    }
    return element.crossOrigin !== null;
  };

  const started = (element) =>
    !element.paused || element.autoplay || element.currentTime > 0;

  const route = (element) => {
    if (routed.has(element)) return true;
    if (!routable(element)) return false;
    const context = ensureContext();
    // Never move audio into a context that cannot play yet; that would silence the page.
    if (!context || context.state !== "running") return false;
    try {
      context.createMediaElementSource(element).connect(mediaChain.input);
    } catch (_) {
      // The page already owns a MediaElementSource; its context feeds our chain through the
      // patched destination, so scaling `volume` too would apply the setting twice.
      routed.add(element);
      unscale(element);
      return true;
    }
    routed.add(element);
    unscale(element);
    return true;
  };

  const unscale = (element) => {
    const state = scaled.get(element);
    if (!state) return;
    scaled.delete(element);
    if (element.volume === state.applied) element.volume = state.pageVolume;
  };

  const scale = (element) => {
    let state = scaled.get(element);
    if (!state) {
      state = { pageVolume: element.volume, applied: element.volume };
      scaled.set(element, state);
    } else if (element.volume !== state.applied) {
      state.pageVolume = element.volume; // The page's own player changed volume.
    }
    const target = Math.round(state.pageVolume * Math.min(1, settings.gain) * 1000) / 1000;
    state.applied = target;
    if (element.volume !== target) element.volume = target;
  };

  const audible = (element) => {
    if (element.paused || element.ended || element.muted) return false;
    const state = scaled.get(element);
    if ((state ? state.pageVolume : element.volume) <= 0) return false;
    if (element.audioTracks && element.audioTracks.length === 0) return false;
    if (
      typeof element.webkitAudioDecodedByteCount === "number" &&
      element.currentTime > 0.5 &&
      element.webkitAudioDecodedByteCount === 0
    )
      return false;
    return true;
  };

  let reported = "";
  let timer = 0;
  let limited = false;
  const report = () => {
    const playing = elements().filter(audible);
    const state = `${playing.length ? 1 : 0}:${limited && playing.length ? 1 : 0}`;
    if (state !== reported) {
      reported = state;
      post(`state:${frameId}:${state}`);
    }
    // Keep sampling while something plays: audio decoding and tracks appear asynchronously.
    const anyPlaying = elements().some((element) => !element.paused);
    if (anyPlaying && !timer) {
      timer = setInterval(report, 1000);
    } else if (!anyPlaying && timer) {
      clearInterval(timer);
      timer = 0;
    }
  };

  const refresh = () => {
    limited = false;
    for (const element of elements()) {
      if (routed.has(element)) continue;
      if (isDefault()) {
        unscale(element);
        continue;
      }
      if (!started(element)) continue;
      if (route(element)) continue;
      scale(element);
      // Routable media is only waiting for the audio context to start; it is not limited.
      if (!element.paused && !routable(element) && (settings.gain > 1 || settings.preset !== "flat"))
        limited = true;
    }
    report();
  };

  const track = (element) => {
    if (!(element instanceof HTMLMediaElement) || seen.has(element)) return;
    seen.add(element);
    media.add(new WeakRef(element));
  };

  const onMediaEvent = (event) => {
    const element = event.target;
    if (!(element instanceof HTMLMediaElement)) return;
    track(element);
    if (event.type === "volumechange" && scaled.has(element)) {
      const state = scaled.get(element);
      if (element.volume !== state.applied) scale(element);
    }
    if (event.type === "play" || event.type === "playing" || event.type === "loadeddata")
      refresh();
    else report();
  };
  const MEDIA_EVENTS = [
    "play",
    "playing",
    "pause",
    "ended",
    "emptied",
    "loadeddata",
    "volumechange",
  ];
  const listen = (target) => {
    for (const type of MEDIA_EVENTS) target.addEventListener(type, onMediaEvent, true);
  };
  listen(window);
  // Media events do not cross shadow boundaries; listen inside every shadow root as well.
  const attachShadow = Element.prototype.attachShadow;
  if (attachShadow) {
    Element.prototype.attachShadow = function (...args) {
      const root = attachShadow.apply(this, args);
      listen(root);
      return root;
    };
  }
  // Detached `new Audio()` players never dispatch events through the document.
  const play = HTMLMediaElement.prototype.play;
  HTMLMediaElement.prototype.play = function (...args) {
    track(this);
    const result = play.apply(this, args);
    if (!isDefault()) queueMicrotask(refresh);
    return result;
  };
  for (const element of document.querySelectorAll("audio, video")) track(element);

  // ---- Settings -------------------------------------------------------------------------------
  const relay = () => {
    for (let index = 0; index < window.frames.length; index += 1) {
      try {
        window.frames[index].postMessage({ [KEY]: settings }, "*");
      } catch (_) {
        /* Detached frame. */
      }
    }
  };
  const apply = (gain, preset) => {
    gain = Number(gain);
    if (!Number.isFinite(gain) || gain < 0 || gain > 6) return;
    if (!Object.prototype.hasOwnProperty.call(PRESETS, preset)) return;
    settings = { gain, preset };
    applyChains();
    if (!isDefault()) for (const element of document.querySelectorAll("audio, video")) track(element);
    refresh();
    relay();
  };
  Object.defineProperty(window, "__felifoxAudio", {
    value: apply,
    enumerable: false,
    writable: false,
    configurable: false,
  });

  window.addEventListener(
    "message",
    (event) => {
      const data = event.data;
      if (!data || typeof data !== "object") return;
      if (HELLO in data) {
        // A child frame starting up asks its parent for the current settings.
        if (event.source && window.frames.length) {
          event.stopImmediatePropagation();
          try {
            event.source.postMessage({ [KEY]: settings }, "*");
          } catch (_) {
            /* Gone already. */
          }
        }
        return;
      }
      if (!(KEY in data) || isTop || event.source !== window.parent) return;
      event.stopImmediatePropagation();
      const next = data[KEY];
      if (next && typeof next === "object") apply(next.gain, next.preset);
    },
    true,
  );

  if (isTop) {
    post("hello");
    if (!window.ipc) document.addEventListener("DOMContentLoaded", () => post("hello"), { once: true });
  } else {
    try {
      window.parent.postMessage({ [HELLO]: 1 }, "*");
    } catch (_) {
      /* Sandboxed or detached. */
    }
  }
  window.addEventListener("pagehide", () => {
    reported = "";
    post(`state:${frameId}:0:0`);
  });
  window.addEventListener("pageshow", (event) => {
    if (event.persisted) report();
  });
})();
