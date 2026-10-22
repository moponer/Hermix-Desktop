<script setup lang="ts">
/**
 * 模型选择器芯片 + 底部面板。
 *
 * 手机端不能像桌面端那样在顶栏塞下拉——屏幕太窄。
 * 做法：顶栏放一个芯片显示当前模型名，点击展开底部面板列出全部模型。
 *
 * 桌面端的模型配置 UI（ModelListTab）不在手机端复刻——
 * 添加/删除模型需要填 API Key，手机上打字体验差，交给桌面端。
 *
 * ## 清单从哪来
 *
 * **桌面推来的快照**（`model.list` 事件）。手机端不自己存清单，也不走
 * HTTP：此前它调 `GET {relay}/api/model/list`，而中继是只路由的字节管道、
 * 没有通往桌面的 HTTP 通道 → 必然 404 → 选择器永远是空的。
 * 切换同理走 WS（只改默认值，见 `core/session.ts` 的 selectModel）。
 */
import { computed, ref } from 'vue'
import { modelId, type ModelEntry } from '../../core/model'

const props = defineProps<{
  /** 桌面推来的已启用模型清单。 */
  models: ModelEntry[]
  /** 桌面当前的默认模型（`provider:model`）。 */
  defaultModel: string | null
}>()

const emit = defineEmits<{ (e: 'select', model: string): void }>()

const open = ref(false)
const saving = ref(false)
const error = ref('')

const modelList = computed(() => props.models ?? [])

const currentId = computed(() => props.defaultModel)

const currentName = computed(() => {
  if (!props.defaultModel) return '选择模型'
  const i = props.defaultModel.indexOf(':')
  return i > 0 ? props.defaultModel.slice(i + 1) : props.defaultModel
})

function pick(entry: ModelEntry) {
  if (saving.value) return
  const id = modelId(entry)
  error.value = ''
  saving.value = true
  emit('select', id)
  // 桌面会回推新的清单（model.list），那边到了才收。
  setTimeout(() => {
    saving.value = false
  }, 1500)
  open.value = false
}
</script>

<template>
  <!-- 顶栏芯片：当前模型名 -->
  <view class="chip" @tap="open = true">
    <text class="chip-txt">{{ currentName }}</text>
    <text class="chip-arrow">▾</text>
  </view>

  <!-- 底部面板 -->
  <view v-if="open" class="mask" @tap="open = false">
    <view class="sheet" @tap.stop>
      <view class="grip" />
      <text class="sh-title">切换模型</text>
      <text v-if="error" class="sh-err">{{ error }}</text>

      <view v-if="!modelList.length" class="empty">
        <text>还没收到模型清单</text>
        <text class="empty-hint">清单由桌面推过来——确认电脑上的 hermix 正在运行且已与这台手机配对</text>
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
            <text class="row-provider">{{ entry.provider_name || entry.provider_id }}</text>
          </view>
          <text v-if="`${entry.provider_id}:${entry.model_id}` === currentId" class="cur-check">✓</text>
          <view v-if="saving && `${entry.provider_id}:${entry.model_id}` === currentId" class="saving-dot" />
        </view>
      </view>

      <view class="hm-pill ghost cancel-btn" @tap="open = false">
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
  border-radius: $r-card;
  margin-top: 16rpx;
}
</style>
