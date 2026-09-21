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
        <n-grid :cols="4" :x-gap="16">
          <n-gi>
            <n-form-item label="目标数量">
              <n-input-number v-model:value="app.config.count" :min="1" :max="1000" />
            </n-form-item>
          </n-gi>
          <n-gi>
            <n-form-item label="并发（多线程执行）">
              <n-input-number v-model:value="app.config.concurrency" :min="1" :max="8" />
              <template #feedback>
                <span style="font-size: 11px; color: #888">
                  推荐: {{ app.preflightReport?.recommended_concurrency || 4 }} 台并发（根据主机资源测算）
                </span>
              </template>
            </n-form-item>
          </n-gi>
          <n-gi>
            <n-form-item label="重置级别">
              <n-select v-model:value="app.config.reset_level" :options="levelOptions" />
            </n-form-item>
          </n-gi>
          <n-gi>
            <n-form-item label="分配/测试模式">
              <n-select v-model:value="allocationMode" :options="modeOptions" @update:value="onModeChange" />
            </n-form-item>
          </n-gi>
        </n-grid>

        <n-form-item label="执行时间范围">
          <n-space vertical style="width: 100%">
            <n-space align="center">
              <n-switch v-model:value="app.config.time_window_enabled" />
              <n-text style="font-weight: 500">只在设置的时间范围内执行（基于北京时间）</n-text>
              <template v-if="app.config.time_window_enabled">
                <n-time-picker
                  v-model:formatted-value="app.config.time_window_start"
                  value-format="HH:mm"
                  format="HH:mm"
                  size="small"
                  placeholder="起始时间"
                  style="width: 130px"
                />
                <span>至</span>
                <n-time-picker
                  v-model:formatted-value="app.config.time_window_end"
                  value-format="HH:mm"
                  format="HH:mm"
                  size="small"
                  placeholder="结束时间"
                  style="width: 130px"
                />
                <n-tag :type="isCurrentTimeInWindow ? 'success' : 'warning'" size="small">
                  {{ isCurrentTimeInWindow ? "当前在时间范围内（可立即执行）" : "当前不在时间范围内（启动后将自动等待进入窗口）" }}
                </n-tag>
              </template>
            </n-space>
            <n-text v-if="app.config.time_window_enabled" depth="3" style="font-size: 12px">
              提示：若放量执行过程中离开设定时间范围，任务将自动暂停调度挂起等待，直到再次进入时间窗口自动恢复，期间支持随时手动停止。
            </n-text>
          </n-space>
        </n-form-item>

        <n-form-item label="手机换 IP">
          <n-space vertical style="width: 100%">
            <n-space align="center">
              <n-switch v-model:value="app.config.auto_rotate_ip" @update:value="onRotateIpToggle" />
              <n-text style="font-weight: 500">后台平滑换 IP（每累计完成 60~100 台随机触发，流水线无感过渡）</n-text>
              <n-button size="tiny" secondary :loading="refreshingPhones" @click="refreshPhones">
                刷新手机
              </n-button>
              <n-button size="tiny" type="info" secondary :loading="testingIp" @click="manualTestRotateIp">
                测试单次换 IP
              </n-button>
              <n-tooltip trigger="hover">
                <template #trigger>
                  <n-tag
                    v-if="phonePublicIp"
                    type="success"
                    size="small"
                    :bordered="false"
                    style="font-family: monospace; font-weight: 600; cursor: pointer"
                    @click="fetchCurrentPhoneIp"
                  >
                    📱 手机公网 IP: {{ phonePublicIp }} 🔄
                  </n-tag>
                  <n-tag
                    v-else-if="fetchingPhoneIp"
                    type="info"
                    size="small"
                    :bordered="false"
                  >
                    正在向手机探测公网 IP...
                  </n-tag>
                  <n-tag
                    v-else
                    type="default"
                    size="small"
                    :bordered="false"
                    style="cursor: pointer"
                    @click="fetchCurrentPhoneIp"
                  >
                    点击获取手机公网 IP
                  </n-tag>
                </template>
                通过已连接的 USB 手机蜂窝网络直接探测到的真实公网 IP（彻底隔离电脑本地 VPN/代理工具），点击可重新检测
              </n-tooltip>
            </n-space>

            <div v-if="app.config.auto_rotate_ip" style="background: rgba(0,0,0,0.02); padding: 8px 12px; border-radius: 6px; border: 1px dashed #d9d9d9">
              <n-space align="center" style="margin-bottom: 8px">
                <span style="font-size: 13px">已识别真机：</span>
                <n-tag v-if="detectedPhones.length > 0" type="success" size="small">
                  {{ detectedPhones.map(p => `${p.model} (${p.serial})`).join('、') }}
                </n-tag>
                <n-tag v-else type="warning" size="small">
                  未检测到 USB 连接的安卓真机（请检查 USB 连线、调试权限与网络共享）
                </n-tag>
              </n-space>

              <n-grid :cols="2" :x-gap="12" style="margin-bottom: 8px">
                <n-gi>
                  <n-form-item label="换 IP 间隔下限 (最小台数)" :show-feedback="false">
                    <n-input-number
                      v-model:value="app.config.rotate_ip_interval_min"
                      :min="1"
                      :max="app.config.rotate_ip_interval_max || 100"
                      placeholder="默认 60"
                      size="small"
                      style="width: 100%"
                    />
                  </n-form-item>
                </n-gi>
                <n-gi>
                  <n-form-item label="换 IP 间隔上限 (最大台数)" :show-feedback="false">
                    <n-input-number
                      v-model:value="app.config.rotate_ip_interval_max"
                      :min="app.config.rotate_ip_interval_min || 1"
                      placeholder="默认 100"
                      size="small"
                      style="width: 100%"
                    />
                  </n-form-item>
                </n-gi>
              </n-grid>

              <n-grid :cols="2" :x-gap="12" style="margin-bottom: 8px">
                <n-gi>
                  <n-form-item label="手机热点名称 (SSID)" :show-feedback="false">
                    <n-input
                      v-model:value="app.config.rotate_ip_hotspot_ssid"
                      placeholder="如: P70 (Mac 自动强连，留空则不强制)"
                      size="small"
                    />
                  </n-form-item>
                </n-gi>
                <n-gi>
                  <n-form-item label="手机热点密码" :show-feedback="false">
                    <n-input
                      v-model:value="app.config.rotate_ip_hotspot_password"
                      type="password"
                      show-password-on="click"
                      placeholder="如: 123456789"
                      size="small"
                    />
                  </n-form-item>
                </n-gi>
              </n-grid>

              <n-space align="center">
                <n-text depth="3" style="font-size: 12px">
                  随机间隔: 累计每 {{ app.config.rotate_ip_interval_min || 60 }} ~ {{ app.config.rotate_ip_interval_max || 100 }} 台换一次 ｜ 断网保持: {{ app.config.rotate_ip_disconnect_wait_s || 4 }}s ｜ 恢复等待: {{ app.config.rotate_ip_reconnect_wait_s || 6 }}s ｜ 虚拟机冷启动完全不阻塞，平滑无感
                </n-text>
              </n-space>
            </div>
          </n-space>
        </n-form-item>

        <n-form-item label="耗时预估">
          <n-text>{{ estText }}</n-text>
        </n-form-item>
      </n-form>

      <n-alert v-if="stackStatus && stackStatus.available_today > 0" type="success" style="margin-bottom: 12px">
        已就绪：档案堆栈中已有 {{ stackStatus.available_today }} 台可用档案，可以直接测试【留存设备】！
      </n-alert>
      <n-alert v-else-if="stackStatus && stackStatus.total_entries === 0" type="info" style="margin-bottom: 12px">
        提示：堆栈当前为空（首次运行），本次将运行 L3 新增设备并自动保存为档案，跑完即可测试【留存】！
      </n-alert>
      <n-alert v-else-if="stackStatus && stackStatus.available_today === 0" type="warning" style="margin-bottom: 12px">
        提示：今天的 {{ stackStatus.used_today }} 份档案已全部用完，继续运行将自动走 L3 新增。若需重新测试留存，请点击下方的「手动重置设备使用状态」。
      </n-alert>

      <n-alert v-if="crossesMidnight" type="warning">
        预计跨零点完成，DAU 按北京时间自然日切分，建议改期或分批。
      </n-alert>

      <div style="margin-top: 12px; display: flex; gap: 12px; align-items: center; flex-wrap: wrap">
        <n-button type="primary" size="large" :disabled="!canStart" @click="start">
          开始放量
        </n-button>
        <n-button v-if="batch.running" type="error" size="large" @click="stop">停止（将清理设备）</n-button>
        <n-button type="warning" size="large" @click="resetUsage">手动重置设备使用状态</n-button>
      </div>

      <div v-if="stackStatus" style="margin-top: 12px">
        <n-space align="center">
          <n-tag type="info">档案堆栈容量: {{ stackStatus.total_entries }} / {{ stackStatus.capacity }}</n-tag>
          <n-tag type="success">今天可用: {{ stackStatus.available_today }} 台</n-tag>
          <n-tag type="warning">今天已用: {{ stackStatus.used_today }} 台</n-tag>
          <n-text v-if="stackStatus.last_cleared_date" depth="3" style="font-size: 12px">
            （上次重置日期: {{ stackStatus.last_cleared_date }}）
          </n-text>
        </n-space>
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
        <n-statistic v-if="batch.blockedHits > 0" label="已拦截系统/广告无用流量" :value="batch.blockedHits" />
        <n-statistic v-if="batch.totalTraffic" label="放行网络流量" :value="batch.totalTraffic" />
        <n-tag type="success" size="small" style="align-self: center">
          🛡️ 省流量拦截已开启
        </n-tag>
        <n-button v-if="batch.done" type="primary" @click="$router.push(`/report/${batch.runId}`)">
          查看报告
        </n-button>
      </n-space>

      <!-- 手机换 IP 动态状态条 -->
      <div v-if="app.config.auto_rotate_ip || batch.currentIp || batch.rotatingIp" style="margin-top: 12px; padding: 8px 12px; background: #fafafa; border-radius: 6px; border: 1px solid #eee">
        <n-space align="center">
          <n-tag v-if="batch.rotatingIp" type="warning" size="small">
            ✈️ 正在切换手机飞行模式重拨换 IP 中...
          </n-tag>
          <n-tag v-if="batch.currentIp" type="info" size="small">
            🌐 出口 IP: {{ batch.currentIp }}
          </n-tag>
          <n-text v-if="batch.rotateMessage" depth="3" style="font-size: 12px">
            {{ batch.rotateMessage }}
          </n-text>
        </n-space>
      </div>

      <!-- 时间范围等待动态状态条 -->
      <div v-if="batch.waitingWindow" style="margin-top: 12px; padding: 8px 12px; background: #fffbe6; border-radius: 6px; border: 1px solid #ffe58f">
        <n-space align="center">
          <n-tag type="warning" size="small">
            ⏸️ 时间范围挂起等待中
          </n-tag>
          <n-text style="font-size: 13px; color: #d48806; font-weight: 500">
            {{ batch.windowMessage || `当前不在设定时间范围 [${app.config.time_window_start} - ${app.config.time_window_end}] 内，正在等待进入时间范围...` }}
          </n-text>
        </n-space>
      </div>

      <n-grid :cols="6" :x-gap="8" :y-gap="8" style="margin-top: 16px">
        <n-gi v-for="[idx, d] in [...batch.devices].sort((a, b) => a[0] - b[0])" :key="idx">
          <n-tooltip>
            <template #trigger>
              <div class="dev-cell" :class="[d.status, d.is_retention ? 'retention' : 'new-device']">
                #{{ idx }} {{ d.is_retention ? '(留存)' : (d.reset_level && d.reset_level.includes('L2.5') ? '(多用户)' : '(新增)') }}
              </div>
            </template>
            <div style="max-width: 320px; font-size: 12px; line-height: 1.6">
              <div>
                <b>运行模式：</b>
                <n-tag :type="d.is_retention ? 'success' : (d.reset_level && d.reset_level.includes('L2.5') ? 'info' : 'warning')" size="small">
                  {{ d.is_retention ? "留存设备（档案还原）" : (d.reset_level && d.reset_level.includes('L2.5') ? "L2.5 多用户" : "L3 恢复出厂新增") }}
                </n-tag>
              </div>
              <div v-if="d.device_model"><b>设备型号：</b>{{ d.device_model }}</div>
              <div><b>运行状态：</b>{{ d.status }}　<b>耗时：</b>{{ d.duration_s.toFixed(1) }}s</div>
              <div><b>ANDROID_ID：</b>{{ d.android_id || "—" }}</div>
              <div v-if="d.umid && d.umid !== '—' && d.umid !== ''"><b>UMID：</b>{{ d.umid }}</div>
              <div v-if="d.error" style="color: #ff8888; margin-top: 4px"><b>错误：</b>{{ d.error }}</div>
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
  NInput, NInputNumber, NProgress, NSelect, NSpace, NStatistic, NSwitch, NTag, NText, NTimePicker, NTooltip, useMessage,
} from "naive-ui";
import { api } from "../api/ipc";
import { onBatchDevice, onBatchIpStatus, onBatchProgress, onBatchTimeWindowStatus } from "../api/events";
import { useAppStore } from "../stores/app";
import { useBatchStore } from "../stores/batch";
import ApkDropZone from "../components/ApkDropZone.vue";
import type { ApkInfo, StackStatusReport, UsbPhoneInfo } from "../api/types";

const app = useAppStore();
const batch = useBatchStore();
const message = useMessage();
const declaredAppkey = ref("");
const apkInfo = ref<ApkInfo | null>(null);
const stackStatus = ref<StackStatusReport | null>(null);

const detectedPhones = ref<UsbPhoneInfo[]>([]);
const refreshingPhones = ref(false);
const testingIp = ref(false);
const phonePublicIp = ref<string | null>(null);
const fetchingPhoneIp = ref(false);

async function fetchCurrentPhoneIp() {
  fetchingPhoneIp.value = true;
  try {
    const ip = await api.getCurrentPublicIp(app.config.rotate_ip_serial || undefined);
    if (ip) {
      phonePublicIp.value = ip;
      batch.currentIp = ip;
    }
  } catch (e) {
    // 忽略加载异常
  } finally {
    fetchingPhoneIp.value = false;
  }
}

const levelOptions = [
  { label: "L1 · pm clear（~3s/台）", value: "L1" },
  { label: "L2 · 卸载重装（~15s/台）", value: "L2" },
  { label: "L2.5 · 多用户（~15s/台）", value: "L25" },
  { label: "L3 · 恢复出厂（~100s/台，已验证）", value: "L3" },
];

const allocationMode = ref("auto");
const modeOptions = [
  { label: "自动概率轮换 (推荐)", value: "auto" },
  { label: "强制留存测试 (100% 抽取档案)", value: "force_retention" },
  { label: "强制全新增测试 (100% L3 生成)", value: "force_new" },
];

function onModeChange(val: string) {
  if (val === "force_retention") {
    app.config.l3_probability = 0.0;
  } else if (val === "force_new") {
    app.config.l3_probability = 1.0;
  } else {
    app.config.l3_probability = 0.40;
  }
}

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

const isCurrentTimeInWindow = computed(() => {
  if (!app.config.time_window_enabled) return true;
  const startStr = app.config.time_window_start || "08:00";
  const endStr = app.config.time_window_end || "22:00";
  const [sh, sm] = startStr.split(":").map(Number);
  const [eh, em] = endStr.split(":").map(Number);
  const startMin = (sh || 0) * 60 + (sm || 0);
  const endMin = (eh || 0) * 60 + (em || 0);

  const parts = new Intl.DateTimeFormat("zh-CN", {
    timeZone: "Asia/Shanghai",
    hour: "numeric",
    minute: "numeric",
    hourCycle: "h23",
  }).formatToParts(new Date());
  const bjHour = Number(parts.find((p) => p.type === "hour")?.value ?? 0);
  const bjMinute = Number(parts.find((p) => p.type === "minute")?.value ?? 0);
  const curMin = bjHour * 60 + bjMinute;

  if (startMin <= endMin) {
    return curMin >= startMin && curMin < endMin;
  } else {
    return curMin >= startMin || curMin < endMin;
  }
});

async function fetchStackStatus() {
  try {
    stackStatus.value = await api.getStackStatus();
  } catch (e) {
    // 忽略加载错误
  }
}

async function resetUsage() {
  try {
    await api.resetProfileUsage();
    message.success("已手动重置今天的所有档案使用状态（解除单日限制）");
    await fetchStackStatus();
  } catch (e: any) {
    message.error("重置失败：" + e);
  }
}

async function inspectApk() {
  if (!app.config.apk_path) return;
  try {
    apkInfo.value = await api.inspectApk(app.config.apk_path, declaredAppkey.value);
    if (apkInfo.value.pkg && !app.config.pkg) app.config.pkg = apkInfo.value.pkg;
    if (apkInfo.value.app_label) app.config.app_label = apkInfo.value.app_label;
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
  if (app.config.time_window_enabled) {
    if (!app.config.time_window_start) app.config.time_window_start = "08:00";
    if (!app.config.time_window_end) app.config.time_window_end = "22:00";
  }
  const id = await api.batchStart(app.config);
  batch.reset(id);
  if (app.config.time_window_enabled && !isCurrentTimeInWindow.value) {
    batch.waitingWindow = true;
    batch.windowMessage = `当前北京时间不在设定时间范围 [${app.config.time_window_start} - ${app.config.time_window_end}] 内，已暂停调度挂起等待中...`;
  }
  message.success(`已启动 ${id}`);
}

async function stop() {
  await api.batchStop(batch.runId);
  batch.running = false;
}

async function refreshPhones() {
  refreshingPhones.value = true;
  try {
    detectedPhones.value = await api.detectUsbPhones();
    await fetchCurrentPhoneIp();
  } catch (e: any) {
    message.error("检测手机失败: " + e);
  } finally {
    refreshingPhones.value = false;
  }
}

async function onRotateIpToggle(enabled: boolean) {
  if (enabled && detectedPhones.value.length === 0) {
    await refreshPhones();
  } else if (enabled) {
    await fetchCurrentPhoneIp();
  }
}

async function manualTestRotateIp() {
  testingIp.value = true;
  try {
    const res = await api.testRotateIp({
      serial: app.config.rotate_ip_serial || undefined,
      disconnect_wait_s: app.config.rotate_ip_disconnect_wait_s,
      reconnect_wait_s: app.config.rotate_ip_reconnect_wait_s,
      hotspot_ssid: app.config.rotate_ip_hotspot_ssid || undefined,
      hotspot_password: app.config.rotate_ip_hotspot_password || undefined,
    });
    if (res.success) {
      message.success(res.message);
      batch.currentIp = res.new_ip;
      phonePublicIp.value = res.new_ip;
    } else {
      message.warning(res.message);
    }
  } catch (e: any) {
    message.error("测试换 IP 失败: " + e);
  } finally {
    testingIp.value = false;
  }
}

onMounted(async () => {
  await fetchStackStatus();
  await onBatchDevice((d) => {
    batch.onDevice(d);
    fetchStackStatus();
  });
  await onBatchProgress((p) => {
    batch.onProgress(p);
    fetchStackStatus();
  });
  await onBatchIpStatus((s) => {
    batch.onIpStatus(s);
    if (s.ip) {
      phonePublicIp.value = s.ip;
    }
  });
  await onBatchTimeWindowStatus((s) => {
    batch.onTimeWindowStatus(s);
  });
  refreshPhones();
  fetchCurrentPhoneIp();
});
</script>

<style scoped>
.dev-cell {
  height: 38px;
  border-radius: 6px;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 12px;
  background: #e8e8e8;
  color: #666;
  font-weight: 500;
}
.dev-cell.ok.retention { background: #d3f0dd; color: #18a058; border: 1px solid #18a058; }
.dev-cell.ok.new-device { background: #e3f2fd; color: #2080f0; border: 1px solid #2080f0; }
.dev-cell.fail { background: #fbe0e3; color: #d03050; border: 1px solid #d03050; }
</style>
