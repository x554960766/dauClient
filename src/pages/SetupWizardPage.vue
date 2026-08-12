<template>
  <div style="max-width: 860px; margin: 0 auto">
    <n-h2>首次初始化</n-h2>
    <n-alert type="info" style="margin-bottom: 16px">
      客户端将自动下载 Android 工具链与系统镜像到私有目录（不污染系统环境、不影响 Android Studio）。
      首次下载约 2.1GB，支持断点续传。
    </n-alert>

    <n-card title="SDK 目录" size="small" style="margin-bottom: 16px">
      <n-text code>{{ app.setupReport?.sdk_dir }}</n-text>
      <div style="margin-top: 8px; font-size: 12px; color: #888">
        平台 {{ app.setupReport?.platform }} / {{ app.setupReport?.arch }} ·
        {{ app.setupReport?.hypervisor.detail }}
      </div>
    </n-card>

    <n-card title="组件清单" size="small">
      <n-list>
        <n-list-item v-for="c in app.setupReport?.components ?? []" :key="c.id">
          <div style="display: flex; align-items: center; gap: 12px">
            <n-tag :type="tagType(c.state.kind)" size="small" style="width: 90px; text-align: center">
              {{ stateText(c.state) }}
            </n-tag>
            <span style="flex: 1">{{ c.label }}</span>
            <span style="color: #999; font-size: 12px">~{{ c.size_hint_mb }}MB</span>
            <n-progress
              v-if="progress[c.id] !== undefined && c.state.kind !== 'Ready'"
              type="line"
              :percentage="progress[c.id]"
              style="width: 180px"
            />
          </div>
        </n-list-item>
      </n-list>
    </n-card>

    <div style="margin-top: 20px; display: flex; gap: 12px">
      <n-button type="primary" size="large" :loading="downloading" @click="start">
        {{ downloading ? "正在下载安装…" : "开始初始化" }}
      </n-button>
      <n-button size="large" @click="$router.push('/')" :disabled="!allReady">进入环境预检</n-button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { NAlert, NButton, NCard, NH2, NList, NListItem, NProgress, NTag, NText, useMessage } from "naive-ui";
import { api } from "../api/ipc";
import { onSetupProgress } from "../api/events";
import { useAppStore } from "../stores/app";

const app = useAppStore();
const message = useMessage();
const downloading = ref(false);
const progress = ref<Record<string, number>>({});

const allReady = computed(() =>
  app.setupReport?.components.every((c) => c.state.kind === "Ready")
);

function tagType(kind: string) {
  return kind === "Ready" ? "success" : kind === "Failed" ? "error" : "warning";
}
function stateText(s: { kind: string; detail?: any }) {
  if (s.kind === "Ready") return "就绪";
  if (s.kind === "Missing") return "未安装";
  if (s.kind === "Failed") return "失败";
  return s.kind;
}

async function start() {
  downloading.value = true;
  try {
    await api.setupDownload([34]);
    await app.refreshSetup();
    message.success("初始化完成");
  } catch (e: any) {
    message.error("初始化失败: " + e);
  } finally {
    downloading.value = false;
  }
}

onMounted(async () => {
  await app.refreshSetup();
  await onSetupProgress((p) => {
    progress.value[p.component] = p.pct;
  });
});
</script>
