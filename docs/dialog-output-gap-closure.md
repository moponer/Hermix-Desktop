# 对话输出疑点回源查实（hermix-desktop）

口径：每条结论给出文件路径:行号 + 该处代码原文摘录（≤3 行）；「对既有结论的影响」标 推翻 / 修正 / 维持不变。源码未读到的小节写「未读到，缺口保留」，不填行号。

## 一、done 帧是否必然发出（crates/hermix-server/src/ws/mod.rs handle_ws_stream）

待查。

## 二、出站队列是否共用（ws/outbound.rs、transport.rs、stdio_session.rs writer_tx）

待查。

## 三、审批合并队列是否连带堵正文（crates/hermix-server/src/coalescer.rs）

待查。

## 四、docCanvasStore 上屏时机（packages @hermix/preview）

待查。

## 五、invoke runtime 与前端轮询（src-tauri/src/lib.rs、apps/desktop-tauri/src/App.tsx）

待查。

## 本次实读过的文件清单

- docs/dialog-output-chain-map.md
