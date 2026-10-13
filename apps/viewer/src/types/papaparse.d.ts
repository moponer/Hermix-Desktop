// papaparse 没有自带类型；包内 workbook.ts 仅用它做 .csv/.tsv 解码，
// 调通就够了。
declare module "papaparse" {
  export interface ParseResult<T> {
    data: T[];
    errors: { type?: string; code?: string; message: string; row?: number }[];
    meta: { delimiter?: string; linebreak?: string; aborted?: boolean; truncated?: boolean; cursor?: number };
  }
  export interface ParseConfig {
    header?: boolean;
    skipEmptyLines?: boolean | "greedy";
    dynamicTyping?: boolean;
    delimiter?: string;
    newline?: string;
  }
  export function parse<T = unknown>(input: string, config?: ParseConfig): ParseResult<T>;
  export function unparse(data: unknown, config?: ParseConfig): string;
  const _default: { parse: typeof parse; unparse: typeof unparse };
  export default _default;
}
