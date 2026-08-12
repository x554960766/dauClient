<template>
  <div :class="['apk-drop-zone', { active: dragging, wrong: wrongType }]">
    <div class="icon">{{ wrongType ? "⚠️" : "📦" }}</div>
    <div class="title">
      {{ dragging ? (wrongType ? "请拖入 .apk 文件" : "松开即可加载") : "拖入 APK 文件到此处" }}
    </div>
    <div class="hint">仅支持 .apk 格式</div>
  </div>
</template>

<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { getCurrentWebview } from "@tauri-apps/api/webview";

const emit = defineEmits<{ drop: [path: string] }>();
const dragging = ref(false);
const wrongType = ref(false);
let unlisten: (() => void) | null = null;

function isApk(p: string): boolean {
  return p.toLowerCase().endsWith(".apk");
}

onMounted(async () => {
  unlisten = await getCurrentWebview().onDragDropEvent((event) => {
    const p = event.payload;
    if (p.type === "enter") {
      // enter 携带 paths，判断是否 .apk
      const hasApk = p.paths.some(isApk);
      dragging.value = true;
      wrongType.value = !hasApk && p.paths.length > 0;
    } else if (p.type === "over") {
      // over 不带 paths，保持 enter 时的高亮状态
      dragging.value = true;
    } else if (p.type === "leave") {
      dragging.value = false;
      wrongType.value = false;
    } else if (p.type === "drop") {
      dragging.value = false;
      wrongType.value = false;
      const apk = p.paths.find(isApk);
      if (apk) emit("drop", apk);
    }
  });
});

onUnmounted(() => {
  unlisten?.();
  unlisten = null;
});
</script>

<style scoped>
.apk-drop-zone {
  border: 2px dashed #d0d0d0;
  border-radius: 8px;
  padding: 24px;
  text-align: center;
  color: #999;
  transition: all 0.2s ease;
  background: #fafafa;
  cursor: default;
  user-select: none;
}
.apk-drop-zone.active {
  border-color: #18a058;
  background: #f0fcf4;
  color: #18a058;
  transform: scale(1.01);
}
.apk-drop-zone.active.wrong {
  border-color: #d03050;
  background: #fff0f2;
  color: #d03050;
}
.apk-drop-zone .icon {
  font-size: 30px;
  margin-bottom: 6px;
  line-height: 1;
}
.apk-drop-zone .title {
  font-size: 14px;
  font-weight: 500;
}
.apk-drop-zone .hint {
  font-size: 12px;
  margin-top: 2px;
  opacity: 0.8;
}
</style>
