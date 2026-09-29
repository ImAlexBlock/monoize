import { useEffect, useRef } from "react";
import { useReducedMotion } from "framer-motion";

// Decorative "character ocean" canvas (PS-V7).
//
// A dense grid of small glyphs. Brightness follows a travelling wave field, so
// the surface reads as flowing glitter rather than static text. The pointer
// adds a smoothed, bounded lift along its path.
//
// Bounded: fixed cell grid with a cap, loop paused off-screen or hidden, and a
// single static frame with no listeners under reduced motion.
const CELL = 8;
const MAX_CELLS = 24000;
const GLYPHS = ["·", "˙", ".", "ˑ", "﹒", "·", "•"];
const clamp = (value: number, min: number, max: number) => Math.min(max, Math.max(min, value));

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
    let cols = 0;
    let rows = 0;
    let cell = CELL;
    let glyphs: string[] = [];
    let phases = new Float32Array(0);
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

      cell = CELL;
      while (Math.ceil(width / cell) * Math.ceil(height / cell) > MAX_CELLS) cell += 2;
      cols = Math.ceil(width / cell);
      rows = Math.ceil(height / cell);
      glyphs = new Array(cols * rows);
      phases = new Float32Array(cols * rows);
      for (let i = 0; i < glyphs.length; i++) {
        glyphs[i] = GLYPHS[(Math.random() * GLYPHS.length) | 0];
        phases[i] = Math.random() * Math.PI * 2;
      }
      color = readColor();
    };

    const draw = (time: number) => {
      const t = time / 1000;
      ctx.clearRect(0, 0, width, height);
      ctx.font = `${cell + 1}px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace`;
      ctx.textBaseline = "middle";
      ctx.fillStyle = color;

      pointer.x += (pointer.tx - pointer.x) * 0.08;
      pointer.y += (pointer.ty - pointer.y) * 0.08;
      pointer.vx *= 0.86;
      pointer.vy *= 0.86;
      const pointerSpeed = Math.min(1, Math.hypot(pointer.vx, pointer.vy) / 20);

      const lift = 3 + pointerSpeed * 4;

      for (let cy = 0; cy < rows; cy++) {
        const y = cy * cell;
        for (let cx = 0; cx < cols; cx++) {
          const x = cx * cell;
          const i = cy * cols + cx;

          // Travelling wave field: mostly horizontal bands with a slow cross
          // swell, so the surface flows sideways on its own.
          const wave =
            Math.sin(x * 0.014 + t * 0.9) * 0.9 +
            Math.sin(x * 0.006 - t * 0.5 + y * 0.018) * 0.8 +
            Math.sin(y * 0.04 + t * 0.45) * 0.5;

          const dist = Math.hypot(x - pointer.x, y - pointer.y);
          const influence = Math.max(0, 1 - dist / 200) * (0.35 + pointerSpeed * 0.65);

          const crest = Math.max(0, wave + influence * 1.6);
          const base = Math.min(1, crest / 1.7);
          const sparkle = 0.55 + 0.45 * Math.sin(x * 0.5 + y * 0.7 + t * 3 + phases[i]);
          const twinkle = 0.5 + 0.5 * Math.sin(t * 4 + phases[i] * 3);
          const alpha = base * (0.3 + 0.7 * sparkle) * (0.5 + 0.5 * twinkle) * (0.9 + influence * 1.2);
          if (alpha <= 0.03) continue;

          ctx.globalAlpha = Math.min(0.9, alpha * 1.6);
          ctx.fillText(glyphs[i], x, y - influence * lift);
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
      if (pointer.tx < -1000) {
        // First sample: snap so the field does not jump in from off-screen.
        pointer.x = nx;
        pointer.y = ny;
        pointer.vx = 0;
        pointer.vy = 0;
      } else {
        pointer.vx = clamp(nx - pointer.tx, -30, 30);
        pointer.vy = clamp(ny - pointer.ty, -30, 30);
      }
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
