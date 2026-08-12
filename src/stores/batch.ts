import { defineStore } from "pinia";
import { computed, ref } from "vue";
import type { DeviceResult } from "../api/types";

export const useBatchStore = defineStore("batch", () => {
  const runId = ref<string>("");
  const running = ref(false);
  const done = ref(false);
  const devices = ref<Map<number, DeviceResult>>(new Map());
  const umengHits = ref(0);

  const ok = computed(() => [...devices.value.values()].filter((d) => d.status === "ok").length);
  const fail = computed(() => [...devices.value.values()].filter((d) => d.status === "fail").length);

  function onDevice(d: DeviceResult) {
    devices.value.set(d.index, d);
    devices.value = new Map(devices.value);
  }

  function onProgress(p: { ok: number; fail: number; done: boolean; umeng_hits?: number }) {
    if (p.umeng_hits !== undefined) umengHits.value = p.umeng_hits;
    if (p.done) {
      running.value = false;
      done.value = true;
    }
  }

  function reset(id: string) {
    runId.value = id;
    running.value = true;
    done.value = false;
    devices.value = new Map();
    umengHits.value = 0;
  }

  return { runId, running, done, devices, ok, fail, umengHits, onDevice, onProgress, reset };
});
