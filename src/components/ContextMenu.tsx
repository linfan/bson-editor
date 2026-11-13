/**
 * A right-click menu rendered into `document.body`.
 *
 * The tree rows live inside a scrolling container, so an absolutely positioned
 * menu would be clipped by it. A portal plus fixed positioning keeps the menu
 * above everything, and the coordinates are clamped so a right-click near an
 * edge does not push it off screen.
 */
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

export interface MenuItem {
  label: string;
  onSelect: () => void;
}

interface Props {
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
}

export function ContextMenu({ x, y, items, onClose }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const [at, setAt] = useState({ left: x, top: y });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    setAt({
      left: Math.max(4, Math.min(x, window.innerWidth - width - 8)),
      top: Math.max(4, Math.min(y, window.innerHeight - height - 8)),
    });
  }, [x, y]);

  useEffect(() => {
    const onDown = (event: PointerEvent) => {
      if (!ref.current?.contains(event.target as Node)) onClose();
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    const onScroll = () => onClose();
    window.addEventListener("pointerdown", onDown, true);
    window.addEventListener("keydown", onKey);
    // Capture, because scroll events do not bubble out of the tree.
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onScroll);
    return () => {
      window.removeEventListener("pointerdown", onDown, true);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onScroll);
    };
  }, [onClose]);

  return createPortal(
    <div ref={ref} className="ctxmenu" role="menu" style={{ left: at.left, top: at.top }}>
      {items.map((item) => (
        <button
          key={item.label}
          role="menuitem"
          onClick={() => {
            onClose();
            item.onSelect();
          }}
        >
          {item.label}
        </button>
      ))}
    </div>,
    document.body,
  );
}
