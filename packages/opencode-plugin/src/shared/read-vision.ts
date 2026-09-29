import { resolvePromptContext } from "./last-assistant-model.js";

export const VISION_HOST_TIMEOUT_MS = 1500;
type Model = { providerID: string; modelID: string };
type State = {
  models: Map<string, Model>;
  history: Map<string, Promise<Model | undefined>>;
  capabilities: Map<string, Promise<boolean | undefined>>;
};
const states = new WeakMap<object, State>();
function stateFor(client: object): State {
  let state = states.get(client);
  if (!state) {
    state = { models: new Map(), history: new Map(), capabilities: new Map() };
    states.set(client, state);
  }
  return state;
}

/** Host hooks are authoritative; history is only a fallback before the first hook. */
export function rememberReadModel(client: object, sessionID: string, model: Model): void {
  stateFor(client).models.set(sessionID, model);
}

async function bounded<T>(call: () => Promise<T>): Promise<T | undefined> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      Promise.resolve().then(call),
      new Promise<undefined>((resolve) => {
        timer = setTimeout(() => resolve(undefined), VISION_HOST_TIMEOUT_MS);
      }),
    ]);
  } catch {
    return undefined;
  } finally {
    clearTimeout(timer);
  }
}
function record(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

export async function currentSessionVisionCapability(
  client: object,
  sessionID: string | undefined,
): Promise<boolean | undefined> {
  if (!sessionID) return undefined;
  const state = stateFor(client);
  let model = state.models.get(sessionID);
  if (!model) {
    let pending = state.history.get(sessionID);
    if (!pending) {
      pending = bounded(() => resolvePromptContext(client, sessionID)).then(
        (context) => context?.model,
      );
      state.history.set(sessionID, pending);
      void pending.finally(() => state.history.delete(sessionID));
    }
    const historical = await pending;
    // A hook may have supplied a newer model while history was in flight.
    model = state.models.get(sessionID) ?? historical;
  }
  if (!model) return undefined;
  const currentModel = model;
  const key = JSON.stringify([model.providerID, model.modelID]);
  let capability = state.capabilities.get(key);
  if (!capability) {
    capability = (async () => {
      const api = (client as { provider?: { list?: () => Promise<unknown> } }).provider;
      if (!api?.list) return undefined;
      const list = api.list.bind(api);
      const listed = record(await bounded(list));
      const catalog = record(listed?.data) ?? listed;
      const providers = catalog?.all ?? catalog?.providers;
      if (!Array.isArray(providers)) return undefined;
      const provider = providers.map(record).find((entry) => entry?.id === currentModel.providerID);
      const models = provider?.models;
      const entry = record(
        Array.isArray(models)
          ? models.find((entry) => record(entry)?.id === currentModel.modelID)
          : record(models)?.[currentModel.modelID],
      );
      const inputs = record(entry?.modalities)?.input;
      return Array.isArray(inputs)
        ? inputs.includes("image")
        : typeof entry?.attachment === "boolean"
          ? entry.attachment
          : undefined;
    })();
    state.capabilities.set(key, capability);
    // Do not retain transient failures or unknown catalog entries indefinitely.
    void capability.then((value) => {
      if (value === undefined) state.capabilities.delete(key);
    });
  }
  return capability;
}
