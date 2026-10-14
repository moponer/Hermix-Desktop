<script setup lang="ts">
/**
 * 模型选择器芯片 + 底部面板。
 *
 * 手机端不能像桌面端那样在顶栏塞下拉——屏幕太窄。
 * 做法：顶栏放一个芯片显示当前模型名，点击展开底部面板列出全部模型。
 *
 * 桌面端的模型配置 UI（ModelListTab）不在手机端复刻——
 * 添加/删除模型需要填 API Key，手机上打字体验差，交给桌面端。
 */
import { ref, computed, onMounted } from 'vue'
import { getModelList, setDefaultModel, type ModelEntry } from '../../core/model'

const open = ref(false)
const loading = ref(false)
const modelList = ref<ModelEntry[]>([])
const defaultModel = ref<string | null>(null)
const saving = ref(false)
const error = ref('')

/** "provider:model" 格式 */
const currentId = computed(() => defaultModel.value)
const currentName = computed(() => {
  if (!defaultModel.value) return '选择模型'
  const [p, m] = defaultModel.value.split(':', 2)
  return m || defaultModel.value
})

onMounted(() => {
  void loadModels()
})

async function loadModels() {
  loading.value = true
  error.value = ''
  const data = await getModelList()
  loading.value = false
  if (!data) {
    error.value = '获取模型列表失败'
    return
  }
  modelList.value = data.models
  defaultModel.value = data.default_model
}

async function pick(entry: ModelEntry) {
  const id = `${entry.provider_id}:${entry.model_id}`
  saving.value = true
  error.value = ''
  const ok = await setDefaultModel(modelList.value, id)
  saving.value = false
  if (!ok) {
    error.value = '切换失败，请重试'
    return
  }
  defaultModel.value = id
  open.value = false
}

function openSheet() {
  open.value = true
  void loadModels()
}
</script>

<template>
  <!-- 顶栏芯片：当前模型名 -->
  <view class="chip" @tap="openSheet">
    <text class="chip-txt">{{ loading ? '…' : currentName }}</text>
    <text class="chip-arrow">▾</text>
  </view>

  <!-- 底部面板 -->
  <view v-if="open" class="mask" @tap="open = false">
    <view class="sheet" @tap.stop>
      <view class="grip" />
      <text class="sh-title">切换模型</text>
      <text v-if="error" class="sh-err">{{ error }}</text>

      <view v-if="loading" class="loading">
        <text>加载中…</text>
      </view>

      <view v-else-if="!modelList.length" class="empty">
        <text>暂无可用模型</text>
        <text class="empty-hint">在桌面端配置模型后可在此切换</text>
      </view>

      <view v-else class="list">
        <view
          v-for="entry in modelList"
          :key="`${entry.provider_id}:${entry.model_id}`"
          class="row"
          :class="{ cur: `${entry.provider_id}:${entry.model_id}` === currentId }"
          @tap="pick(entry)"
        >
          <view class="row-info">
            <text class="row-model">{{ entry.model_id }}</text>
            <text class="row-provider">{{ entry.provider_id }}</text>
          </view>
          <text v-if="`${entry.provider_id}:${entry.model_id}` === currentId" class="cur-check">✓</text>
          <view v-if="saving && `${entry.provider_id}:${entry.model_id}` === currentId" class="saving-dot" />
        </view>
      </view>

      <view class="cancel-btn" @tap="open = false">
        <text>取消</text>
      </view>
    </view>
  </view>
</template>

<style lang="scss" scoped>
.chip {
  display: flex;
  align-items: center;
  gap: 6rpx;
  padding: 10rpx 18rpx;
  border-radius: 999px;
  background: $raised;
  color: $fg2;
  font-size: $fs-mini;
  max-width: 220rpx;
}
.chip:active {
  opacity: 0.7;
}
.chip-txt {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 160rpx;
}
.chip-arrow {
  font-size: 18rpx;
  color: $fg4;
  flex: none;
}

/* 底部面板 */
.mask {
  position: fixed;
  inset: 0;
  z-index: 80;
  background: rgba(0, 0, 0, 0.55);
  display: flex;
  align-items: flex-end;
}
.sheet {
  width: 100%;
  background: $raised;
  border-radius: 36rpx 36rpx 0 0;
  padding: 16rpx 28rpx calc(env(safe-area-inset-bottom) + 28rpx);
  max-height: 72vh;
  overflow-y: auto;
}
.grip {
  width: 64rpx;
  height: 8rpx;
  border-radius: 4rpx;
  background: $fg4;
  margin: 0 auto 20rpx;
}
.sh-title {
  display: block;
  font-size: $fs-body;
  font-weight: 600;
  color: $fg;
  margin-bottom: 20rpx;
  text-align: center;
}
.sh-err {
  display: block;
  font-size: $fs-mini;
  color: $err;
  text-align: center;
  margin-bottom: 12rpx;
}
.loading,
.empty {
  padding: 40rpx 0;
  text-align: center;
  color: $fg2;
  font-size: $fs-sub;
}
.empty-hint {
  display: block;
  margin-top: 10rpx;
  font-size: $fs-mini;
  color: $fg4;
}
.list {
  display: flex;
  flex-direction: column;
  gap: 10rpx;
}
.row {
  display: flex;
  align-items: center;
  gap: 16rpx;
  padding: 22rpx 24rpx;
  border-radius: $r-card;
  background: $card;
}
.row.cur {
  background: rgba(111, 174, 148, 0.12);
}
.row:active {
  opacity: 0.75;
}
.row-info {
  flex: 1;
  min-width: 0;
}
.row-model {
  display: block;
  font-size: $fs-body;
  font-weight: 500;
  color: $fg;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.row.cur .row-model {
  color: $accent;
}
.row-provider {
  display: block;
  font-size: $fs-mini;
  color: $fg4;
  margin-top: 4rpx;
}
.cur-check {
  flex: none;
  color: $accent;
  font-size: $fs-body;
}
.saving-dot {
  flex: none;
  width: 20rpx;
  height: 20rpx;
  border-radius: 50%;
  background: $accent;
  animation: pulse 0.8s ease-in-out infinite;
}
@keyframes pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.3; }
}
.cancel-btn {
  height: 88rpx;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: $r-card;
  background: $card;
  color: $fg2;
  font-size: $fs-sub;
  margin-top: 16rpx;
}
.cancel-btn:active {
  opacity: 0.7;
}
</style>
