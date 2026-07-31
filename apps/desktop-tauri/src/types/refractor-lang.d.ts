// refractor 把所有语言打成 JS，没自带 .d.ts。
// 这里统一声明成 unknown，让 refractor.register 接受。
declare module "refractor" {
  // refractor 是 CJS，导出 highlight + register 作为 named exports
  // (不是 refractor 这个名字，是直接 named)
  export function highlight(value: string, name: string): any;
  export function register(lang: any): void;
}

// Wildcard declaration covering every `refractor/lang/*` language module in
// one shot (avoids a long list of individual `declare module` entries).
declare module "refractor/lang/*" {
  const value: unknown;
  export default value;
}
