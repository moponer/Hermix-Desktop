import { createSSRApp } from 'vue'
import App from './App.vue'
import { reportFatal } from './core/boot'

export function createApp() {
  const app = createSSRApp(App)
  // 渲染期异常的唯一收口点。改造前这里什么都没有——异常直接变成白屏，
  // 用户既看不到信息也没有恢复路径（见 core/boot.ts 的文件头）。
  app.config.errorHandler = (err, _instance, info) => {
    reportFatal(err, info ?? '')
  }
  return { app }
}
