import { Effect } from "effect";

/**
 * The `metadata.source` value on every prompt AFT admits into a session itself
 * (background-completion wakes). OpenCode 2 runs every prompt, AFT's own
 * included, through the session `prompt` hook; the hook reads this marker to
 * tell AFT's machine-written text from text an operator typed, never the text.
 */
export const AFT_PROMPT_SOURCE = "aft";

/** True when a prompt's metadata marks it as one AFT admitted itself. */
export function isAftOriginatedPrompt(metadata: unknown): boolean {
  return (
    typeof metadata === "object" &&
    metadata !== null &&
    (metadata as { source?: unknown }).source === AFT_PROMPT_SOURCE
  );
}

export interface V2SessionPromptInput {
  readonly sessionID: string;
  readonly text: string;
  readonly delivery: "steer";
  readonly metadata?: Readonly<Record<string, unknown>>;
  readonly resume?: boolean;
}

export interface V2SessionSyntheticInput {
  readonly sessionID: string;
  readonly text: string;
  readonly description?: string;
  readonly metadata?: Readonly<Record<string, unknown>>;
  readonly resume: false;
}

export interface V2SessionAdmission {
  prompt(input: V2SessionPromptInput): Effect.Effect<unknown, unknown>;
  synthetic(input: V2SessionSyntheticInput): Effect.Effect<unknown, unknown>;
}

export interface CompletionWake {
  readonly sessionID: string;
  readonly taskIDs: readonly string[];
  readonly text: string;
  readonly metadata?: Readonly<Record<string, unknown>>;
}

export interface StatusOnlyUpdate {
  readonly sessionID: string;
  readonly text: string;
  readonly description?: string;
  readonly metadata?: Readonly<Record<string, unknown>>;
}

/**
 * Admit V2 session updates through the host's native inbox. Completion text is
 * model-directed, while status records are synthetic and never resume a turn.
 */
export class V2SessionDelivery {
  private readonly admittedCompletions = new Set<string>();
  private disposed = false;

  constructor(private readonly session: V2SessionAdmission) {}

  completion(input: CompletionWake): Effect.Effect<boolean, unknown> {
    return Effect.suspend(() => {
      if (this.disposed) return Effect.succeed(false);
      const keys = [...new Set(input.taskIDs)].map((taskID) => `${input.sessionID}\0${taskID}`);
      if (keys.length === 0 || keys.every((key) => this.admittedCompletions.has(key))) {
        return Effect.succeed(false);
      }
      for (const key of keys) this.admittedCompletions.add(key);

      return this.session
        .prompt({
          sessionID: input.sessionID,
          text: input.text,
          delivery: "steer",
          metadata: {
            ...input.metadata,
            // Set last so no caller metadata can drop the marker the prompt
            // hook relies on to treat this wake as machine-written.
            source: AFT_PROMPT_SOURCE,
            task_ids: [...new Set(input.taskIDs)],
          },
        })
        .pipe(Effect.map(() => true));
    });
  }

  status(input: StatusOnlyUpdate): Effect.Effect<void, unknown> {
    return Effect.suspend(() => {
      if (this.disposed) return Effect.succeed(undefined);
      return this.session
        .synthetic({
          sessionID: input.sessionID,
          text: input.text,
          description: input.description,
          metadata: input.metadata,
          resume: false,
        })
        .pipe(Effect.asVoid);
    });
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.admittedCompletions.clear();
  }
}
