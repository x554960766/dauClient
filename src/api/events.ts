import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export async function onSetupProgress(
  cb: (p: { component: string; pct: number; stage: string }) => void
): Promise<UnlistenFn> {
  return listen("setup://progress", (e) => cb(e.payload as any));
}

export async function onPilotState(
  cb: (p: { state: string; detail?: string }) => void
): Promise<UnlistenFn> {
  return listen("pilot://state", (e) => cb(e.payload as any));
}

export async function onPilotEvidence(cb: (p: any) => void): Promise<UnlistenFn> {
  return listen("pilot://evidence", (e) => cb(e.payload));
}

export async function onPilotLog(
  cb: (p: { device: string; line: string; tags: string[] }) => void
): Promise<UnlistenFn> {
  return listen("pilot://log", (e) => cb(e.payload as any));
}

export async function onBatchDevice(cb: (p: any) => void): Promise<UnlistenFn> {
  return listen("batch://device", (e) => cb(e.payload));
}

export async function onBatchProgress(
  cb: (p: { ok: number; fail: number; done: boolean; umeng_hits?: number; blocked_hits?: number }) => void
): Promise<UnlistenFn> {
  return listen("batch://progress", (e) => cb(e.payload as any));
}

export async function onBatchIpStatus(
  cb: (p: { rotating: boolean; message: string; ip?: string }) => void
): Promise<UnlistenFn> {
  return listen("batch://ip_status", (e) => cb(e.payload as any));
}

export async function onBatchTimeWindowStatus(
  cb: (p: { waiting: boolean; start: string; end: string; message: string }) => void
): Promise<UnlistenFn> {
  return listen("batch://time_window_status", (e) => cb(e.payload as any));
}
