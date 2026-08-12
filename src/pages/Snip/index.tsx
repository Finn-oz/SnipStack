import { convertFileSrc } from "@tauri-apps/api/core";
import { useMount, useUnmount } from "ahooks";
import {
  type FC,
  type MouseEvent as ReactMouseEvent,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import {
  getSnipFrame,
  snipCancel,
  snipConfirm,
  snipOverlayReady,
} from "@/commands";
import { log } from "@/utils/log";

interface Point {
  x: number;
  y: number;
}

interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** 小于该 CSS 像素的拖拽视为误触,复位选区而不提交。 */
const MIN_DRAG_PX = 4;

/** 覆盖层窗口 URL 形如 `#/snip?monitor=1`,取对应显示器序号。 */
const parseMonitor = (): number => {
  const query = window.location.hash.split("?")[1] ?? "";
  const value = Number(new URLSearchParams(query).get("monitor") ?? "0");

  if (Number.isInteger(value) && value >= 0) {
    return value;
  }
  return 0;
};

const normalizeRect = (from: Point, to: Point): Rect => {
  return {
    height: Math.abs(to.y - from.y),
    width: Math.abs(to.x - from.x),
    x: Math.min(from.x, to.x),
    y: Math.min(from.y, to.y),
  };
};

/**
 * 截屏取字覆盖层:显示冻结帧,拖拽框选后把 CSS 像素选区提交给 Rust。
 * 每个显示器一个本页面实例;物理像素换算在 Rust 侧完成。
 */
const Snip: FC = () => {
  const { t } = useTranslation("snip");
  const monitorRef = useRef(parseMonitor());
  const originRef = useRef<Point>(null);
  const submittedRef = useRef(false);
  const [frameSrc, setFrameSrc] = useState<string>();
  const [rect, setRect] = useState<Rect>();
  const [dragging, setDragging] = useState(false);

  const handleKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      void snipCancel();
    }
  };

  useMount(() => {
    const load = async () => {
      try {
        const path = await getSnipFrame(monitorRef.current);
        setFrameSrc(convertFileSrc(path));
      } catch (error) {
        log.error("load snip frame failed", error);
      }
    };

    void load();
    window.addEventListener("keydown", handleKeyDown);
  });

  useUnmount(() => {
    window.removeEventListener("keydown", handleKeyDown);
  });

  const handleFrameLoaded = () => {
    void snipOverlayReady(monitorRef.current);
  };

  const handleContextMenu = (event: ReactMouseEvent) => {
    event.preventDefault();
    void snipCancel();
  };

  const handleMouseDown = (event: ReactMouseEvent) => {
    if (event.button !== 0) {
      return;
    }
    originRef.current = { x: event.clientX, y: event.clientY };
    setDragging(true);
    setRect(undefined);
  };

  const handleMouseMove = (event: ReactMouseEvent) => {
    const origin = originRef.current;

    if (!dragging || !origin) {
      return;
    }
    setRect(normalizeRect(origin, { x: event.clientX, y: event.clientY }));
  };

  const handleMouseUp = (event: ReactMouseEvent) => {
    const origin = originRef.current;

    if (event.button !== 0 || !dragging || !origin) {
      return;
    }
    setDragging(false);
    originRef.current = null;

    const selection = normalizeRect(origin, {
      x: event.clientX,
      y: event.clientY,
    });

    if (selection.width < MIN_DRAG_PX || selection.height < MIN_DRAG_PX) {
      setRect(undefined);
      return;
    }
    if (submittedRef.current) {
      return;
    }
    submittedRef.current = true;
    void snipConfirm({ monitor: monitorRef.current, ...selection });
  };

  const handleMouseLeave = () => {
    if (!dragging) {
      return;
    }
    setDragging(false);
    originRef.current = null;
    setRect(undefined);
  };

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: 全屏截屏覆盖层是纯鼠标框选画布,键盘路径由 Esc 全局监听覆盖。
    <div
      className="fixed inset-0 cursor-crosshair select-none overflow-hidden bg-black"
      onContextMenu={handleContextMenu}
      onMouseDown={handleMouseDown}
      onMouseLeave={handleMouseLeave}
      onMouseMove={handleMouseMove}
      onMouseUp={handleMouseUp}
    >
      {frameSrc && (
        <img
          alt=""
          className="pointer-events-none block h-full w-full"
          draggable={false}
          onLoad={handleFrameLoaded}
          src={frameSrc}
        />
      )}
      {rect ? (
        <div
          className="pointer-events-none absolute border border-blue-400 border-solid"
          style={{
            boxShadow: "0 0 0 100000px rgba(0, 0, 0, 0.4)",
            height: rect.height,
            left: rect.x,
            top: rect.y,
            width: rect.width,
          }}
        />
      ) : (
        <div className="pointer-events-none absolute inset-0 bg-black/40" />
      )}
      {!rect && (
        <div className="pointer-events-none absolute top-8 left-1/2 -translate-x-1/2 rounded-md bg-black/60 px-3 py-1.5 text-sm text-white">
          {t("hint")}
        </div>
      )}
    </div>
  );
};

export default Snip;
