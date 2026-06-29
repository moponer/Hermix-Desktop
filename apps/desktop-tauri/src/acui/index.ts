/**
 * M5: ACUI module entry point.
 *
 * Re-exports the public surface so callers can do:
 *   import { AcuiPanel, useAcuiState, AcuiRegistry } from "../acui";
 */

export * from "./types";
export { AcuiRegistry, AcuiFallback } from "./AcuiRegistry";
export type { AcuiComponent, AcuiComponentProps, AcuiDecisionBus } from "./AcuiRegistry";
export { AcuiPanel } from "./AcuiPanel";
export type { AcuiPanelProps } from "./AcuiPanel";
export { renderAcuiCards } from "./AcuiRenderer";
export type { AcuiRenderBuckets, AcuiRendererProps } from "./AcuiRenderer";
export { useAcuiState } from "./useAcuiState";
export type { UseAcuiState, AcuiStats } from "./useAcuiState";
export { SelfCheckStepCard } from "./builtin/SelfCheckStepCard";
export { AwakeningCard } from "./builtin/AwakeningCard";