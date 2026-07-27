// refractor 把所有语言打成 JS，没自带 .d.ts。
// 这里统一声明成 unknown，让 refractor.register 接受。
declare module "refractor" {
  // refractor 是 CJS，导出 highlight + register 作为 named exports
  // (不是 refractor 这个名字，是直接 named)
  export function highlight(value: string, name: string): any;
  export function register(lang: any): void;
}

// 通配符声明：覆盖所有 refractor/lang/* 语言模块。
// 之前为每种语言单独声明，但随着语言覆盖扩展到 60+ 种，
// 维护一长串 declare module 太冗余。通配符一次性覆盖所有。
declare module "refractor/lang/*" {
  const value: unknown;
  export default value;
}
