import { invoke } from "@tauri-apps/api/core";
import type {
  ApkInfo,
  DeviceIdentity,
  EngineConfig,
  Gate1Answers,
  Gate1Verdict,
  PreflightReport,
  ReconcileReport,
  SetupReport,
  StackStatusReport,
} from "./types";

export const api = {
  setupStatus: () => invoke<SetupReport>("setup_status"),
  setupDownload: (apiLevels: number[]) =>
    invoke<void>("setup_download", { apiLevels }),
  windowsHypervisorSetup: () =>
    invoke<{ ready: boolean; detail: string }>("windows_hypervisor_setup"),

  preflightCheck: (config?: EngineConfig) =>
    invoke<PreflightReport>("preflight_check", { config }),
  inspectApk: (apkPath: string, declaredAppkey: string) =>
    invoke<ApkInfo>("inspect_apk", { apkPath, declaredAppkey }),

  pilotStart: (config: EngineConfig) => invoke<void>("pilot_start", { config }),
  pilotResetTrial: (config: EngineConfig, level: string) =>
    invoke<DeviceIdentity>("pilot_reset_trial", { config, level }),
  pilotGate1Submit: (answers: Gate1Answers, evidenceOk: boolean) =>
    invoke<Gate1Verdict>("pilot_gate1_submit", { answers, evidenceOk }),

  batchStart: (config: EngineConfig) => invoke<string>("batch_start", { config }),
  batchStop: (runId: string) => invoke<void>("batch_stop", { runId }),

  cleanupOrphans: () => invoke<number>("cleanup_orphans"),
  exportReport: (runId: string, savePath: string) =>
    invoke<void>("export_report", { runId, savePath }),
  reconcileTPlus1: (runId: string, backendList: string) =>
    invoke<ReconcileReport>("reconcile_t_plus_1", { runId, backendList }),
  listRuns: () => invoke<string[]>("list_runs"),

  getStackStatus: () => invoke<StackStatusReport>("get_stack_status"),
  resetProfileUsage: () => invoke<void>("reset_profile_usage"),

  detectUsbPhones: () => invoke<import("./types").UsbPhoneInfo[]>("detect_usb_phones"),
  getCurrentPublicIp: (serial?: string) => invoke<string | null>("get_current_public_ip", { serial }),
  testRotateIp: (params?: {
    serial?: string;
    disconnect_wait_s?: number;
    reconnect_wait_s?: number;
    hotspot_ssid?: string;
    hotspot_password?: string;
  }) => invoke<import("./types").RotateIpResult>("test_rotate_ip", params || {}),
};

export function defaultConfig(): EngineConfig {
  return {
    apk_path: "",
    pkg: "",
    count: 10,
    concurrency: 2,
    reset_level: "L3",
    system_image: "",
    device_profile: "pixel_6",
    dwell_s: 10,
    flush_dwell_s: 5,
    boot_timeout_s: 180,
    emu_mem_mb: 1280,
    use_proxy: true,
    max_users: null,
    stack_capacity: 300,
    l3_probability: 0.40,
    full_push_probability: 0.60,
    enable_stack_mode: true,
    auto_rotate_ip: false,
    rotate_ip_interval_min: 60,
    rotate_ip_interval_max: 100,
    rotate_ip_serial: null,
    rotate_ip_disconnect_wait_s: 4,
    rotate_ip_reconnect_wait_s: 6,
    rotate_ip_hotspot_ssid: "P70",
    rotate_ip_hotspot_password: "123456789",
    time_window_enabled: false,
    time_window_start: "08:00",
    time_window_end: "22:00",
  };
}
