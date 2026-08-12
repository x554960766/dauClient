<template>
  <div style="max-width: 1000px; margin: 0 auto">
    <n-h2>结果报告（Phase 3）</n-h2>

    <n-space style="margin-bottom: 16px">
      <n-select v-model:value="selectedRun" :options="runOptions" placeholder="选择运行批次" style="width: 280px" />
      <n-button :disabled="!selectedRun" @click="exportMd">导出 Markdown 报告</n-button>
    </n-space>

    <n-card title="T+1 逐台对账" size="small">
      <n-alert type="info" style="margin-bottom: 12px">
        次日友盟 DAU 更新后，把后台导出的设备标识列表粘贴到下方，客户端做逐台差集——
        不只告诉你「少了 N 台」，还告诉你是哪 N 台。差值即过滤率结论。
      </n-alert>
      <n-input
        v-model:value="backendList"
        type="textarea"
        :rows="5"
        placeholder="粘贴友盟后台导出的设备标识列表（UMID 或 ANDROID_ID，每行一个或逗号分隔）"
      />
      <n-button style="margin-top: 8px" :disabled="!selectedRun || !backendList" @click="reconcile">
        开始对账
      </n-button>

      <template v-if="report">
        <n-divider />
        <n-space size="large">
          <n-statistic label="客户端成功设备" :value="report.client_count" />
          <n-statistic label="后台设备数" :value="report.backend_count" />
          <n-statistic label="过滤率 %" :value="report.filter_rate_pct" />
        </n-space>
        <n-table
          v-if="report.missing_in_backend.length"
          size="small"
          style="margin-top: 12px"
          :data="report.missing_in_backend"
          :columns="missingColumns"
        />
      </template>
    </n-card>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { useRoute } from "vue-router";
import {
  NAlert, NButton, NCard, NDivider, NH2, NInput, NSelect, NSpace, NStatistic, NTable, useMessage,
} from "naive-ui";
import { api } from "../api/ipc";
import type { ReconcileReport } from "../api/types";

const route = useRoute();
const message = useMessage();
const runs = ref<string[]>([]);
const selectedRun = ref<string>("");
const backendList = ref("");
const report = ref<ReconcileReport | null>(null);

const runOptions = computed(() => runs.value.map((r) => ({ label: r, value: r })));
const missingColumns = [
  { title: "#", key: "index" },
  { title: "ANDROID_ID", key: "android_id" },
  { title: "UMID", key: "umid" },
  { title: "代理命中", key: "proxy_hits" },
  { title: "耗时(s)", key: "duration_s" },
];

async function exportMd() {
  const path = `${selectedRun.value}-report.md`;
  await api.exportReport(selectedRun.value, path);
  message.success(`已导出 ${path}`);
}

async function reconcile() {
  report.value = await api.reconcileTPlus1(selectedRun.value, backendList.value);
}

onMounted(async () => {
  runs.value = await api.listRuns();
  const param = route.params.id as string;
  selectedRun.value = param && param !== "latest" ? param : runs.value[0] ?? "";
});
</script>
