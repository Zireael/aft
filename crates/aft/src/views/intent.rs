//! Dirty intent: every AFT write records the paths it changed before it
//! acknowledges, and queries match pending-intent paths on their current bytes
//! before any index pruning (`snapshot::LiveDelta::record_intent`).
