// Optional host export. Upstream Pi does not install OMP; the runtime import is
// feature-detected before any tool definition is widened or delegated.
declare module "@oh-my-pi/pi-coding-agent/internal-urls" {
  export const InternalUrlRouter: {
    instance(): import("./omp-internal-urls.js").OmpInternalUrlRouter;
  };
}
