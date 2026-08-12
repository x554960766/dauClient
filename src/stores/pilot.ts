import { defineStore } from "pinia";
import { ref } from "vue";
import type { PilotEvidence } from "../api/types";

export interface LogEntry {
  device: string;
  line: string;
  tags: string[];
}

export const usePilotStore = defineStore("pilot", () => {
  const state = ref<string>("Idle");
  const failedDetail = ref<string>("");
  const logs = ref<LogEntry[]>([]);
  const evidence = ref<PilotEvidence | null>(null);
  /// 标定试验历史：每级的标识结果
  const trials = ref<{ level: string; identity: PilotEvidence }[]>([]);

  function pushLog(l: LogEntry) {
    logs.value.push(l);
    if (logs.value.length > 2000) logs.value.splice(0, 500);
  }

  function setEvidence(e: PilotEvidence) {
    evidence.value = e;
    if (e.trial_level) {
      trials.value.push({ level: e.trial_level, identity: e });
    }
  }

  function setFailed(detail: string) {
    state.value = "Failed";
    failedDetail.value = detail;
  }

  function reset() {
    state.value = "Idle";
    failedDetail.value = "";
    logs.value = [];
    evidence.value = null;
    trials.value = [];
  }

  return { state, failedDetail, logs, evidence, trials, pushLog, setEvidence, setFailed, reset };
});
