<template>
  <div style="max-width: 1100px; margin: 0 auto">
    <n-h2>试点验证（Phase 1）</n-h2>

    <n-grid :cols="2" :x-gap="16">
      <n-gi>
        <n-card title="执行状态" size="small">
          <n-steps vertical :current="stepIndex" size="small">
            <n-step v-for="s in steps" :key="s" :title="stepLabels[s] || s" />
          </n-steps>
        <n-button type="primary" style="margin-top: 12px" :disabled="running" @click="startPilot">
          {{ running ? "试点运行中…" : "开始试点" }}
        </n-button>
        <n-alert v-if="pilot.state === 'Failed'" type="error" style="margin-top: 12px" title="试点失败">
          {{ pilot.failedDetail || "未知错误（请查看 tauri dev 终端日志）" }}
        </n-alert>
      </n-card>

        <n-card title="上报硬证据（代理 CONNECT 计数）" size="small" style="margin-top: 16px">
          <n-statistic label="友盟域名命中次数" :value="pilot.evidence?.umeng_hits ?? 0" />
          <div style="font-size: 12px; color: #888; margin-top: 6px">
            总 CONNECT: {{ pilot.evidence?.total_connects ?? 0 }}（不解密、零证书）
          </div>
        </n-card>

        <n-card title="设备标识（去友盟后台集成测试填这个）" size="small" style="margin-top: 16px">
          <n-descriptions :column="1" size="small" bordered>
            <n-descriptions-item label="ANDROID_ID">
              {{ pilot.evidence?.android_id || "—" }}
              <n-button text type="primary" size="tiny" @click="copy(pilot.evidence?.android_id)">复制</n-button>
            </n-descriptions-item>
            <n-descriptions-item label="UMID">
              {{ pilot.evidence?.umid || "—" }}
              <n-button text type="primary" size="tiny" @click="copy(pilot.evidence?.umid)">复制</n-button>
            </n-descriptions-item>
            <n-descriptions-item label="提取方式">{{ pilot.evidence?.identity_method || "—" }}</n-descriptions-item>
          </n-descriptions>
          <n-button text type="primary" tag="a" href="https://www.umeng.com" target="_blank" style="margin-top: 8px">
            去友盟后台集成测试 ↗
          </n-button>
        </n-card>
      </n-gi>

      <n-gi>
        <n-card title="重置阶梯标定（逐级试，找最低可用级别）" size="small">
          <div style="display: flex; gap: 8px; margin-bottom: 12px">
            <n-button v-for="l in ['L1', 'L2', 'L25', 'L3']" :key="l" size="small"
              :disabled="pilot.state !== 'Gate1Review'" @click="trial(l)">
              试 {{ l === "L25" ? "L2.5(多用户)" : l }}
            </n-button>
          </div>
          <n-table size="small" :data="trialData" :columns="trialColumns" />
        </n-card>

        <n-card title="实时日志（logcat 辅助信号）" size="small" style="margin-top: 16px">
          <div ref="logBox" style="height: 260px; overflow: auto; font-family: monospace; font-size: 11px; background: #111; color: #ccc; padding: 8px; border-radius: 6px">
            <div v-for="(l, i) in pilot.logs" :key="i" :style="{ color: l.tags.length ? '#7ee787' : '#aaa' }">
              {{ l.line }}
            </div>
          </div>
        </n-card>
      </n-gi>
    </n-grid>

    <n-card title="GATE 1 人工确认（必须拿友盟后台证据，不能推测）" size="small" style="margin-top: 16px">
      <n-form inline>
        <n-form-item label="后台收到模拟器数据">
          <n-switch v-model:value="gate.backend_received" />
        </n-form-item>
        <n-form-item label="重置后设备数 +1">
          <n-switch v-model:value="gate.backend_device_count_increased" />
        </n-form-item>
        <n-form-item label="后台观察设备数">
          <n-input-number v-model:value="gate.observed_device_count" :min="0" style="width: 100px" />
        </n-form-item>
        <n-button type="primary" @click="submitGate">提交判定</n-button>
      </n-form>
      <n-alert v-if="verdict" :type="verdict.passed ? 'success' : 'error'" style="margin-top: 12px">
        {{ verdict.message }}
      </n-alert>
    </n-card>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, reactive, ref, watch } from "vue";
import {
  NAlert, NButton, NCard, NDescriptions, NDescriptionsItem, NForm, NFormItem, NGi, NGrid,
  NH2, NInputNumber, NStatistic, NStep, NSteps, NSwitch, NTable, useMessage,
} from "naive-ui";
import { api } from "../api/ipc";
import { onPilotEvidence, onPilotLog, onPilotState } from "../api/events";
import { useAppStore } from "../stores/app";
import { usePilotStore } from "../stores/pilot";
import type { Gate1Verdict } from "../api/types";

const app = useAppStore();
const pilot = usePilotStore();
const message = useMessage();
const logBox = ref<HTMLElement>();

const stepLabels: Record<string, string> = {
  CreatingPilot: "创建试点设备",
  Booting: "启动模拟器",
  NetWaiting: "等待网络",
  Installing: "安装 App",
  Launching: "启动 App",
  EvidenceCollect: "采集证据",
  Gate1Review: "GATE 1 待确认",
  "ResetTrial-L1": "试 L1：清除数据",
  "ResetTrial-L2": "试 L2：重装",
  "ResetTrial-L25": "试 L2.5：多用户",
  "ResetTrial-L3": "试 L3：恢复出厂",
};
const steps = [
  "CreatingPilot", "Booting", "NetWaiting", "Installing", "Launching", "EvidenceCollect", "Gate1Review",
  "ResetTrial-L1", "ResetTrial-L2", "ResetTrial-L25", "ResetTrial-L3",
];
const running = computed(() => pilot.state !== "Idle" && pilot.state !== "Gate1Review" && pilot.state !== "Failed");
const stepIndex = computed(() => Math.max(0, steps.indexOf(pilot.state)));

const gate = reactive({ backend_received: false, backend_device_count_increased: false, observed_device_count: 0 });
const verdict = ref<Gate1Verdict | null>(null);

const trialColumns = [
  { title: "级别", key: "level" },
  { title: "ANDROID_ID", key: "android_id" },
  { title: "UMID", key: "umid" },
  { title: "友盟命中", key: "umeng_hits", render: (r: any) => (r.umeng_hits > 0 ? `${r.umeng_hits} 次 ✓` : "0 ✗") },
];
const trialData = computed(() =>
  pilot.trials.map((t) => ({
    level: t.level,
    android_id: t.identity.android_id.slice(0, 12) + "…",
    umid: t.identity.umid ? t.identity.umid.slice(0, 12) + "…" : "—",
    umeng_hits: t.identity.trial_umeng_hits ?? 0,
  }))
);

async function startPilot() {
  pilot.reset();
  try {
    await api.pilotStart(app.config);
  } catch (e: any) {
    // 后端 pilot_run 返回 Err（某步失败），命令层把错误抛回来
    pilot.setFailed(String(e));
  }
}

async function trial(level: string) {
  try {
    await api.pilotResetTrial(app.config, level);
    message.success(`${level} 试验完成，对比标识是否变化`);
  } catch (e: any) {
    message.error(`${level} 试验失败: ` + e);
  }
}

async function submitGate() {
  const evidenceOk = (pilot.evidence?.umeng_hits ?? 0) > 0;
  verdict.value = await api.pilotGate1Submit(gate, evidenceOk);
  if (verdict.value.passed) {
    app.gate1Passed = true;
  }
}

function copy(text?: string) {
  if (text) navigator.clipboard.writeText(text);
}

watch(() => pilot.logs.length, async () => {
  await nextTick();
  logBox.value?.scrollTo(0, logBox.value.scrollHeight);
});

onMounted(async () => {
  await onPilotState((p) => {
    pilot.state = p.state;
    if (p.state === "Failed" && p.detail) pilot.failedDetail = p.detail;
  });
  await onPilotLog((l) => pilot.pushLog(l));
  await onPilotEvidence((e) => pilot.setEvidence(e));
});
</script>
