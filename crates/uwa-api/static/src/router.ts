export interface Route {
  path: string;
  label: string;
  render: (root: HTMLElement) => void | (() => void);
}

export class Router {
  private current = "";
  private cleanup: (() => void) | void = undefined;

  constructor(private routes: Route[], private defaultPath: string) {}

  start(): void {
    window.addEventListener("hashchange", () => this.handle());
    this.handle();
  }

  navigate(path: string): void {
    if (location.hash === `#${path}`) return;
    location.hash = `#${path}`;
  }

  private handle(): void {
    if (this.cleanup) { try { this.cleanup(); } catch { /* ignore */ } this.cleanup = undefined; }
    const hash = location.hash.slice(1) || this.defaultPath;
    const route = this.routes.find((r) => r.path === hash) ?? this.routes.find((r) => r.path === this.defaultPath);
    if (!route) return;

    this.current = route.path;

    // Nav
    const nav = document.getElementById("nav");
    if (nav) {
      nav.replaceChildren(
        ...this.routes.map((r) =>
          Object.assign(document.createElement("a"), {
            href: `#${r.path}`,
            textContent: r.label,
            className: r.path === route.path ? "active" : "",
          }),
        ),
      );
    }

    // View
    const app = document.getElementById("app");
    if (app) {
      app.replaceChildren();
      const cleanup = route.render(app);
      if (typeof cleanup === "function") this.cleanup = cleanup;
    }
  }
}
