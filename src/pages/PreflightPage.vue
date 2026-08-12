<template>
  <div style="max-width: 860px; margin: 0 auto">
    <n-h2>环境预检（Phase 0）</n-h2>

    <n-card title="测试包" size="small" style="margin-bottom: 16px">
      <ApkDropZone @drop="onApkDrop" />

      <div v-if="app.config.apk_path" style="margin-top: 12px; display: flex; flex-direction: column; gap: 6px">
        <div class="apk-row">
          <span class="apk-label">APK 路径</span>
          <n-text style="word-break: break-all; font-size: 12px; flex: 1">{{ app.config.apk_path }}</n-text>
        </div>
        <div class="apk-row">
          <span class="apk-label">包名</span>
          <n-tag v-if="app.config.pkg" size="small" type="info">{{ app.config.pkg }}</n-tag>
          <n-text v-else depth="3" style="font-size: 12px">未解析</n-text>
        </div>
        <div v-if="apkInfo" class="apk-row">
          <span class="apk-label">AppKey</span>
          <n-tag v-if="apkInfo.appkey" size="small" :type="apkInfo.blacklist_hit ? 'error' : 'default'">
            {{ apkInfo.appkey }}{{ apkInfo.blacklist_hit ? "（黑名单）" : "" }}
          </n-tag>
          <n-text v-else depth="3" style="font-size: 12px">未检出</n-text>
        </div>
        <div v-if="apkInfo" class="apk-row">
          <span class="apk-label">debuggable</span>
          <n-tag :type="apkInfo.debuggable ? 'success' : 'warning'" size="small">
            {{ apkInfo.debuggable ? "是（标识可免 root 提取）" : "否（标识提取需 root 兜底）" }}
          </n-tag>
        </div>
        <div v-if="apkInfo && apkInfo.min_sdk" class="apk-row">
          <span class="apk-label">minSdk</span>
          <n-tag size="small">{{ apkInfo.min_sdk }}</n-tag>
        </div>
      </div>
    </n-card>

    <n-spin :show="checking">
      <div style="border: 1px solid #e0e0e6; border-radius: 4px; background: #fff; overflow: hidden">
        <div
          v-for="(item, idx) in app.preflightReport?.items ?? []"
          :key="item.id"
          :style="{
            display: 'flex',
            alignItems: 'flex-start',
            gap: '12px',
            padding: '10px 14px',
            borderBottom: idx < (app.preflightReport?.items.length ?? 0) - 1 ? '1px solid #f0f0f0' : 'none',
          }"
        >
          <span
            :style="{
              flexShrink: 0,
              width: '18px',
              height: '18px',
              lineHeight: '18px',
              textAlign: 'center',
              borderRadius: '50%',
              color: '#fff',
              fontSize: '12px',
              background: item.ok ? '#18a058' : '#d03050',
              marginTop: '2px',
            }"
          >{{ item.ok ? "✓" : "✗" }}</span>
          <div style="flex: 1; minWidth: 0">
            <div style="font-weight: 500; line-height: 1.4">{{ item.label }}</div>
            <div style="font-size: 12px; color: #888; margin-top: 2px; line-height: 1.4; word-break: break-all">{{ item.detail }}</div>
          </div>
          <div
            v-if="!item.ok"
            style="flex-shrink: 0; width: 200px; font-size: 12px; color: #d03050; line-height: 1.4; margin-top: 2px"
          >{{ item.fix_hint }}</div>
        </div>
      </div>
    </n-spin>

    <n-alert v-if="app.preflightReport?.crosses_midnight" type="warning" style="margin-top: 16px">
      按当前 COUNT × 标定耗时估算，预计 {{ app.preflightReport.est_finish }} 完成——将跨零点，
      DAU 按北京时间自然日切分，建议改期或分批。
    </n-alert>

    <div style="margin-top: 20px; display: flex; gap: 12px">
      <n-button @click="recheck" :loading="checking">重新检查</n-button>
      <n-button @click="cleanup" secondary>清理残留 AVD</n-button>
      <n-button type="primary" :disabled="!app.preflightReport?.all_green" @click="$router.push('/pilot')">
        进入试点验证
      </n-button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref } from "vue";
import { NAlert, NButton, NCard, NH2, NSpin, NTag, NText, useMessage } from "naive-ui";
import ApkDropZone from "../components/ApkDropZone.vue";
import { api } from "../api/ipc";
import { useAppStore } from "../stores/app";
import type { ApkInfo } from "../api/types";

const app = useAppStore();
const message = useMessage();
const checking = ref(false);
const apkInfo = ref<ApkInfo | null>(null);

async function onApkDrop(path: string) {
  app.config.apk_path = path;
  try {
    apkInfo.value = await api.inspectApk(path, "");
    if (apkInfo.value.pkg) app.config.pkg = apkInfo.value.pkg;
    message.success(`已解析：${apkInfo.value.pkg || "未知包名"}`);
  } catch (e: any) {
    apkInfo.value = null;
    message.warning("APK 解析失败（aapt 可能未就绪）: " + e);
  }
  // 拖入后立刻重新预检，APK 校验项会转绿/红
  await recheck();
}

async function recheck() {
  checking.value = true;
  try {
    await app.runPreflight();
  } finally {
    checking.value = false;
  }
}

async function cleanup() {
  const n = await api.cleanupOrphans();
  message.info(`已清理 ${n} 个残留 AVD（仅客户端创建的 dau-* 设备）`);
}

onMounted(async () => {
  // 已有 apk_path 时回填解析结果
  if (app.config.apk_path && !apkInfo.value) {
    try {
      apkInfo.value = await api.inspectApk(app.config.apk_path, "");
      if (apkInfo.value.pkg && !app.config.pkg) app.config.pkg = apkInfo.value.pkg;
    } catch {
      /* 忽略，预检照常跑 */
    }
  }
  await recheck();
});
</script>

<style scoped>
.apk-row {
  display: flex;
  align-items: center;
  gap: 10px;
}
.apk-label {
  flex-shrink: 0;
  width: 70px;
  font-size: 12px;
  color: #888;
}
</style>
