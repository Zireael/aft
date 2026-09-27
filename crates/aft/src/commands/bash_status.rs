use crate::bash_background::output::RUNNING_OUTPUT_PREVIEW_BYTES;
use crate::bash_background::persistence::BgMode;
use crate::bash_background::registry::BgTaskSnapshot;
use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};
use base64::Engine;
use serde::Deserialize;
use serde_json::json;

const PREVIEW_BYTES: usize = RUNNING_OUTPUT_PREVIEW_BYTES;

pub(crate) const UNKNOWN_TASK_GUIDANCE: &str = "Task IDs only come from a bash tool result or completion notice. If you never received one, the command was not promoted — re-run the command instead of polling.";

pub(crate) fn format_unknown_task_message(task_id: &str) -> String {
    format!("background task not found: {task_id}. {UNKNOWN_TASK_GUIDANCE}")
}

pub(crate) fn format_erased_task_message(task_id: &str) -> String {
    format!(
        "background task row was erased: {task_id}. A recently armed watch still references this task; stop polling this ID and treat the missing row as a terminal storage failure."
    )
}

#[derive(Debug, Deserialize)]
struct BashStatusParams {
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    output_mode: Option<String>,
    #[serde(default)]
    output_offset: Option<u64>,
    #[serde(default)]
    stderr_offset: Option<u64>,
}

pub fn handle(req: &RawRequest, ctx: &AppContext) -> Response {
    let raw_params = req
        .params
        .get("params")
        .cloned()
        .unwrap_or_else(|| req.params.clone());
    let params = match serde_json::from_value::<BashStatusParams>(raw_params) {
        Ok(params) => params,
        Err(e) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("bash_status: invalid params: {e}"),
            );
        }
    };

    let output_mode = params.output_mode.clone();
    let output_offset = params.output_offset;
    let stderr_offset = params.stderr_offset;
    let Some(task_id) = params.task_id else {
        return Response::error(&req.id, "invalid_request", "bash_status: missing task_id");
    };

    if let Some(output_mode) = output_mode.as_deref() {
        if !matches!(output_mode, "screen" | "raw" | "both") {
            return Response::error(
                &req.id,
                "invalid_request",
                "bash_status: output_mode must be one of screen, raw, or both",
            );
        }
    }

    let storage_dir = crate::bash_background::storage_dir(ctx.config().storage_dir.as_deref());
    if ctx.bash_background().has_erased_watch_reference(&task_id) {
        return Response::error(&req.id, "task_erased", format_erased_task_message(&task_id));
    }
    match ctx.bash_background().status(
        &task_id,
        req.session(),
        ctx.config().project_root.as_deref(),
        Some(&storage_dir),
        PREVIEW_BYTES,
    ) {
        Some(mut snapshot) => {
            let pty_raw = maybe_render_pty_screen(
                ctx,
                req.session(),
                &task_id,
                &mut snapshot,
                output_mode.as_deref(),
            );
            if snapshot.sandbox_native
                && snapshot.sandbox_unavailable
                && snapshot.exit_code == Some(crate::sandbox_spawn::SANDBOX_UNAVAILABLE_EXIT_CODE)
            {
                Response::error_with_data(
                    &req.id,
                    "sandbox_unavailable",
                    "native sandbox failed before the command could run; set sandbox.enabled=false to disable native sandboxing",
                    json!({
                        "task_id": snapshot.info.task_id,
                        "exit_code": snapshot.exit_code,
                        "output_preview": snapshot.output_preview,
                    }),
                )
            } else {
                let mode = snapshot.info.mode.clone();
                let mut data = json!(snapshot);
                if matches!(output_mode.as_deref(), Some("raw" | "both")) {
                    if let Some(raw) = pty_raw {
                        data["pty_raw"] = json!(String::from_utf8_lossy(&raw));
                    }
                }
                if let Some(offset) = output_offset {
                    let artifact = if mode == BgMode::Pty {
                        crate::bash_background::persistence::TaskArtifact::Pty
                    } else {
                        crate::bash_background::persistence::TaskArtifact::Stdout
                    };
                    match ctx.bash_background().read_artifact_range(
                        &task_id,
                        req.session(),
                        artifact,
                        offset,
                    ) {
                        Ok((bytes, next)) => {
                            data["output_chunk_base64"] =
                                json!(base64::engine::general_purpose::STANDARD.encode(bytes));
                            data["output_next_offset"] = json!(next);
                        }
                        Err(error) => {
                            return Response::error(
                                &req.id,
                                "artifact_refused",
                                format!("bash_status: task output refused: {error}"),
                            );
                        }
                    }
                }
                if mode == BgMode::Pipes {
                    if let Some(offset) = stderr_offset {
                        match ctx.bash_background().read_artifact_range(
                            &task_id,
                            req.session(),
                            crate::bash_background::persistence::TaskArtifact::Stderr,
                            offset,
                        ) {
                            Ok((bytes, next)) => {
                                data["stderr_chunk_base64"] =
                                    json!(base64::engine::general_purpose::STANDARD.encode(bytes));
                                data["stderr_next_offset"] = json!(next);
                            }
                            Err(error) => {
                                return Response::error(
                                    &req.id,
                                    "artifact_refused",
                                    format!("bash_status: task stderr refused: {error}"),
                                );
                            }
                        }
                    }
                }
                Response::success(&req.id, data)
            }
        }
        None => Response::error(
            &req.id,
            "task_not_found",
            format_unknown_task_message(&task_id),
        ),
    }
}

fn maybe_render_pty_screen(
    ctx: &AppContext,
    session_id: &str,
    task_id: &str,
    snapshot: &mut BgTaskSnapshot,
    output_mode: Option<&str>,
) -> Option<Vec<u8>> {
    if snapshot.info.mode != BgMode::Pty {
        return None;
    }
    match ctx.bash_background().read_artifact(
        task_id,
        session_id,
        crate::bash_background::persistence::TaskArtifact::Pty,
    ) {
        Ok(raw) => {
            if !matches!(output_mode, Some("raw")) {
                let rows = snapshot.pty_rows.unwrap_or(24);
                let cols = snapshot.pty_cols.unwrap_or(80);
                snapshot.pty_screen = Some(crate::pty_render::render_screen(&raw, rows, cols));
            }
            Some(raw)
        }
        Err(error) => {
            snapshot.pty_screen = Some(format!(
                "[PTY screen unavailable: failed to read raw output: {error}]"
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{format_erased_task_message, format_unknown_task_message};

    #[test]
    fn unknown_task_message_steers_agents_to_rerun() {
        assert_eq!(
            format_unknown_task_message("bash-unknown"),
            "background task not found: bash-unknown. Task IDs only come from a bash tool result or completion notice. If you never received one, the command was not promoted — re-run the command instead of polling."
        );
    }

    #[test]
    fn erased_task_message_steers_agents_to_stop_polling() {
        let message = format_erased_task_message("bash-erased");
        assert!(message.contains("background task row was erased: bash-erased"));
        assert!(message.contains("stop polling this ID"));
        assert!(!message.contains(super::UNKNOWN_TASK_GUIDANCE));
    }
}
