import { Component, type ReactNode } from "react";

export class BrainRendererBoundary extends Component<
  { children: ReactNode; onOverview: () => void },
  { error: Error | null }
> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <section className="aperture" style={{ display: "grid", placeItems: "center", padding: 32 }}>
        <div role="alert" style={{ maxWidth: 480 }}>
          <h2>Brain renderer unavailable</h2>
          <p>The selected canvas could not start. You can open the 2D overview or use the navigation to explore other surfaces.</p>
          <details style={{ marginBottom: 20 }}>
            <summary>Renderer error</summary>
            <pre style={{ whiteSpace: "pre-wrap" }}>{String(this.state.error)}</pre>
          </details>
          <button
            type="button"
            className="nl-chip"
            onClick={() => {
              this.props.onOverview();
              this.setState({ error: null });
            }}
          >
            Open 2D overview
          </button>
        </div>
      </section>
    );
  }
}
