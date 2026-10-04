import { defineConfig } from 'vite'
import uni from '@dcloudio/vite-plugin-uni'

// H5 目标可在本仓库内用 Node 构建验证（`npm run build:h5`），
// 因此**逻辑层的正确性有一道真门禁**，而不只是"看起来对"。
// Android/iOS 打包需要各自的原生 SDK，不在 CI 的这条路径上。
export default defineConfig({
  plugins: [uni()],
  server: {
    // 手机连本机 PC 跑 H5 预览时用；relay 在本机测试时把这里换成 relay 地址
    host: '0.0.0.0',
    port: 5173,
  },
})
