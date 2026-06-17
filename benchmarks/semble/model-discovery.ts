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

    // Probe reranker FIRST (more specific — a model that responds to /v1/rerank
    // with valid results is a reranker even if it also responds to /v1/embeddings)
    const isReranker = await probeReranker(url, m.id);
    if (isReranker) {
      model.type = "reranker";
      reranker_models.push(model);
      if (verbose) console.log(`    ✓ ${m.id} — reranker`);
      models.push(model);
      continue;
    }

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

    // Assume chat/LLM
    model.type = "chat";
    chat_models.push(model);
    if (verbose) console.log(`    ○ ${m.id} — chat/LLM`);
    models.push(model);
  }

  return { endpoint: url, models, embedding_models, reranker_models, chat_models, unknown_models };
}

/**
 * Verify specific models without probing all others.
 * When user specifies --semantic-api-model and --rerank-model, skip full
 * discovery to avoid unloading desired models from GPU memory.
 */
export async function verifySpecificModels(
  url: string,
  embeddingModelId?: string,
  rerankerModelId?: string,
  verbose: boolean = false,
): Promise<ModelDiscoveryResult> {
  const models: DiscoveredModel[] = [];
  const embedding_models: DiscoveredModel[] = [];
  const reranker_models: DiscoveredModel[] = [];

  if (embeddingModelId) {
    const dim = await probeEmbedding(url, embeddingModelId);
    if (dim !== null) {
      const model: DiscoveredModel = { id: embeddingModelId, name: embeddingModelId, type: "embedding", vector_dim: dim };
      embedding_models.push(model);
      models.push(model);
      if (verbose) console.log(`  ✓ ${embeddingModelId} — embedding verified (dim=${dim})`);
    } else {
      console.warn(`  WARNING: Embedding model ${embeddingModelId} did not respond to /v1/embeddings`);
    }
  }

  if (rerankerModelId) {
    const isReranker = await probeReranker(url, rerankerModelId);
    if (isReranker) {
      const model: DiscoveredModel = { id: rerankerModelId, name: rerankerModelId, type: "reranker" };
      reranker_models.push(model);
      models.push(model);
      if (verbose) console.log(`  ✓ ${rerankerModelId} — reranker verified`);
    } else {
      console.warn(`  WARNING: Reranker model ${rerankerModelId} did not respond to /v1/rerank`);
    }
  }

  return { endpoint: url, models, embedding_models, reranker_models, chat_models: [], unknown_models: [] };
}

/**
 * Re-probe a specific model to ensure it's loaded into GPU memory.
 * After full discovery unloads models, this reloads the desired ones.
 */
export async function ensureModelLoaded(
  url: string,
  modelId: string,
  type: "embedding" | "reranker",
  verbose: boolean = false,
): Promise<boolean> {
  if (verbose) console.log(`  Ensuring ${modelId} is loaded...`);
  if (type === "embedding") {
    const dim = await probeEmbedding(url, modelId);
    if (dim !== null) {
      if (verbose) console.log(`  ✓ ${modelId} loaded (dim=${dim})`);
      return true;
    }
  } else {
    const ok = await probeReranker(url, modelId);
    if (ok) {
      if (verbose) console.log(`  ✓ ${modelId} loaded`);
      return true;
    }
  }
  console.warn(`  WARNING: ${modelId} failed to load after discovery`);
  return false;
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

// ---------------------------------------------------------------------------
// Interactive model selection
// ---------------------------------------------------------------------------

import * as readline from "readline";

/**
 * Prompt user to select a model from a list interactively.
 * Returns the selected model or null if user skips.
 */
function promptSelect(
  rl: readline.Interface,
  title: string,
  models: DiscoveredModel[],
): Promise<DiscoveredModel | null> {
  return new Promise((resolve) => {
    if (models.length === 0) {
      resolve(null);
      return;
    }
    console.log(`\n  ${title}:`);
    for (let i = 0; i < models.length; i++) {
      const m = models[i];
      const dim = m.vector_dim ? ` (dim=${m.vector_dim})` : "";
      console.log(`    [${i + 1}] ${m.id}${dim}`);
    }
    console.log(`    [s] Skip (don't use this mode)`);

    rl.question(`  Select model [1-${models.length}/s]: `, (answer) => {
      const trimmed = answer.trim().toLowerCase();
      if (trimmed === "s" || trimmed === "skip" || trimmed === "") {
        resolve(null);
      } else {
        const idx = parseInt(trimmed, 10) - 1;
        if (idx >= 0 && idx < models.length) {
          resolve(models[idx]);
        } else {
          console.log(`  Invalid selection, using first model.`);
          resolve(models[0]);
        }
      }
    });
  });
}

/**
 * Interactive model selection flow.
 * Discovers models, displays them, and prompts user to select.
 * Returns selected embedding model, reranker model, and whether user wants to proceed.
 */
export async function interactiveModelSelection(
  semanticApiUrl: string,
  rerankUrl: string | undefined,
  verbose: boolean,
): Promise<{
  embeddingModel: DiscoveredModel | null;
  rerankerModel: DiscoveredModel | null;
  proceed: boolean;
}> {
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });

  try {
    console.log("\n=== Interactive Model Discovery ===");

    // Discover semantic API models
    console.log(`\nQuerying ${semanticApiUrl}/v1/models...`);
    const semanticDiscovery = await discoverModels(semanticApiUrl, verbose);

    if (semanticDiscovery.models.length === 0) {
      console.log("  No models found at semantic API endpoint.");
      rl.close();
      return { embeddingModel: null, rerankerModel: null, proceed: false };
    }

    // Print all discovered models
    console.log("\n  Discovered models:");
    for (const line of formatDiscoveredModels(semanticDiscovery)) {
      console.log(`  ${line}`);
    }

    // Select embedding model
    const embeddingModel = await promptSelect(
      rl,
      "Select embedding model for semantic search",
      semanticDiscovery.embedding_models,
    );

    if (embeddingModel) {
      console.log(`  → Selected: ${embeddingModel.id} (dim=${embeddingModel.vector_dim})`);
    }

    // Discover reranker models if endpoint provided
    let rerankerModel: DiscoveredModel | null = null;
    if (rerankUrl) {
      console.log(`\nQuerying ${rerankUrl}/v1/models...`);
      const rerankDiscovery = await discoverModels(rerankUrl, verbose);

      if (rerankDiscovery.reranker_models.length > 0) {
        rerankerModel = await promptSelect(
          rl,
          "Select reranker model",
          rerankDiscovery.reranker_models,
        );

        if (rerankerModel) {
          console.log(`  → Selected: ${rerankerModel.id}`);
        }
      } else {
        console.log("  No reranker models found.");
      }
    }

    // Confirm
    console.log("\n  Configuration:");
    console.log(`    Embedding: ${embeddingModel?.id || "(none)"}${embeddingModel?.vector_dim ? ` dim=${embeddingModel.vector_dim}` : ""}`);
    console.log(`    Reranker:  ${rerankerModel?.id || "(none)"}`);

    return new Promise((resolve) => {
      rl.question("\n  Proceed with this configuration? [Y/n]: ", (answer) => {
        const proceed = answer.trim().toLowerCase() !== "n";
        rl.close();
        resolve({ embeddingModel, rerankerModel, proceed });
      });
    });
  } catch (e) {
    rl.close();
    console.error(`  Interactive discovery failed: ${e}`);
    return { embeddingModel: null, rerankerModel: null, proceed: false };
  }
}
