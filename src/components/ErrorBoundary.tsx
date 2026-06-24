import React from "react";
import { log } from "../lib/logger";

type Props = { children: React.ReactNode };
type State = { failed: boolean };

export default class ErrorBoundary extends React.Component<Props, State> {
  state: State = { failed: false };

  static getDerivedStateFromError(): State {
    return { failed: true };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    log.error("react", "component crash", {
      errorType: error.name,
      componentStack: (info.componentStack ?? "").slice(0, 1200),
    });
  }

  render() {
    if (this.state.failed) {
      return (
        <div style={{ padding: 24, color: "var(--color-t1)" }}>
          Something went wrong. Diagnostic details were written to the local logs.
        </div>
      );
    }
    return this.props.children;
  }
}
