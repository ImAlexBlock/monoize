import { useEffect, useRef } from "react";
import { useReducedMotion } from "framer-motion";

// Decorative "character ocean" canvas (PS-V7).
//
// The surface is drawn as stacked wavy character lines in perspective: lines
// near the horizon are small, dim, and widely spaced (blank space reads as
// distance); lines toward the viewer are larger, bolder, and brighter. The
// pointer raises a traveling bump, so a ripple follows the cursor.
//
// Everything is bounded: the line/column loop is fixed size, the loop pauses
// when off-screen or hidden, and reduced motion renders one static frame with
// no pointer listeners.
const GLYPHS = ["·", ".", "˙", "-", "~", "≈", "﹏"];
const LINES = 24;

function glyphAt(x: number, row: number) {
  const h = Math.abs(Math.sin(x * 0.37 + row * 12.9) * 43758.5453) % 1;
  return GLYPHS[(h * GLYPHS.length) | 0];
}

export function CharacterOcean({ className = "" }: { className?: string }) {
  const ref = useRef<HTMLCanvasElement>(null);
  const reduceMotion = useReducedMotion();

  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    let width = 0;
    let height = 0;
    let raf = 0;
    let onScreen = true;
    let running = true;

    const pointer = { x: -9999, y: -9999, tx: -9999, ty: -9999, vx: 0, vy: 0 };

    const readColor = () => {
      const token = getComputedStyle(canvas).getPropertyValue("--foreground").trim();
      return token ? `hsl(${token})` : "rgba(128,128,128,1)";
    };
    let color = readColor();

    const resize = () => {
      const rect = canvas.getBoundingClientRect();
      width = Math.max(1, Math.floor(rect.width));
      height = Math.max(1, Math.floor(rect.height));
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      canvas.width = Math.floor(width * dpr);
      canvas.height = Math.floor(height * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      color = readColor();
    };

    const draw = (time: number) => {
      const t = time / 1000;
      ctx.clearRect(0, 0, width, height);
      ctx.textBaseline = "middle";
      ctx.fillStyle = color;

      pointer.x += (pointer.tx - pointer.x) * 0.14;
      pointer.y += (pointer.ty - pointer.y) * 0.14;
      pointer.vx *= 0.9;
      pointer.vy *= 0.9;
      const pointerSpeed = Math.min(1, Math.hypot(pointer.vx, pointer.vy) / 26);

      const horizon = height * 0.26;
      const sea = height - horizon;

      for (let row = 0; row < LINES; row++) {
        const depth = row / (LINES - 1); // 0 = far (horizon), 1 = near
        const y0 = horizon + sea * Math.pow(depth, 1.7);
        const size = 8 + depth * 10;
        const amplitude = 3 + depth * 26;
        const frequency = 0.022 - depth * 0.012;
        const speed = 0.7 + depth * 0.6;
        const weight = depth > 0.66 ? 700 : depth > 0.33 ? 500 : 400;
        const baseAlpha = 0.05 + depth * 0.5;
        const step = size * (1.4 + (1 - depth) * 1.1);

        ctx.font = `${weight} ${Math.round(size)}px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace`;

        for (let x = -step; x < width + step; x += step) {
          const phase = row * 0.7;
          const wave =
            Math.sin(x * frequency + t * speed + phase) +
            0.5 * Math.sin(x * frequency * 2.2 - t * speed * 0.8 + phase * 1.3);
          const waveY = y0 + wave * amplitude;

          const dist = Math.hypot(x - pointer.x, waveY - pointer.y);
          const influence = Math.max(0, 1 - dist / 260) * (0.4 + pointerSpeed);

          const alpha = Math.min(0.95, baseAlpha + influence * 0.55);
          if (alpha < 0.04) continue;

          ctx.globalAlpha = alpha;
          ctx.fillText(glyphAt(x, row), x, waveY + influence * 16);
        }
      }
      ctx.globalAlpha = 1;
    };

    const loop = (time: number) => {
      if (!running) return;
      if (onScreen) draw(time);
      raf = requestAnimationFrame(loop);
    };

    const onPointerMove = (event: PointerEvent) => {
      const rect = canvas.getBoundingClientRect();
      const nx = event.clientX - rect.left;
      const ny = event.clientY - rect.top;
      pointer.vx = nx - pointer.tx;
      pointer.vy = ny - pointer.ty;
      pointer.tx = nx;
      pointer.ty = ny;
    };
    const onPointerLeave = () => {
      pointer.tx = -9999;
      pointer.ty = -9999;
    };

    resize();

    if (reduceMotion) {
      draw(0);
    } else {
      raf = requestAnimationFrame(loop);
      window.addEventListener("pointermove", onPointerMove, { passive: true });
      window.addEventListener("pointerout", onPointerLeave, { passive: true });
    }

    const resizeObserver = new ResizeObserver(() => {
      resize();
      if (reduceMotion) draw(0);
    });
    resizeObserver.observe(canvas);

    const intersectionObserver = new IntersectionObserver(
      (entries) => { for (const entry of entries) onScreen = entry.isIntersecting; },
      { threshold: 0 },
    );
    intersectionObserver.observe(canvas);

    const onVisibility = () => {
      running = !document.hidden;
      if (running && !reduceMotion) {
        cancelAnimationFrame(raf);
        raf = requestAnimationFrame(loop);
      }
    };
    document.addEventListener("visibilitychange", onVisibility);

    return () => {
      cancelAnimationFrame(raf);
      resizeObserver.disconnect();
      intersectionObserver.disconnect();
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerout", onPointerLeave);
    };
  }, [reduceMotion]);

  return <canvas ref={ref} aria-hidden="true" className={className} />;
}
