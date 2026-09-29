import { useEffect, useRef } from "react";
import { useReducedMotion } from "framer-motion";

// Decorative "character ocean" canvas (PS-V7). A grid of monospace glyphs rides
// layered sine waves; the pointer adds a ripple that follows its trajectory.
// Everything is bounded: the cell grid is capped, the loop pauses when the
// canvas is off-screen or the document is hidden, and reduced motion renders a
// single static frame with no event listeners.
const CELL = 16;
const MAX_CELLS = 5200;
const GLYPHS = "abcdefghijklmnopqrstuvwxyz0123456789·-~≈+*#".split("");

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

      // Grow the cell size when the grid would exceed the cap, so cost stays bounded.
      cell = CELL;
      while (Math.ceil(width / cell) * Math.ceil(height / cell) > MAX_CELLS) cell += 4;
      cols = Math.ceil(width / cell);
      rows = Math.ceil(height / cell);
      glyphs = new Array(cols * rows);
      for (let i = 0; i < glyphs.length; i++) glyphs[i] = GLYPHS[(Math.random() * GLYPHS.length) | 0];
      color = readColor();
    };

    const draw = (time: number) => {
      const t = time / 1000;
      ctx.clearRect(0, 0, width, height);
      ctx.font = `${cell - 4}px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace`;
      ctx.textBaseline = "middle";
      ctx.fillStyle = color;

      pointer.x += (pointer.tx - pointer.x) * 0.14;
      pointer.y += (pointer.ty - pointer.y) * 0.14;
      pointer.vx *= 0.9;
      pointer.vy *= 0.9;
      const speed = Math.min(1, Math.hypot(pointer.vx, pointer.vy) / 26);

      for (let cy = 0; cy < rows; cy++) {
        const y = cy * cell;
        for (let cx = 0; cx < cols; cx++) {
          const x = cx * cell;
          // Slow, directional current: low frequencies and slow time make the
          // field drift like water. The pointer ripple is the only fast term.
          const wave =
            Math.sin(x * 0.008 + t * 0.32) * 1.15 +
            Math.sin(y * 0.026 - t * 0.2) +
            Math.sin((x * 0.5 + y) * 0.006 + t * 0.16) * 0.85;

          const dx = x - pointer.x;
          const dy = y - pointer.y;
          const dist = Math.hypot(dx, dy);
          const influence = Math.max(0, 1 - dist / 240) * (0.35 + speed);
          const ripple = influence * Math.sin(dist * 0.06 - t * 4.2) * 1.5;

          const level = (wave + ripple) * 0.5;
          // Only the crests show: troughs stay empty, so the field reads as
          // flowing water bands instead of a uniform wall of glyphs.
          const crest = Math.max(0, level - 0.15);
          const alpha = crest * 0.42 + influence * 0.34;
          if (alpha <= 0.02) continue;

          ctx.globalAlpha = Math.min(0.5, alpha);
          ctx.fillText(glyphs[cy * cols + cx], x, y + level * 6);
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
