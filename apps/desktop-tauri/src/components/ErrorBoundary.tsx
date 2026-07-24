// ErrorBoundary — catastrophic crash catcher.
// Catches render-phase errors only. Async/event-handler errors
// still need their own try/catch (but those won't crash the whole tree).
// Wrap individual top-level views so a crash in one panel doesn't
// white-screen the entire desktop shell.

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
        <div className="error-boundary-fallback">
          <div className="error-boundary-fallback__icon">⚠</div>
          <h2 className="error-boundary-fallback__title">
            {this.props.name ? `${this.props.name} 发生错误` : "页面发生错误"}
          </h2>
          <p className="error-boundary-fallback__desc">
            应用遇到了一个意外错误。请尝试点击下方按钮重新加载此区域。
            如果问题持续出现，请重启应用。
          </p>
          <div className="error-boundary-fallback__actions">
            <button
              onClick={this.handleRetry}
              className="error-boundary-fallback__btn error-boundary-fallback__btn--primary"
            >
              重试
            </button>
            <button
              onClick={() => window.location.reload()}
              className="error-boundary-fallback__btn error-boundary-fallback__btn--secondary"
            >
              重新加载应用
            </button>
          </div>
          {this.state.error && (
            <details className="error-boundary-fallback__details">
              <summary className="error-boundary-fallback__summary">错误详情</summary>
              <pre className="error-boundary-fallback__pre">
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
