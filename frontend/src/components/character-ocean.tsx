import { useEffect, useRef } from "react";
import { useReducedMotion } from "framer-motion";

// Decorative "character ocean" canvas (PS-V7). A grid of monospace glyphs rides
// layered sine waves; the pointer adds a ripple that follows its trajectory.
// Everything is bounded: the cell grid is capped, the loop pauses when the
// canvas is off-screen or the document is hidden, and reduced motion renders a
// single static frame with no event listeners.
const CELL = 10;
const MAX_CELLS = 9000;
const GLYPHS = ["·", "˙", ".", "ˑ", "﹒", "·", "•", "~"];

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

      // Grow the cell size when the grid would exceed the cap, so cost stays bounded.
      cell = CELL;
      while (Math.ceil(width / cell) * Math.ceil(height / cell) > MAX_CELLS) cell += 4;
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
      ctx.font = `${cell - 3}px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace`;
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
          const i = cy * cols + cx;

          // Sum of slow directional swells: the surface height field.
          const swell =
            Math.sin(x * 0.013 + t * 0.5) +
            Math.sin(y * 0.022 - t * 0.33) +
            Math.sin((x * 0.6 + y) * 0.009 + t * 0.24);

          const dx = x - pointer.x;
          const dy = y - pointer.y;
          const dist = Math.hypot(dx, dy);
          const influence = Math.max(0, 1 - dist / 220) * (0.4 + speed);

          // Water lights up on the crests. The pointer raises a local swell, so
          // the ripple pulls a band of sparkles along its trajectory.
          const crest = Math.max(0, swell + influence * 1.8 * Math.cos(dist * 0.05 - t * 4));
          const base = Math.pow(Math.min(1, crest / 2.4), 2.4);

          const speckle = 0.5 + 0.5 * Math.sin(x * 0.33 + y * 0.5 + t * 2.4 + phases[i]);
          const twinkle = 0.4 + 0.6 * Math.sin(t * 3.2 + phases[i] * 3);
          const alpha = base * (0.3 + 0.7 * speckle) * (0.5 + 0.5 * twinkle) * (0.9 + influence * 1.8);
          if (alpha <= 0.05) continue;

          ctx.globalAlpha = Math.min(0.9, alpha * 1.5);
          ctx.fillText(glyphs[i], x, y + swell * 2.2);
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
