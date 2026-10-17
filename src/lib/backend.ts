/**
 * 桌面版走 Tauri，浏览器版走同一组 HTTP 命令。
 *
 * 调用形状和 `invoke` / `listen` 对齐，界面状态不必知道自己跑在哪。
 */

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

const EVENT_CHANNEL = "watcher://event";
const NOTICE_CHANNEL = "watcher://notice";
const OPEN_BAG_CHANNEL = "watcher://open-bag";

type Handler = (payload: unknown) => void;

const channels = new Map<string, Set<Handler>>();
let source: EventSource | null = null;
let alertAudio: HTMLAudioElement | null = null;

export function isWeb(): boolean {
  return typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window);
}

/** 在用户点击「开始」时调用，换取之后自动播放提示音的权限。 */
export function prepareAlert(): void {
  if (!isWeb()) return;
  alertAudio ??= new Audio("/alert.mp3");
  alertAudio.preload = "auto";
}

export function playAlert(): void {
  if (!alertAudio) prepareAlert();
  if (!alertAudio) return;
  alertAudio.currentTime = 0;
  void alertAudio.play().catch(() => {
    // 浏览器还没把这次播放算作用户手势时会拒绝，下一轮有货再试。
  });
}

export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!isWeb()) return tauriInvoke<T>(command, args);
  if (command === "check_for_update") return null as T;
  if (command === "install_update") {
    throw new Error("網頁版請重新建置並啟動容器來更新");
  }
  return webInvoke<T>(command, args ?? {});
}

export async function listen<T>(
  channel: string,
  handler: (event: { payload: T }) => void,
): Promise<() => void> {
  if (!isWeb()) {
    const unlisten = await tauriListen<T>(channel, handler);
    return () => {
      unlisten();
    };
  }
  ensureSource();
  const wrapped: Handler = (payload) => handler({ payload: payload as T });
  const set = channels.get(channel) ?? new Set<Handler>();
  set.add(wrapped);
  channels.set(channel, set);
  return () => {
    set.delete(wrapped);
  };
}

async function webInvoke<T>(command: string, args: Record<string, unknown>): Promise<T> {
  switch (command) {
    case "list_regions":
      return request<T>("/api/regions");
    case "list_categories":
      return request<T>("/api/categories");
    case "list_stores":
      return request<T>(`/api/stores?locale=${encodeURIComponent(String(args.locale ?? ""))}`);
    case "list_products":
      return request<T>(`/api/products?locale=${encodeURIComponent(String(args.locale ?? ""))}`);
    case "refresh_products":
      return request<T>("/api/products/refresh", { method: "POST", body: args });
    case "get_settings":
      return request<T>("/api/settings");
    case "save_settings":
      return request<T>("/api/settings", { method: "PUT", body: args.settings });
    case "set_push_urls":
      return request<T>("/api/push-urls", { method: "PUT", body: args });
    case "get_snapshot":
      return request<T>("/api/snapshot");
    case "set_targets":
      return request<T>("/api/targets", { method: "PUT", body: args });
    case "set_interval":
      return request<T>("/api/interval", { method: "PUT", body: args });
    case "start_watching":
      return request<T>("/api/watch/start", { method: "POST" });
    case "stop_watching":
      return request<T>("/api/watch/stop", { method: "POST" });
    case "is_running":
      return request<T>("/api/running");
    case "test_notify":
      return request<T>("/api/notify/test", { method: "POST" });
    default:
      throw new Error(`網頁版沒有這個指令：${command}`);
  }
}

async function request<T>(
  path: string,
  init?: { method?: string; body?: unknown },
): Promise<T> {
  const response = await fetch(path, {
    method: init?.method ?? "GET",
    headers: init?.body === undefined ? undefined : { "content-type": "application/json" },
    body: init?.body === undefined ? undefined : JSON.stringify(init.body),
  });
  if (response.status === 204) return undefined as T;
  const text = await response.text();
  const payload: unknown = text ? JSON.parse(text) : null;
  if (!response.ok) {
    const message =
      payload && typeof payload === "object" && "error" in payload
        ? String((payload as { error: unknown }).error)
        : response.statusText;
    throw new Error(message);
  }
  return payload as T;
}

function ensureSource(): void {
  if (source) return;
  source = new EventSource("/api/events");
  const bind = (name: string, channel: string) => {
    source?.addEventListener(name, (event) => {
      const data = (event as MessageEvent<string>).data;
      let payload: unknown = data;
      try {
        payload = JSON.parse(data);
      } catch {
        // 保留原文。正常路径都是 JSON。
      }
      for (const handler of channels.get(channel) ?? []) handler(payload);
    });
  };
  bind("watcher", EVENT_CHANNEL);
  bind("notice", NOTICE_CHANNEL);
  bind("open-bag", OPEN_BAG_CHANNEL);
}
