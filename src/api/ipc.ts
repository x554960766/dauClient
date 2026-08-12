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
};

export function defaultConfig(): EngineConfig {
  return {
    apk_path: "",
    pkg: "",
    count: 50,
    concurrency: 2,
    reset_level: "L3",
    system_image: "",
    device_profile: "pixel_6",
    dwell_s: 10,
    flush_dwell_s: 8,
    boot_timeout_s: 180,
    emu_mem_mb: 2048,
    use_proxy: true,
    max_users: null,
  };
}
