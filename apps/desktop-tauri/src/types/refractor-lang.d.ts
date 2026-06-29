// refractor 把所有语言打成 JS，没自带 .d.ts。
// 这里统一声明成 unknown，让 refractor.register 接受。
declare module "refractor" {
  // refractor 是 CJS，导出 highlight + register 作为 named exports
  // (不是 refractor 这个名字，是直接 named)
  export function highlight(value: string, name: string): any;
  export function register(lang: any): void;
}
declare module "refractor/lang/python" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/typescript" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/javascript" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/jsx" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/tsx" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/rust" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/go" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/java" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/kotlin" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/c" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/cpp" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/ruby" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/php" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/bash" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/css" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/scss" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/json" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/yaml" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/toml" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/markup" {
  const value: unknown;
  export default value;
}
declare module "refractor/lang/sql" {
  const value: unknown;
  export default value;
}
