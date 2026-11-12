import {
  type EventCallback,
  type EventName,
  listen,
} from "@tauri-apps/api/event";
import { useMount, useUnmount } from "ahooks";
import { useRef } from "react";
import { log } from "@/utils/log";

/**
 * 订阅 Tauri 事件，组件卸载时自动取消监听。
 * 等价于 `listen(event, handler)` + cleanup，消除样板代码。
 */
export const useTauriListen = <T>(
  event: EventName,
  handler: EventCallback<T>,
) => {
  const unlistenRef = useRef<(() => void) | undefined>(void 0);
  const disposedRef = useRef(false);
  const handlerRef = useRef(handler);
  handlerRef.current = handler;

  useMount(async () => {
    try {
      const unlisten = await listen<T>(event, (payload) => {
        handlerRef.current(payload);
      });

      // 订阅是异步的：组件可能在它完成之前就已卸载（快速切换页签、搜索结果刷新），
      // 那时卸载回调拿不到 unlisten。这里补上，否则监听永久留存并继续触发回调。
      if (disposedRef.current) {
        unlisten();
        return;
      }

      unlistenRef.current = unlisten;
    } catch (error) {
      log.error(`Failed to listen tauri event: ${event}`, error);
    }
  });

  useUnmount(() => {
    disposedRef.current = true;
    unlistenRef.current?.();
  });
};
