/**
 * wire 类型绑定的统一出口（P2-1）。
 *
 * 这些类型由 Rust 侧 `hermix-transport` 的 `#[derive(TS)]` **单向生成**：
 * `npm run gen:bindings`（等价于
 * `cargo test -p hermix-transport --features typescript export_bindings`）。
 *
 * 不要手改同目录下的 `.ts` 文件——下次生成会被覆盖。要改形状就在 Rust
 * 结构体上改，然后重新生成。`npm run check:bindings` 会在 CI 里重新生成
 * 并断言没有 diff，从而拦住「改了 Rust 忘了同步前端」这类漂移。
 */
export type { FileAttachment } from "./FileAttachment";
export type { ImageAttachment } from "./ImageAttachment";
