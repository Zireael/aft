import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { bashDeadTransportDeclaration } from "../../harness/permission-plan.js";
import type {
  HarnessExtension,
  HarnessRuntimeEvent,
  HarnessValidationContext,
  ScenarioDefinition,
  ScenarioLifecycleContext,
} from "../../harness/types.js";
const here = dirname(fileURLToPath(import.meta.url));
const expectedIds = ["bash/T1/happy","bash/T2/invalid_arguments","bash/T2/missing_target","bash/T3/fallback_ask_allow","bash/T3/fallback_ask_deny","bash/T3/fallback_config_deny","bash/T3/loop_ask_allow","bash/T3/loop_ask_deny","bash/T3/loop_config_deny","bash/T4/abort","bash/T5/completion_wake","bash/T5/message_aborts_sync_watch","bash/T5/message_detaches_wait","bash/T5/watch_pattern_once","bash/T6/bash-bash-output/complete","bash/T6/bash-bash-output/incomplete","bash/T7/happy"];
async function validate(context: HarnessValidationContext): Promise<void> {
  const scenarios = context.scenarios.filter((scenario) => scenario.tool === "bash");
  const actual = scenarios.map((scenario) => scenario.id).sort();
  if (JSON.stringify(actual) !== JSON.stringify(expectedIds)) throw new Error("bash" + " scenario identity mismatch: " + actual.join(","));
  for (const scenario of scenarios) for (const turn of scenario.turns) if (turn.response.kind === "tool_calls") for (const call of turn.response.calls) {
    if (call.disk_effects !== undefined && call.non_mutating_evidence !== undefined) throw new Error(scenario.id + ":" + call.id + " has two disk classifications");
    if (call.disk_effects === undefined && call.non_mutating_evidence === undefined) throw new Error(scenario.id + ":" + call.id + " lacks a disk classification");
  }
  const matrix = JSON.parse(await readFile(join(here, "matrix.json"), "utf8"));
  if (JSON.stringify(matrix.rows?.[0]?.trajectories) !== JSON.stringify({"T1":"applicable","T2":"applicable","T3":"applicable","T4":"applicable","T5":"applicable","T6":"applicable","T7":"applicable"})) throw new Error("bash" + " applicability mismatch");
  const controls = JSON.parse(await readFile(join(here, "mutation-controls.json"), "utf8"));
  if (!Array.isArray(controls.controls) || controls.controls.length < 3) throw new Error("bash" + " mutation controls missing");
  // Every fallback row says what its dead transport produces, and a row that
  // refuses instead of falling back has to record where the break-glass
  // coverage went. Read at validation time so a row that drops the record
  // fails before a run, rather than passing quietly with one capability less
  // than the slice claims.
  for (const scenario of scenarios.filter((candidate) => candidate.id.startsWith("bash/T3/fallback_"))) {
    const declared = bashDeadTransportDeclaration(scenario);
    if (declared.outcome === "refusal" && !declared.hostFallbackCoverage) {
      throw new Error(scenario.id + " does not record its break-glass coverage");
    }
  }
  for (const scenario of scenarios) {
    const plan = messageDetachPlan(scenario);
    if (!plan) continue;
    const control = scenario.controls?.find((candidate) => candidate.id === plan.control_id);
    if (control?.purpose !== "message") {
      throw new Error(scenario.id + " message_detach names no message control " + plan.control_id);
    }
    if (scenario.compare_call_id !== plan.call_id) {
      throw new Error(scenario.id + " message_detach must judge the compared call " + String(scenario.compare_call_id));
    }
  }
}

/**
 * A row that sends a message while a call is waiting declares which control
 * sends it, which call it must end, and how soon. The comparison alone proves
 * the result is the interrupted one; this adds that it came back promptly, so a
 * wait that ran to its natural end and only then reported cannot pass.
 */
export interface MessageDetachPlan {
  control_id: string;
  call_id: string;
  budget_ms: number;
}

export function messageDetachPlan(scenario: ScenarioDefinition): MessageDetachPlan | undefined {
  const raw = scenario.metadata?.message_detach as Partial<MessageDetachPlan> | undefined;
  if (!raw) return undefined;
  if (
    typeof raw.control_id !== "string" ||
    typeof raw.call_id !== "string" ||
    !Number.isInteger(raw.budget_ms) ||
    (raw.budget_ms ?? 0) < 1
  ) {
    throw new Error(scenario.id + " has an invalid message_detach declaration");
  }
  return raw as MessageDetachPlan;
}

/**
 * Judge one runtime event of a message-detach row. `sentAt` holds, per running
 * row, when its message control started; a result seen before that, or later
 * than the budget after it, was not ended by the message.
 */
export function judgeMessageDetachEvent(
  scenario: ScenarioDefinition,
  event: HarnessRuntimeEvent,
  sentAt: Map<string, number>,
  key: string,
): void {
  const plan = messageDetachPlan(scenario);
  if (!plan) return;
  if (event.kind === "control_started" && event.control.id === plan.control_id) {
    sentAt.set(key, event.at);
    return;
  }
  if (event.kind !== "tool_result_observed" || event.call.id !== plan.call_id) return;
  const sent = sentAt.get(key);
  if (sent === undefined) {
    throw new Error(
      `${scenario.id}: the ${plan.call_id} result came back before the message was sent, so no message ended that wait`,
    );
  }
  const elapsed = event.at - sent;
  if (elapsed > plan.budget_ms) {
    throw new Error(
      `${scenario.id}: the ${plan.call_id} result came back ${elapsed}ms after the message, over the ${plan.budget_ms}ms budget, so the message did not end the wait`,
    );
  }
}

// Keyed by the row's forensic directory, which is unique per row and host run,
// because rows run concurrently and share this extension.
const messageSentAt = new Map<string, number>();

function observe(context: ScenarioLifecycleContext, event: HarnessRuntimeEvent): void {
  judgeMessageDetachEvent(context.scenario, event, messageSentAt, context.forensic_dir);
  if (event.kind === "host_exit") messageSentAt.delete(context.forensic_dir);
}
const extension: HarnessExtension = { name: "bash-scenarios-v1", validate, observe };
export default extension;
