/** V2 passes a Location-scoped Effect context where V1 passes an SDK client. */
export function isV2PluginContext(client: unknown): boolean {
  return (
    typeof (client as { location?: { directory?: unknown } } | null)?.location?.directory ===
    "string"
  );
}
