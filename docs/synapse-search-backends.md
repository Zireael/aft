# Synapse embeddings and reranking

SubC is the daemon that hosts modules and routes requests to them. Synapse's
management surface is its named-operation API: `models.list` lists served models
and their fingerprints; `rerank.score` scores query/candidate pairs. A fingerprint
identifies a particular model artifact and execution configuration.

## Selecting Synapse embeddings

`aft setup` enables the semantic-index feature; it does not select an embedding
provider. Select Synapse in **user configuration** (`~/.config/cortexkit/aft.jsonc`):

```json
{
  "semantic": {
    "backend": "synapse",
    "model": "gte-modernbert-base-f16"
  },
  "subc": {
    "connection_file": "/absolute/path/to/subc-connection.json"
  }
}
```

Use a model ID actually served by your Synapse installation (`models.list`), not
an embedding-provider default. Run AFT under the SubC daemon with Synapse
registered. Synapse is not an in-process embedding model. The connection file
must exist and contain the running daemon's connection information. Missing
connection configuration reports `synapse_missing_connection_file`; failed
daemon connections report `synapse_daemon_unavailable`; a missing management
surface reports `synapse_capability_unavailable`. These reasons explain why the
semantic index is not ready; lexical search remains usable.

The Rust schema/resolver and both plugin schemas accept `semantic.backend:
"synapse"`, carry `subc.connection_file` from user config, and reject an
implicit/default model by leaving the model unset. The semantic index already records the served
fingerprint and reports initialization failures. Setup has no provider picker,
and doctor does not probe Synapse specifically: inspect live semantic-index
status in a running AFT session rather than interpreting installed binaries as
proof of Synapse connectivity. A future provider picker/doctor probe would need
to use the daemon's catalog and `models.list`, not a local model-file check.

## Reranker backend integration

The remote and Synapse implementations are backend building blocks; they are
not yet wired to the search pipeline in this change. The default remains off.

Remote accepts a base endpoint and appends `/rerank`. The default wire format
sends `model`, `query`, `documents`, and `top_n` equal to the submitted batch
size. The server must return every document index exactly once with a finite
`relevance_score` in `[0,1]`. The maximum batch defaults to 20 and can be supplied
by the caller. `api_key_env` names the environment variable containing the bearer
key. Errors do not include credentials, request bodies, or vendor response
bodies; redirects are refused so credentials cannot leak to another host.

**TEI selection rule:** use `tei+http://host` or `tei+https://host` in the existing
endpoint field. The prefix is stripped before connecting; the request is
`{query, texts}` and the response is `[{index, score}]`. No new config key is
needed. Use a base URL, not a URL already ending in `/rerank`.

Synapse uses the same SubC connection file and management-route identity as
embeddings, but requires `models.list` and `rerank.score`, not embedding ops.
The caller must supply whether AFT is running under SubC. A backend constructor
accepts a rerank model, an optional explicit fingerprint, and a queue budget.
Without an override, `SynapseReranker::connect` discovers the model via
`models.list` under the constructor's bounded deadline, off the search path.
It returns an error rather than installing a backend if discovery fails.
`fingerprint()` never performs I/O and returns a pin fixed for the backend's
entire lifetime. Concurrent score calls wait for the transport lock up to their
own deadlines; an expired wait returns `Timeout`.
Every score request sends `required_fingerprint`, refuses equivalent substitutions,
and sends both `max_queue_ms` and `deadline_ms`. A refused substitution is
reported with the old and new pins when discovery still fits the deadline.
Synapse scores are finite raw logits, not probabilities. Batches above 20 are
refused before routing. Connect, catalog, route setup, and scoring share the
caller's total deadline, with no request retries.

The backends use the shared trait in `commands::semantic_search::rerank`.
Temporary `remote.rs` and `synapse.rs` module declarations in `synapse_embed.rs`
compile the backends and their tests without making them selectable by search.
The search pipeline still needs a builder that constructs backends outside
interactive requests, using resolved user configuration and the actual
daemon-mode flag. The config resolver is unchanged here.
