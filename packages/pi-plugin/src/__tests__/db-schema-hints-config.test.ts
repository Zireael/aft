import { expect, test } from "bun:test";
import { AftConfigSchema, resolveBashConfig, resolveProjectOverridesForConfigure } from "../config";

test("database schema hints default on and explicit project switch reaches configure", () => {
  expect(resolveBashConfig({}).db_schema_hints).toBe(true);
  const config = AftConfigSchema.parse({ disabled_tools: [], bash: { db_schema_hints: false } });
  expect(resolveBashConfig(config).db_schema_hints).toBe(false);
  expect(resolveProjectOverridesForConfigure(config).bash).toEqual({ db_schema_hints: false });
});

test("database schema hints reject nonboolean values", () => {
  expect(AftConfigSchema.safeParse({ bash: { db_schema_hints: "false" } }).success).toBe(false);
});
