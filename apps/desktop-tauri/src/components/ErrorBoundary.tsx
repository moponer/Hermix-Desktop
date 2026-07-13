// ErrorBoundary — catastrophic crash catcher (P3-3 of FRONTEND_AUDIT_2026-07-13)
//
// Without this, a single uncaught React render error whites out the entire
// desktop shell. The user has to kill the process and restart — unacceptable
// for a long-running desktop app.
//
// This component catches render-phase errors only. Async/event-handler errors
// still need their own try/catch (but those won't crash the whole tree).

import { Component, type ReactNode } from "react";

interface Props {
  children: ReactNode;
  /** Optional: display name for debugging (e.g. "ChatView", "Sidebar"). */
  name?: string;
}

interface State {
  hasError: boolean;
  error: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, errorInfo: React.ErrorInfo): void {
    console.error(`[ErrorBoundary${this.props.name ? ` ${this.props.name}` : ""}]`, error, errorInfo);
  }

  handleRetry = (): void => {
    this.setState({ hasError: false, error: null });
  };

  render(): ReactNode {
    if (this.state.hasError) {
      return (
        <div className="error-boundary-fallback" style={{
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          justifyContent: "center",
          height: "100%",
          width: "100%",
          padding: 32,
          background: "#0f0f12",
          color: "#a0a0a8",
          fontFamily: "system-ui, sans-serif",
        }}>
          <div style={{ fontSize: 48, marginBottom: 16 }}>
            ⚠
          </div>
          <h2 style={{ fontSize: 18, fontWeight: 600, margin: "0 0 8px", color: "#e0e0e4" }}>
            {this.props.name ? `${this.props.name} 发生错误` : "页面发生错误"}
          </h2>
          <p style={{ fontSize: 14, margin: "0 0 24px", textAlign: "center", maxWidth: 480, lineHeight: 1.5 }}>
            应用遇到了一个意外错误。请尝试点击下方按钮重新加载此区域。
            如果问题持续出现，请重启应用。
          </p>
          <div style={{ display: "flex", gap: 12 }}>
            <button
              onClick={this.handleRetry}
              style={{
                padding: "8px 20px",
                borderRadius: 6,
                border: "none",
                background: "#2e2e34",
                color: "#e0e0e4",
                cursor: "pointer",
                fontSize: 14,
                fontWeight: 500,
              }}
            >
              重试
            </button>
            <button
              onClick={() => window.location.reload()}
              style={{
                padding: "8px 20px",
                borderRadius: 6,
                border: "1px solid #3e3e44",
                background: "transparent",
                color: "#8e8e94",
                cursor: "pointer",
                fontSize: 14,
              }}
            >
              重新加载应用
            </button>
          </div>
          {this.state.error && (
            <details style={{ marginTop: 24, maxWidth: 560, width: "100%" }}>
              <summary style={{ cursor: "pointer", fontSize: 12, color: "#6e6e74" }}>
                错误详情
              </summary>
              <pre style={{
                marginTop: 8,
                padding: 12,
                background: "#1c1c20",
                borderRadius: 6,
                fontSize: 12,
                overflow: "auto",
                maxHeight: 200,
                whiteSpace: "pre-wrap",
                wordBreak: "break-word",
                color: "#c0a0a0",
              }}>
                {this.state.error.message}
                {"\n\n"}
                {this.state.error.stack}
              </pre>
            </details>
          )}
        </div>
      );
    }
    return this.props.children;
  }
}
