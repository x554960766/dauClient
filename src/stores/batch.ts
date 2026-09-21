import { defineStore } from "pinia";
import { computed, ref } from "vue";
import type { DeviceResult } from "../api/types";

export const useBatchStore = defineStore("batch", () => {
  const runId = ref<string>("");
  const running = ref(false);
  const done = ref(false);
  const devices = ref<Map<number, DeviceResult>>(new Map());
  const umengHits = ref(0);
  const blockedHits = ref(0);
  const totalTraffic = ref<string>("");
  const umengTraffic = ref<string>("");
  const currentIp = ref<string>("");
  const rotatingIp = ref<boolean>(false);
  const rotateMessage = ref<string>("");
  const waitingWindow = ref<boolean>(false);
  const windowMessage = ref<string>("");

  const ok = computed(() => [...devices.value.values()].filter((d) => d.status === "ok").length);
  const fail = computed(() => [...devices.value.values()].filter((d) => d.status === "fail").length);

  function onDevice(d: DeviceResult) {
    devices.value.set(d.index, d);
    devices.value = new Map(devices.value);
  }

  function onProgress(p: { ok: number; fail: number; done: boolean; umeng_hits?: number; blocked_hits?: number; total_traffic?: string; umeng_traffic?: string }) {
    if (p.umeng_hits !== undefined) umengHits.value = p.umeng_hits;
    if (p.blocked_hits !== undefined) blockedHits.value = p.blocked_hits;
    if (p.total_traffic !== undefined) totalTraffic.value = p.total_traffic;
    if (p.umeng_traffic !== undefined) umengTraffic.value = p.umeng_traffic;
    if (p.done) {
      running.value = false;
      done.value = true;
      rotatingIp.value = false;
      waitingWindow.value = false;
    }
  }

  function onIpStatus(p: { rotating: boolean; message: string; ip?: string }) {
    rotatingIp.value = p.rotating;
    rotateMessage.value = p.message || "";
    if (p.ip) {
      currentIp.value = p.ip;
    }
  }

  function onTimeWindowStatus(p: { waiting: boolean; start: string; end: string; message: string }) {
    waitingWindow.value = p.waiting;
    windowMessage.value = p.message || "";
  }

  function reset(id: string) {
    runId.value = id;
    running.value = true;
    done.value = false;
    devices.value = new Map();
    umengHits.value = 0;
    blockedHits.value = 0;
    currentIp.value = "";
    rotatingIp.value = false;
    rotateMessage.value = "";
    waitingWindow.value = false;
    windowMessage.value = "";
  }

  return {
    runId,
    running,
    done,
    devices,
    ok,
    fail,
    umengHits,
    blockedHits,
    totalTraffic,
    umengTraffic,
    currentIp,
    rotatingIp,
    rotateMessage,
    waitingWindow,
    windowMessage,
    onDevice,
    onProgress,
    onIpStatus,
    onTimeWindowStatus,
    reset,
  };
});
