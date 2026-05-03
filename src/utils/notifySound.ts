/**
 * Subtle two-note chime played on new-mail arrival. Synthesized via
 * Web Audio API so we don't have to ship a WAV, keep bundle size
 * small, and give the user a volume slider without re-encoding.
 *
 * The "chime" is a pair of sine-wave beeps at A5 (880 Hz) and E6
 * (1319 Hz), each ~180 ms with exponential decay. Short enough to
 * feel like a glance, not an interruption; high enough to be
 * audible over typical desktop background noise without being
 * shrill.
 *
 * Browsers suspend AudioContext until a user gesture has happened.
 * Until then we silently skip — acceptable: the first notification
 * after cold start just won't chime. Subsequent syncs will.
 */

let cachedCtx: AudioContext | null = null;

function getContext(): AudioContext | null {
  // Lazy-init: creating a context before the first user gesture
  // triggers an Autoplay warning on some browsers even if we
  // never `.resume()`.
  if (cachedCtx) return cachedCtx;
  try {
    // @ts-expect-error webkitAudioContext is Safari-legacy but still
    // shipped in some WebView2 builds on older Windows images.
    const Ctor = window.AudioContext || window.webkitAudioContext;
    if (!Ctor) return null;
    cachedCtx = new Ctor();
    return cachedCtx;
  } catch {
    return null;
  }
}

function tone(
  ctx: AudioContext,
  freq: number,
  startAt: number,
  durationMs: number,
  volume: number,
) {
  const osc = ctx.createOscillator();
  const gain = ctx.createGain();
  osc.frequency.value = freq;
  osc.type = "sine";
  // Quick attack (5 ms) to avoid click, then exponential decay over
  // the full duration. Exponential = perceptually linear fade for
  // the ear; a linear ramp sounds unnaturally abrupt at the tail.
  const peak = Math.max(0.0001, volume);
  const end = startAt + durationMs / 1000;
  gain.gain.setValueAtTime(0.0001, startAt);
  gain.gain.exponentialRampToValueAtTime(peak, startAt + 0.005);
  gain.gain.exponentialRampToValueAtTime(0.0001, end);
  osc.connect(gain).connect(ctx.destination);
  osc.start(startAt);
  osc.stop(end + 0.05);
}

/**
 * Play the new-mail chime once. `volume` is a linear 0..1 factor —
 * 0.5 is a good "subtle" default. Non-throwing; if audio is
 * unavailable (no user gesture yet, disabled browser policy, …) the
 * call returns silently.
 */
export function playNotifySound(volume = 0.5): void {
  const ctx = getContext();
  if (!ctx) return;
  // If the context is suspended (pre-first-gesture), try to resume
  // — some browsers allow resume from a subsequent gesture, but
  // we're called from a timer here so it often won't work. Ignoring
  // the failure keeps the caller simple.
  if (ctx.state === "suspended") {
    void ctx.resume().catch(() => {});
  }
  const t0 = ctx.currentTime;
  tone(ctx, 880, t0, 180, volume);
  tone(ctx, 1319, t0 + 0.14, 200, volume * 0.85);
}
