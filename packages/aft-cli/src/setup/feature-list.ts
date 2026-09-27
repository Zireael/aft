import type { Readable, Writable } from "node:stream";
import { styleText } from "node:util";
import { GroupMultiSelectPrompt } from "@clack/core";
import {
  formatInstructionFooter,
  isCancel,
  limitOptions,
  MULTISELECT_INSTRUCTIONS,
  S_BAR,
  S_BAR_END,
  S_CHECKBOX_ACTIVE,
  S_CHECKBOX_INACTIVE,
  S_CHECKBOX_SELECTED,
  symbol,
} from "@clack/prompts";

/**
 * The grouped feature checklist for setup.
 *
 * Clack's own group multiselect prints a row's description as a hint in
 * parentheses after the label, only for checked or focused rows, and wraps a
 * long hint with the wrong tree prefix (the continuation line loses the `│`
 * that joins the group). This renderer puts each description on its own
 * indented line under the label, word-wrapped to the terminal width with the
 * same tree prefix on every line, so a row renders the same whatever its state
 * and the tree never breaks. Rows show only the label and description.
 */

export interface FeatureRow {
  value: string;
  label: string;
  description: string;
}

/**
 * How rows depend on each other. `requires` maps a setting row to the tool row
 * it configures: while that tool is unchecked the setting does not apply, is
 * drawn as not applicable and cannot be toggled, and keeps its value so
 * checking the tool again restores it. `implies` maps a row to a row it needs:
 * checking the row checks its prerequisite, and unchecking the prerequisite
 * unchecks the row.
 */
export interface SelectionRules {
  requires: ReadonlyMap<string, string>;
  implies: ReadonlyMap<string, string>;
  /** Display name of a required row, for the not-applicable note. */
  labels?: ReadonlyMap<string, string>;
}

export const NO_RULES: SelectionRules = { requires: new Map(), implies: new Map() };

/**
 * The selection after one toggle, with the rules applied: a row that does not
 * apply keeps its previous value, a newly checked row checks its prerequisite,
 * and a newly unchecked prerequisite unchecks the rows that need it.
 */
export function applySelectionRules(
  previous: readonly string[],
  next: readonly string[],
  rules: SelectionRules,
): string[] {
  const before = new Set(previous);
  const after = new Set(next);
  for (const [row, tool] of rules.requires) {
    if (before.has(tool) || after.has(tool)) continue;
    if (before.has(row)) after.add(row);
    else after.delete(row);
  }
  for (const [row, prerequisite] of rules.implies) {
    if (!after.has(row) || after.has(prerequisite)) continue;
    if (before.has(prerequisite)) after.delete(row);
    else after.add(prerequisite);
  }
  return [...after];
}

/**
 * A finished selection made consistent: every checked row's prerequisite is
 * checked. Used on whatever the prompt returns, so answers never ask for a
 * row without the row it needs.
 */
export function normalizeSelection(picked: readonly string[], rules: SelectionRules): string[] {
  const selected = new Set(picked);
  for (const [row, prerequisite] of rules.implies) {
    if (selected.has(row)) selected.add(prerequisite);
  }
  return [...selected];
}

/** Rows that do not apply under `selected`, each mapped to the unchecked row it requires. */
export function inapplicableRows(
  selected: ReadonlySet<string>,
  rules: SelectionRules,
): Map<string, string> {
  const rows = new Map<string, string>();
  for (const [row, tool] of rules.requires) {
    if (!selected.has(tool)) rows.set(row, tool);
  }
  return rows;
}

/** Everything drawn before a row's text: the prompt guide bar and the tree column. */
const GUIDE = `${S_BAR}  `;
/** Row text column: guide (3) + tree (2) + checkbox (2). */
const TEXT_INDENT = 7;
const MIN_TEXT_WIDTH = 20;

const dim = (text: string) => styleText("dim", text);

/** Word-wrap plain text to `width` columns; a word longer than a line is split. */
export function wrapWords(text: string, width: number): string[] {
  const lines: string[] = [];
  let line = "";
  for (const word of text.split(/\s+/).filter(Boolean)) {
    let rest = word;
    while (rest.length > width) {
      if (line) {
        lines.push(line);
        line = "";
      }
      lines.push(rest.slice(0, width));
      rest = rest.slice(width);
    }
    if (!line) line = rest;
    else if (line.length + 1 + rest.length <= width) line = `${line} ${rest}`;
    else {
      lines.push(line);
      line = rest;
    }
  }
  if (line) lines.push(line);
  return lines;
}

export type RowState = "active" | "selected" | "active-selected" | "inactive";

/**
 * A group checkbox's state. `partial` is a group with some rows checked and
 * some not: clack draws it the same as an empty group, which made a mostly-on
 * group (say Editing with only aft_move and aft_delete off) look switched off.
 */
export type GroupState = RowState | "partial" | "active-partial";

/**
 * Clack has no partial checkbox, so this follows its own convention: a
 * Unicode symbol where its checkboxes use Unicode, an ASCII one otherwise.
 */
const S_CHECKBOX_PARTIAL = S_CHECKBOX_SELECTED === "[+]" ? "[~]" : "◧";

function checkbox(state: GroupState): string {
  if (state === "active-selected" || state === "selected") {
    return styleText("green", S_CHECKBOX_SELECTED);
  }
  if (state === "partial") return styleText("green", S_CHECKBOX_PARTIAL);
  if (state === "active-partial") return styleText("cyan", S_CHECKBOX_PARTIAL);
  if (state === "active") return styleText("cyan", S_CHECKBOX_ACTIVE);
  return dim(S_CHECKBOX_INACTIVE);
}

/**
 * One feature row as terminal lines, without the guide bar. `last` marks the
 * final row of its group, which closes the tree branch. `requires` names the
 * unchecked row this one depends on: the row is then drawn as not applicable,
 * with an empty dim checkbox whatever its remembered value.
 */
export function renderRowLines(
  row: FeatureRow,
  state: RowState,
  last: boolean,
  columns: number,
  requires?: string,
): string[] {
  const branch = dim(last ? S_BAR_END : S_BAR);
  const continuation = last ? " " : dim(S_BAR);
  const focused = state === "active" || state === "active-selected";
  // Only the focused row's label is at full brightness, as in clack's own
  // lists; the checkbox colour shows checked or not. A checked row drawn at
  // full brightness would hide the cursor whenever it sits on a checked row.
  const label = focused ? row.label : dim(row.label);
  const box = requires ? dim(S_CHECKBOX_INACTIVE) : checkbox(state);
  const note = requires ? ` ${dim(`(not applicable: ${requires} is off)`)}` : "";
  const lines = [`${branch} ${box} ${label}${note}`];
  const width = Math.max(MIN_TEXT_WIDTH, columns - TEXT_INDENT - 1);
  for (const text of wrapWords(row.description, width)) {
    lines.push(`${continuation}   ${dim(text)}`);
  }
  return lines;
}

/**
 * A group header: the group name with a checkbox that reflects its rows (all,
 * some or none checked). Pressing space on a partial group checks every row,
 * which is clack's toggle for any group that is not fully checked.
 */
export function renderGroupLine(name: string, state: GroupState): string {
  return `${checkbox(state)} ${state === "inactive" ? dim(name) : name}`;
}

/** The header state for a group given how many of its rows are checked. */
export function groupState(active: boolean, checked: number, total: number): GroupState {
  if (total > 0 && checked === total) return active ? "active-selected" : "selected";
  if (checked > 0) return active ? "active-partial" : "partial";
  return active ? "active" : "inactive";
}

/**
 * A complete static frame of the checklist: message, every group and row, and
 * the key help. Used for the prompt's first paint and by tests; the live
 * prompt scrolls the same lines when they do not fit the terminal.
 */
export function renderFeatureList(
  message: string,
  groups: Record<string, FeatureRow[]>,
  selected: ReadonlySet<string>,
  columns: number,
  cursor: string | null = null,
  rules: SelectionRules = NO_RULES,
): string {
  const out = [S_BAR, `${symbol("active")}  ${message}`];
  const inapplicable = inapplicableRows(selected, rules);
  for (const [group, rows] of Object.entries(groups)) {
    const applicable = rows.filter((row) => !inapplicable.has(row.value));
    const checked = applicable.filter((row) => selected.has(row.value)).length;
    out.push(
      `${GUIDE}${renderGroupLine(group, groupState(group === cursor, checked, applicable.length))}`,
    );
    rows.forEach((row, index) => {
      const state = rowState(row.value === cursor, selected.has(row.value));
      const lines = renderRowLines(
        row,
        state,
        index === rows.length - 1,
        columns,
        requiredLabel(inapplicable.get(row.value), rules),
      );
      for (const line of lines) out.push(`${GUIDE}${line}`);
    });
  }
  out.push(...formatInstructionFooter(MULTISELECT_INSTRUCTIONS, true));
  return out.join("\n");
}

function requiredLabel(required: string | undefined, rules: SelectionRules): string | undefined {
  return required === undefined ? undefined : (rules.labels?.get(required) ?? required);
}

function rowState(active: boolean, selected: boolean): RowState {
  if (active) return selected ? "active-selected" : "active";
  return selected ? "selected" : "inactive";
}

type PromptOption = FeatureRow & { group: string | boolean };

/** Terminal streams for the prompt; tests pass their own to render at a fixed width. */
export interface PromptStreams {
  input?: Readable;
  output?: Writable & { columns?: number };
}

/**
 * Show the checklist and return the checked row values; calls `onCancel` on
 * cancel. `rules` keeps dependent rows consistent as the user toggles.
 */
export async function promptFeatureList(
  message: string,
  groups: Record<string, FeatureRow[]>,
  initial: string[],
  onCancel: () => never,
  streams: PromptStreams = {},
  rules: SelectionRules = NO_RULES,
): Promise<string[]> {
  const output = streams.output ?? process.stdout;
  const columnsOf = () => output.columns ?? 80;
  const rowsByValue = new Map<string, { row: FeatureRow; last: boolean }>();
  for (const rows of Object.values(groups)) {
    rows.forEach((row, index) => {
      rowsByValue.set(row.value, { row, last: index === rows.length - 1 });
    });
  }
  const prompt = new GroupMultiSelectPrompt<FeatureRow>({
    options: groups,
    initialValues: initial,
    required: false,
    selectableGroups: true,
    ...(streams.input ? { input: streams.input } : {}),
    ...(streams.output ? { output: streams.output } : {}),
    render() {
      const header = `${S_BAR}\n${symbol(this.state)}  ${message}\n`;
      const values = (this.value ?? []) as string[];
      const inapplicable = inapplicableRows(new Set(values), rules);
      if (this.state === "submit" || this.state === "cancel") {
        const picked = this.options
          .filter(
            (option) =>
              option.group !== true &&
              values.includes(option.value) &&
              !inapplicable.has(option.value),
          )
          .map((option) => option.label);
        const summary =
          this.state === "cancel"
            ? styleText(["strikethrough", "dim"], picked.join(", ") || "none")
            : dim(picked.join(", ") || "none");
        const columns = columnsOf();
        const wrapped = wrapWords(summary, Math.max(MIN_TEXT_WIDTH, columns - GUIDE.length));
        return `${header}${wrapped.map((line) => `${GUIDE}${line}`).join("\n")}`;
      }
      const columns = columnsOf();
      const styled = (option: PromptOption, active: boolean) => {
        if (option.group === true) {
          const name = String(option.value);
          const items = this.getGroupItems(name).filter((item) => !inapplicable.has(item.value));
          const checked = items.filter((item) => values.includes(item.value)).length;
          return renderGroupLine(name, groupState(active, checked, items.length));
        }
        const entry = rowsByValue.get(option.value);
        if (!entry) return option.label;
        const state = rowState(active, values.includes(option.value));
        return renderRowLines(
          entry.row,
          state,
          entry.last,
          columns,
          requiredLabel(inapplicable.get(option.value), rules),
        ).join("\n");
      };
      const footer = formatInstructionFooter(MULTISELECT_INSTRUCTIONS, true);
      const lines = limitOptions({
        options: this.options as PromptOption[],
        cursor: this.cursor,
        output,
        columnPadding: GUIDE.length,
        rowPadding: header.split("\n").length + footer.length + 1,
        style: styled,
      });
      return `${header}${GUIDE}${lines.join(`\n${GUIDE}`)}\n${footer.join("\n")}\n`;
    },
  });
  // Runs after the prompt's own space handler has toggled the row or group
  // under the cursor, and before the frame is redrawn.
  let previous = [...initial];
  prompt.on("cursor", (key) => {
    if (key !== "space") return;
    const next = applySelectionRules(previous, (prompt.value ?? []) as string[], rules);
    prompt.value = next;
    previous = next;
  });
  const result = await prompt.prompt();
  if (isCancel(result)) onCancel();
  return result as string[];
}
