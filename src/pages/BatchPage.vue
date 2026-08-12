<template>
  <div style="max-width: 1100px; margin: 0 auto">
    <n-h2>放量执行（Phase 2）</n-h2>

    <n-card title="参数" size="small">
      <div style="margin-bottom: 12px">
        <ApkDropZone @drop="onApkDrop" />
      </div>
      <n-form label-placement="left" label-width="120">
        <n-form-item label="测试 APK">
          <n-input v-model:value="app.config.apk_path" placeholder="拖入或粘贴 APK 路径" @blur="inspectApk" />
        </n-form-item>
        <n-form-item label="包名">
          <n-input v-model:value="app.config.pkg" placeholder="com.xxx.app（解析 APK 自动填充）" />
        </n-form-item>
        <n-form-item label="声明的测试 AppKey">
          <n-input v-model:value="declaredAppkey" @blur="inspectApk" />
        </n-form-item>
        <n-form-item v-if="apkInfo" label="APK 校验">
          <n-space vertical>
            <n-tag :type="apkInfo.appkey_matches_declared ? 'success' : 'error'">
              AppKey {{ apkInfo.appkey_matches_declared ? "一致" : "不一致" }}：{{ apkInfo.appkey || "未检出" }}
            </n-tag>
            <n-tag v-if="apkInfo.blacklist_hit" type="error">命中生产 AppKey 黑名单，已阻断</n-tag>
            <n-tag :type="apkInfo.debuggable ? 'success' : 'warning'">
              {{ apkInfo.debuggable ? "debug 包（标识可免 root 提取）" : "非 debug 包（标识提取需 root 兜底）" }}
            </n-tag>
          </n-space>
        </n-form-item>
        <n-grid :cols="3" :x-gap="16">
          <n-gi>
            <n-form-item label="目标数量">
              <n-input-number v-model:value="app.config.count" :min="1" :max="1000" />
            </n-form-item>
          </n-gi>
          <n-gi>
            <n-form-item label="并发（内存推荐）">
              <n-input-number v-model:value="app.config.concurrency" :min="1" :max="8" />
            </n-form-item>
          </n-gi>
          <n-gi>
            <n-form-item label="重置级别">
              <n-select v-model:value="app.config.reset_level" :options="levelOptions" />
            </n-form-item>
          </n-gi>
        </n-grid>
        <n-form-item label="耗时预估">
          <n-text>{{ estText }}</n-text>
        </n-form-item>
      </n-form>

      <n-alert v-if="crossesMidnight" type="warning">
        预计跨零点完成，DAU 按北京时间自然日切分，建议改期或分批。
      </n-alert>

      <div style="margin-top: 12px; display: flex; gap: 12px">
        <n-button type="primary" size="large" :disabled="!canStart" @click="start">
          开始放量
        </n-button>
        <n-button v-if="batch.running" type="error" size="large" @click="stop">停止（将清理设备）</n-button>
      </div>
      <n-text v-if="!app.gate1Passed" type="warning" style="display: block; margin-top: 8px; font-size: 12px">
        提示：GATE 1 未通过也可以试跑，但建议先完成试点验证
      </n-text>
    </n-card>

    <n-card v-if="batch.runId" title="看板" size="small" style="margin-top: 16px">
      <n-space align="center" size="large">
        <n-progress type="circle" :percentage="progressPct" :status="batch.done ? 'success' : 'default'">
          <span style="font-size: 13px">{{ batch.ok + batch.fail }}/{{ app.config.count }}</span>
        </n-progress>
        <n-statistic label="成功" :value="batch.ok" />
        <n-statistic label="失败" :value="batch.fail" />
        <n-statistic label="友盟 CONNECT 命中" :value="batch.umengHits" />
        <n-button v-if="batch.done" type="primary" @click="$router.push(`/report/${batch.runId}`)">
          查看报告
        </n-button>
      </n-space>

      <n-grid :cols="8" :x-gap="8" :y-gap="8" style="margin-top: 16px">
        <n-gi v-for="[idx, d] in [...batch.devices].sort((a, b) => a[0] - b[0])" :key="idx">
          <n-tooltip>
            <template #trigger>
              <div class="dev-cell" :class="d.status">#{{ idx }}</div>
            </template>
            <div style="max-width: 300px; font-size: 12px">
              <div>状态：{{ d.status }}　耗时：{{ d.duration_s.toFixed(1) }}s</div>
              <div>ANDROID_ID：{{ d.android_id || "—" }}</div>
              <div>UMID：{{ d.umid || "—" }}</div>
              <div v-if="d.error" style="color: #ff8888">{{ d.error }}</div>
            </div>
          </n-tooltip>
        </n-gi>
      </n-grid>
    </n-card>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import {
  NAlert, NButton, NCard, NForm, NFormItem, NGi, NGrid, NH2,
  NInput, NInputNumber, NProgress, NSelect, NSpace, NStatistic, NText, NTooltip, useMessage,
} from "naive-ui";
import { api } from "../api/ipc";
import { onBatchDevice, onBatchProgress } from "../api/events";
import { useAppStore } from "../stores/app";
import { useBatchStore } from "../stores/batch";
import ApkDropZone from "../components/ApkDropZone.vue";
import type { ApkInfo } from "../api/types";

const app = useAppStore();
const batch = useBatchStore();
const message = useMessage();
const declaredAppkey = ref("");
const apkInfo = ref<ApkInfo | null>(null);

const levelOptions = [
  { label: "L1 · pm clear（~3s/台）", value: "L1" },
  { label: "L2 · 卸载重装（~15s/台）", value: "L2" },
  { label: "L2.5 · 多用户（~15s/台）", value: "L25" },
  { label: "L3 · 恢复出厂（~100s/台，已验证）", value: "L3" },
];

const perDevice = computed(() => {
  const base = { L1: 3, L2: 15, L25: 15, L3: 100 }[app.config.reset_level];
  return base + app.config.dwell_s + app.config.flush_dwell_s;
});
const totalMin = computed(() =>
  Math.ceil((app.config.count * perDevice.value) / app.config.concurrency / 60)
);
const estText = computed(
  () => `约 ${totalMin.value} 分钟（${perDevice.value}s/台 × ${app.config.count} 台 ÷ 并发 ${app.config.concurrency}）`
);
const crossesMidnight = computed(() => app.preflightReport?.crosses_midnight ?? false);
const progressPct = computed(() =>
  app.config.count ? Math.round(((batch.ok + batch.fail) / app.config.count) * 100) : 0
);
const canStart = computed(
  () =>
    app.config.apk_path &&
    app.config.pkg &&
    !batch.running &&
    !(apkInfo.value?.blacklist_hit)
);

async function inspectApk() {
  if (!app.config.apk_path) return;
  try {
    apkInfo.value = await api.inspectApk(app.config.apk_path, declaredAppkey.value);
    if (apkInfo.value.pkg && !app.config.pkg) app.config.pkg = apkInfo.value.pkg;
  } catch (e: any) {
    message.warning("APK 解析失败（aapt 可能未安装）: " + e);
  }
}

async function onApkDrop(path: string) {
  app.config.apk_path = path;
  await inspectApk();
  if (apkInfo.value?.pkg) {
    message.success(`已加载：${apkInfo.value.pkg}`);
  }
}

async function start() {
  const id = await api.batchStart(app.config);
  batch.reset(id);
  message.success(`已启动 ${id}`);
}

async function stop() {
  await api.batchStop(batch.runId);
  batch.running = false;
}

onMounted(async () => {
  await onBatchDevice((d) => batch.onDevice(d));
  await onBatchProgress((p) => batch.onProgress(p));
});
</script>

<style scoped>
.dev-cell {
  height: 34px;
  border-radius: 6px;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 12px;
  background: #e8e8e8;
  color: #666;
}
.dev-cell.ok { background: #d3f0dd; color: #18a058; }
.dev-cell.fail { background: #fbe0e3; color: #d03050; }
</style>
