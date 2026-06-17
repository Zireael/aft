/**
 * Model discovery for benchmark setup.
 *
 * Queries /v1/models to discover served models, classifies them by probing
 * /v1/embeddings and /v1/rerank endpoints, and returns structured model info
 * for terminal display and report metadata.
 */

export interface DiscoveredModel {
  id: string;
  name: string;
  type: "embedding" | "reranker" | "chat" | "unknown";
  vector_dim?: number;
  owner?: string;
}

export interface ModelDiscoveryResult {
  endpoint: string;
  models: DiscoveredModel[];
  embedding_models: DiscoveredModel[];
  reranker_models: DiscoveredModel[];
  chat_models: DiscoveredModel[];
  unknown_models: DiscoveredModel[];
}

/**
 * Fetch the model list from an OpenAI-compatible /v1/models endpoint.
 */
async function fetchModelList(url: string): Promise<Array<{ id: string; name?: string; owned_by?: string }>> {
  try {
    const resp = await fetch(`${url}/v1/models`, { signal: AbortSignal.timeout(5000) });
    if (!resp.ok) return [];
    const json = await resp.json() as any;
    return json.data || json.models || [];
  } catch {
    return [];
  }
}

/**
 * Probe a model by attempting an embedding request.
 * Returns vector dimension if successful, null otherwise.
 */
async function probeEmbedding(url: string, modelId: string): Promise<number | null> {
  try {
    const resp = await fetch(`${url}/v1/embeddings`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model: modelId, input: "test" }),
      signal: AbortSignal.timeout(10_000),
    });
    if (!resp.ok) return null;
    const json = await resp.json() as any;
    const data = json.data;
    if (Array.isArray(data) && data.length > 0 && data[0].embedding) {
      return data[0].embedding.length;
    }
    return null;
  } catch {
    return null;
  }
}

/**
 * Probe a model by attempting a rerank request.
 * Returns true if successful, false otherwise.
 */
async function probeReranker(url: string, modelId: string): Promise<boolean> {
  try {
    const resp = await fetch(`${url}/v1/rerank`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model: modelId, query: "test", documents: ["doc1", "doc2"], top_n: 2 }),
      signal: AbortSignal.timeout(10_000),
    });
    if (!resp.ok) return false;
    const json = await resp.json() as any;
    return Array.isArray(json.results) && json.results.length > 0;
  } catch {
    return false;
  }
}

/**
 * Discover and classify all models served by an API endpoint.
 *
 * Classification strategy:
 * 1. Fetch /v1/models for the full list
 * 2. Try /v1/embeddings with each model → embedding model with dimension
 * 3. Try /v1/rerank with each model → reranker model
 * 4. Neither → chat/LLM model (or unknown)
 */
export async function discoverModels(
  url: string,
  verbose: boolean = false,
): Promise<ModelDiscoveryResult> {
  const rawModels = await fetchModelList(url);
  const models: DiscoveredModel[] = [];
  const embedding_models: DiscoveredModel[] = [];
  const reranker_models: DiscoveredModel[] = [];
  const chat_models: DiscoveredModel[] = [];
  const unknown_models: DiscoveredModel[] = [];

  if (verbose) console.log(`  Discovering models from ${url}/v1/models...`);

  for (const m of rawModels) {
    const model: DiscoveredModel = {
      id: m.id,
      name: m.name || m.id,
      type: "unknown",
      owner: m.owned_by,
    };

    // Probe embedding
    const dim = await probeEmbedding(url, m.id);
    if (dim !== null) {
      model.type = "embedding";
      model.vector_dim = dim;
      embedding_models.push(model);
      if (verbose) console.log(`    ✓ ${m.id} — embedding, dim=${dim}`);
      models.push(model);
      continue;
    }

    // Probe reranker
    const isReranker = await probeReranker(url, m.id);
    if (isReranker) {
      model.type = "reranker";
      reranker_models.push(model);
      if (verbose) console.log(`    ✓ ${m.id} — reranker`);
      models.push(model);
      continue;
    }

    // Assume chat/LLM
    model.type = "chat";
    chat_models.push(model);
    if (verbose) console.log(`    ○ ${m.id} — chat/LLM`);
    models.push(model);
  }

  return { endpoint: url, models, embedding_models, reranker_models, chat_models, unknown_models };
}

/**
 * Format discovered models for terminal display.
 */
export function formatDiscoveredModels(result: ModelDiscoveryResult): string[] {
  const lines: string[] = [];
  lines.push(`  Endpoint: ${result.endpoint}`);
  lines.push(`  Total models: ${result.models.length}`);

  if (result.embedding_models.length > 0) {
    lines.push(`  Embedding:`);
    for (const m of result.embedding_models) {
      lines.push(`    - ${m.id} (dim=${m.vector_dim})`);
    }
  }

  if (result.reranker_models.length > 0) {
    lines.push(`  Reranker:`);
    for (const m of result.reranker_models) {
      lines.push(`    - ${m.id}`);
    }
  }

  if (result.chat_models.length > 0) {
    lines.push(`  Chat/LLM:`);
    for (const m of result.chat_models) {
      lines.push(`    - ${m.id}`);
    }
  }

  return lines;
}

/**
 * Auto-detect the best embedding model from discovered models.
 * Prefers models with known good dimensions (384, 768, 1024).
 */
export function selectBestEmbeddingModel(
  result: ModelDiscoveryResult,
  preferred?: string,
): DiscoveredModel | null {
  if (preferred) {
    const found = result.embedding_models.find((m) => m.id === preferred);
    if (found) return found;
  }
  // Prefer smallest reasonable dimension for benchmark speed
  const sorted = [...result.embedding_models].sort((a, b) => (a.vector_dim || 9999) - (b.vector_dim || 9999));
  return sorted[0] || null;
}

/**
 * Auto-detect the best reranker model from discovered models.
 */
export function selectBestRerankerModel(
  result: ModelDiscoveryResult,
  preferred?: string,
): DiscoveredModel | null {
  if (preferred) {
    const found = result.reranker_models.find((m) => m.id === preferred);
    if (found) return found;
  }
  return result.reranker_models[0] || null;
}
