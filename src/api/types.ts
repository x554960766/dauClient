// 与 Rust 侧 serde 类型一一对应

export interface Component {
  id: string;
  label: string;
  size_hint_mb: number;
  required: boolean;
  state: { kind: string; detail?: string | number };
}

export interface SetupReport {
  sdk_dir: string;
  platform: string;
  arch: string;
  components: Component[];
  hypervisor: { platform: string; ready: boolean; detail: string };
}

export interface EngineConfig {
  apk_path: string;
  pkg: string;
  count: number;
  concurrency: number;
  reset_level: "L1" | "L2" | "L25" | "L3";
  system_image: string;
  device_profile: string;
  dwell_s: number;
  flush_dwell_s: number;
  boot_timeout_s: number;
  emu_mem_mb: number;
  use_proxy: boolean;
  max_users: number | null;
}

export interface PreflightItem {
  id: string;
  label: string;
  ok: boolean;
  detail: string;
  fix_hint: string;
}

export interface PreflightReport {
  items: PreflightItem[];
  recommended_concurrency: number;
  all_green: boolean;
  est_finish: string;
  crosses_midnight: boolean;
}

export interface ApkInfo {
  pkg: string;
  appkey: string;
  debuggable: boolean;
  min_sdk: string;
  appkey_matches_declared: boolean | null;
  blacklist_hit: boolean;
}

export interface DeviceIdentity {
  android_id: string;
  umid: string;
  method: string;
}

export interface DeviceResult {
  index: number;
  slot: number;
  status: string;
  error: string | null;
  android_id: string;
  umid: string;
  proxy_hits: number;
  duration_s: number;
  reset_level: string;
  started_at: string;
}

export interface ReconcileReport {
  client_count: number;
  backend_count: number;
  missing_in_backend: DeviceResult[];
  extra_in_backend: string[];
  filter_rate_pct: number;
}

export interface Gate1Answers {
  backend_received: boolean;
  backend_device_count_increased: boolean;
  observed_device_count: number;
}

export interface Gate1Verdict {
  passed: boolean;
  branch: string;
  message: string;
}

export interface PilotEvidence {
  trial_level?: string;
  android_id: string;
  umid: string;
  identity_method: string;
  umeng_hits?: number;
  total_connects?: number;
  hosts?: Record<string, number>;
  // 试 Lx 增量命中（本次「重置→重跑」期间打到友盟的次数）
  trial_umeng_hits?: number;
  trial_total_connects?: number;
}
