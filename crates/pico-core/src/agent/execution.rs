//! Early, exclusive top-level tool execution over the existing SSE transport.
use super::*;
use crate::provider::{Delta, Response};
use crate::store::InProgressResponse;

pub(super) fn is_tool_call(item: &Value) -> bool {
    matches!(
        item["type"].as_str(),
        Some("function_call" | "custom_tool_call")
    )
}

impl Harness {
    pub(super) async fn respond_and_execute(
        &self,
        conversation: &Arc<Conversation>,
        snapshot: &Session,
        instructions: &str,
        cancel: &CancellationToken,
    ) -> Result<Response> {
        conversation
            .state
            .lock()
            .expect("session lock")
            .in_progress_response = Some(InProgressResponse::default());
        // A failed stream cancels its work without masquerading as a user Stop.
        let work_cancel = cancel.child_token();
        let (items, mut received) = tokio::sync::mpsc::unbounded_channel();
        let emit = |delta| match delta {
            Delta::OutputItem { index, item } => {
                // The request future is scoped to this receiver; it cannot outlive it.
                items.send((index, item)).expect("live response consumer");
            }
            Delta::Status(status) => {
                *conversation.status.lock().expect("status lock") = status;
                conversation.changed();
            }
            Delta::Text {
                id,
                text,
                reasoning,
            } => {
                *conversation.status.lock().expect("status lock") = if reasoning {
                    "Thinking…"
                } else {
                    "Writing…"
                }
                .into();
                let mut session = conversation.state.lock().expect("session lock");
                if let Some(entry) = session.entries.by_id_mut(&id) {
                    entry.text.push_str(&text);
                } else {
                    session.entries.push(Entry {
                        id,
                        text,
                        kind: if reasoning {
                            EntryKind::Reasoning
                        } else {
                            EntryKind::Assistant
                        },
                    });
                }
                conversation.changed();
            }
        };
        let request = self
            .provider
            .respond(snapshot, instructions, &work_cancel, &emit);
        tokio::pin!(request);
        let mut response = None;
        let mut next_index = 0;
        let mut active = None;
        let mut changes = conversation.changes.subscribe();
        let mut tool_finished = false;
        let result = async {
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => bail!("Turn cancelled"),
                    result = &mut request, if response.is_none() => {
                        let completed = result?;
                        {
                            let mut session = conversation.state.lock().expect("session lock");
                            // Final-response fallback items are already in the channel.
                            // Include them in this same checkpoint, not in a later save.
                            let progress = session.in_progress_response.as_mut().expect("active response");
                            while let Ok((index, item)) = received.try_recv() {
                                progress.items.insert(index, item);
                            }
                            // Account for a completed model response even if a tool later
                            // fails or the user stops while its invocation is still running.
                            conversation.turn_usage.lock().expect("turn usage lock")
                                .record_response(&completed);
                            session.usage.record_response(completed.usage.clone());
                            reconcile_text(&mut session, &completed.output);
                            conversation.changed();
                        }
                        response = Some(completed);
                        conversation.save_async().await?;
                    }
                    result = async {
                        match &mut active {
                            Some(work) => work.await,
                            None => std::future::pending().await,
                        }
                    } => {
                        active = None;
                        result?;
                        tool_finished = true;
                    }
                    changed = changes.changed() => { changed?; }
                    Some((index, item)) = received.recv() => {
                        record_item(conversation, index, item);
                        // Save the raw call before it can produce any side effects.
                        conversation.save_async().await?;
                    }
                }
                // Preserve the existing post-tool questionnaire barrier, including waits
                // on cells that were already asking when this response began.
                let waiting_for_answers = tool_finished
                    && conversation
                        .questionnaires
                        .as_ref()
                        .is_some_and(|questions| questions.has_pending());
                // Completed items double as an ordered queue. Do not run a later
                // call ahead of an earlier position whose completed item is still missing.
                while active.is_none() && !waiting_for_answers {
                    let item = conversation
                        .state
                        .lock()
                        .expect("session lock")
                        .in_progress_response
                        .as_ref()
                        .expect("active response")
                        .items
                        .get(&next_index)
                        .cloned();
                    let Some(call) = item else { break };
                    let index = next_index;
                    next_index += 1;
                    if is_tool_call(&call) {
                        active = Some(Box::pin(execute_call(
                            conversation.clone(),
                            index,
                            call,
                            work_cancel.clone(),
                        )));
                    }
                }
                if response
                    .as_ref()
                    .is_some_and(|response| next_index == response.output.len())
                    && received.is_empty()
                    && active.is_none()
                    && !waiting_for_answers
                {
                    return Ok(response.take().expect("completed response"));
                }
            }
        }
        .await;
        if result.is_err() {
            work_cancel.cancel();
            // Include already-delivered completed items even when the stream failed in
            // the same poll. They are recovery context, never permission to run more tools.
            while let Ok((index, item)) = received.try_recv() {
                record_item(conversation, index, item);
            }
            // Never drop a live spawn_blocking invocation: persist its outcome before
            // the caller repairs interrupted calls or marks the turn idle.
            if let Some(work) = active
                && let Err(error) = work.await
            {
                return result.map_err(|primary| primary.context(format!(
                    "Could not settle interrupted tool work: {error:#}. Keep Pico open and fix storage before retrying."
                )));
            }
        }
        result
    }
}

fn record_item(conversation: &Conversation, index: usize, item: Value) {
    let mut session = conversation.state.lock().expect("session lock");
    session
        .in_progress_response
        .as_mut()
        .expect("active response")
        .items
        .insert(index, item);
    conversation.changed();
}

async fn execute_call(
    conversation: Arc<Conversation>,
    index: usize,
    call: Value,
    cancel: CancellationToken,
) -> Result<()> {
    if cancel.is_cancelled() {
        return Ok(());
    }
    // The provider validates completed calls once, before emitting them.
    let name = call["name"]
        .as_str()
        .expect("validated tool name")
        .to_owned();
    let call_id = call["call_id"]
        .as_str()
        .expect("validated call ID")
        .to_owned();
    let custom = call["type"] == "custom_tool_call";
    let input = call[if custom { "input" } else { "arguments" }]
        .as_str()
        .expect("validated tool input")
        .to_owned();
    *conversation.status.lock().expect("status lock") = format!("Running {name}…");
    {
        let mut session = conversation.state.lock().expect("session lock");
        session.entries.push(Entry {
            id: call_id.clone(),
            text: String::new(),
            kind: EntryKind::Tool {
                name: name.clone(),
                input: input.clone(),
                output: String::new(),
                complete: false,
                failed: false,
                cell_id: None,
                parent: None,
                media: vec![],
                web: None,
            },
        });
        conversation.changed();
    }
    conversation.save_async().await?;
    let conv = conversation.clone();
    let token = cancel.clone();
    let parent = call_id.clone();
    let result = tokio::task::spawn_blocking(move || match name.as_str() {
        "exec" => conv.code.exec_output(&input, &parent, &token),
        "wait" => conv.code.wait_output(&input, &token),
        _ => bail!("Unknown tool {name}; only exec and wait are top-level tools"),
    })
    .await?;
    let output = result.unwrap_or_else(|error| {
        let mut output = crate::media::Output::text(format!("Tool error: {error:#}"));
        output.failed = true;
        output
    });
    {
        let mut session = conversation.state.lock().expect("session lock");
        let entry = session
            .entries
            .by_id_mut(&call_id)
            .expect("started tool entry");
        let EntryKind::Tool {
            output: result,
            media,
            complete,
            failed,
            cell_id,
            ..
        } = &mut entry.kind
        else {
            unreachable!("started tool entry");
        };
        *failed = output.failed;
        *cell_id = output.cell_id.clone();
        *result = output.summary();
        *media = output.media();
        *complete = true;
        session.in_progress_response.as_mut().expect("active response").results.insert(index,
            json!({"type":if custom { "custom_tool_call_output" } else { "function_call_output" },
                "call_id":call_id,"output":output.history()}));
        conversation.changed();
    }
    conversation.save_async().await?;
    Ok(())
}

#[cfg(test)]
mod tests;
