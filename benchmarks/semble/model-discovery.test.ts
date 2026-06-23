import { describe, expect, it } from "bun:test";
import { openAiEndpoint, rerankResponseHasResults } from "./model-discovery";

describe("model discovery endpoint handling", () => {
  it("normalizes OpenAI-compatible base URLs without duplicating /v1", () => {
    expect(openAiEndpoint("http://127.0.0.1:8090", "models")).toBe("http://127.0.0.1:8090/v1/models");
    expect(openAiEndpoint("http://127.0.0.1:8090/v1", "embeddings")).toBe("http://127.0.0.1:8090/v1/embeddings");
    expect(openAiEndpoint("http://127.0.0.1:8090/v1/rerank", "models")).toBe("http://127.0.0.1:8090/v1/models");
    expect(openAiEndpoint("http://127.0.0.1:8090/v1/rerank", "rerank")).toBe("http://127.0.0.1:8090/v1/rerank");
  });

  it("accepts common reranker response shapes", () => {
    expect(rerankResponseHasResults({ results: [{ index: 0, relevance_score: 0.9 }] })).toBe(true);
    expect(rerankResponseHasResults({ data: [{ index: 0, score: 0.9 }] })).toBe(true);
    expect(rerankResponseHasResults({ results: [] })).toBe(false);
  });
});
