<template>
  <n-config-provider>
    <n-message-provider>
      <n-dialog-provider>
        <n-layout style="height: 100vh">
          <n-layout-header bordered style="padding: 10px 20px; display: flex; align-items: center; gap: 20px">
            <strong>友盟 DAU 批量执行客户端</strong>
            <n-menu mode="horizontal" :options="menuOptions" :value="activeKey" @update:value="onMenu" style="flex: 1" />
            <n-tag v-if="app.gate1Passed" type="success" size="small">GATE 1 已通过</n-tag>
          </n-layout-header>
          <n-layout-content style="padding: 16px 20px; overflow: auto">
            <router-view />
          </n-layout-content>
        </n-layout>
      </n-dialog-provider>
    </n-message-provider>
  </n-config-provider>
</template>

<script setup lang="ts">
import { computed, h, onMounted } from "vue";
import { useRoute, useRouter } from "vue-router";
import { NConfigProvider, NDialogProvider, NLayout, NLayoutContent, NLayoutHeader, NMenu, NMessageProvider, NTag } from "naive-ui";
import { useAppStore } from "./stores/app";

import { api } from "./api/ipc";

const route = useRoute();
const router = useRouter();
const app = useAppStore();

const activeKey = computed(() => route.path);
const menuOptions = [
  { label: "初始化", key: "/setup" },
  { label: "环境预检", key: "/" },
  { label: "试点验证", key: "/pilot" },
  { label: "放量执行", key: "/batch" },
  { label: "报告", key: "/report/latest" },
];

function onMenu(key: string) {
  router.push(key);
}

onMounted(async () => {
  try {
    const count = await api.cleanupOrphans();
    if (count > 0) {
      console.log(`[AppInit] 自动清理上次异常中断残留的 ${count} 个 AVD`);
    }
  } catch {
    /* 忽略 SDK 未准备就绪时的错误 */
  }
  await app.refreshSetup();
  // 组件未就绪时强制进入初始化向导
  if (app.setupReport && app.setupReport.components.some((c) => c.state.kind !== "Ready")) {
    router.push("/setup");
  }
});
</script>
