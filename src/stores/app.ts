import { defineStore } from "pinia";
import { ref } from "vue";
import { api, defaultConfig } from "../api/ipc";
import type { EngineConfig, PreflightReport, SetupReport } from "../api/types";

export const useAppStore = defineStore("app", () => {
  const setupReport = ref<SetupReport | null>(null);
  const preflightReport = ref<PreflightReport | null>(null);
  const config = ref<EngineConfig>(defaultConfig());
  const gate1Passed = ref(false);
  const calibratedLevel = ref<string>("L3");

  async function refreshSetup() {
    setupReport.value = await api.setupStatus();
    if (setupReport.value && !config.value.system_image) {
      const arch = setupReport.value.arch === "arm64" ? "arm64-v8a" : "x86_64";
      config.value.system_image = `system-images;android-34;google_apis;${arch}`;
    }
  }

  async function runPreflight() {
    preflightReport.value = await api.preflightCheck(config.value);
    if (preflightReport.value) {
      config.value.concurrency = preflightReport.value.recommended_concurrency;
    }
  }

  return {
    setupReport,
    preflightReport,
    config,
    gate1Passed,
    calibratedLevel,
    refreshSetup,
    runPreflight,
  };
});
