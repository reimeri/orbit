//! Early, exclusive top-level tool execution with recoverable Responses streams.
use super::*;
use crate::provider::{Delta, Response};
use crate::store::InProgressResponse;

const MAX_RECOVERIES: u8 = 3;

#[derive(Default)]
pub(super) struct Recovery {
    attempts: u8,
    notice: Option<String>,
}
impl Recovery {
    fn notice(&mut self, conversation: &Conversation, message: String) {
        let mut session = conversation.state.lock().expect("session lock");
        if let Some(id) = &self.notice {
            session.entries.by_id_mut(id).expect("recovery notice").text = message;
        } else {
            let entry = Entry::new(EntryKind::Notice, message);
            self.notice = Some(entry.id.clone());
            session.entries.push(entry);
        }
        conversation.changed();
    }

    pub(super) async fn retry(
        &mut self,
        conversation: &Arc<Conversation>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if self.attempts == MAX_RECOVERIES {
            self.notice(
                conversation,
                "Reconnection attempts exhausted. Saved progress is retained.".into(),
            );
            bail!(
                "Could not keep a connection to ChatGPT. Check your network, then send a message to continue from saved progress."
            );
        }
        self.attempts += 1;
        self.notice(conversation, format!("Connection lost. Reconnecting from saved progress (attempt {} of {MAX_RECOVERIES})…", self.attempts));
        conversation.set_status(ConversationStatus::Connecting);
        conversation.save_async().await?;
        let mut changes = conversation.changes.subscribe();
        while conversation
            .questionnaires
            .as_ref()
            .is_some_and(|q| q.has_pending())
        {
            tokio::select! { _ = cancel.cancelled() => bail!("Turn cancelled"), result = changes.changed() => result? }
        }
        tokio::select! {
            _ = cancel.cancelled() => bail!("Turn cancelled"),
            _ = tokio::time::sleep(Duration::from_secs(1 << (self.attempts - 1))) => Ok(())
        }
    }
    pub(super) fn stopped(&mut self, conversation: &Conversation, cancelled: bool) {
        if self.notice.is_some() && (cancelled || self.attempts < MAX_RECOVERIES) {
            self.notice(
                conversation,
                if cancelled {
                    "Reconnection stopped."
                } else {
                    "Could not continue automatically."
                }
                .into(),
            );
        }
        self.notice = None;
    }
    pub(super) fn resumed(&mut self, conversation: &Conversation) {
        if self.notice.is_some() {
            self.notice(
                conversation,
                "Reconnected. Continued from saved progress.".into(),
            );
            self.notice = None;
        }
    }
}

impl Harness {
    pub(super) async fn respond_and_execute(
        &self,
        conversation: &Arc<Conversation>,
        snapshot: &Session,
        instructions: &str,
        cancel: &CancellationToken,
        recovery: &mut Recovery,
    ) -> Result<(Response, Option<String>)> {
        conversation
            .state
            .lock()
            .expect("session lock")
            .in_progress_response = Some(InProgressResponse::default());
        let work_cancel = cancel.child_token();
        let (items, mut received) = tokio::sync::mpsc::unbounded_channel();
        let emit = |delta| match delta {
            Delta::OutputItem { index, item } => {
                // The request future is scoped to this receiver; it cannot outlive it.
                items.send((index, item)).expect("live response consumer");
            }
            Delta::Retrying(status) => {
                conversation.set_status(ConversationStatus::Retrying(status));
                conversation.changed();
            }
            Delta::Text {
                id,
                text,
                reasoning,
            } => {
                conversation.set_status(if reasoning {
                    ConversationStatus::Thinking
                } else {
                    ConversationStatus::Writing
                });
                let mut session = conversation.state.lock().expect("session lock");
                let id = response_row(&mut session, &id);
                if let Some(entry) = session.entries.by_id_mut(&id) {
                    entry.text.push_str(&text);
                } else {
                    session.entries.push(Entry {
                        id,
                        text,
                        kind: if reasoning {
                            EntryKind::Reasoning
                        } else {
                            EntryKind::Reply { final_at: None }
                        },
                    });
                }
                conversation.changed();
            }
        };
        let request = async {
            futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
                self.provider
                    .respond(snapshot, instructions, &work_cancel, &emit),
            ))
            .await
            .unwrap_or_else(|_| {
                Err(anyhow::anyhow!(
                    "Provider task panicked. Inspect completed tool work before continuing."
                ))
            })
        };
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
                        let last_answer = {
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
                            conversation.pending_activity.lock().expect("activity lock")
                                .push(crate::store::activity::Sample::new(snapshot, &completed));
                            let answer = reconcile_text(&mut session, &completed.output);
                            conversation.changed();
                            answer
                        };
                        response = Some((completed, last_answer));
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
                    .is_some_and(|(response, _)| next_index == response.output.len())
                    && received.is_empty()
                    && active.is_none()
                    && !waiting_for_answers
                {
                    return Ok(response.take().expect("completed response"));
                }
            }
        }.await;
        if result.is_err() {
            let dropped = result
                .as_ref()
                .is_err_and(|error| error.is::<crate::provider::StreamInterrupted>())
                && !cancel.is_cancelled();
            if !dropped {
                work_cancel.cancel();
            }
            // Include already-delivered completed items even when the stream failed in
            // the same poll. They are recovery context, never permission to run more tools.
            while let Ok((index, item)) = received.try_recv() {
                record_item(conversation, index, item);
            }
            // Never drop a live spawn_blocking invocation: persist its outcome before
            // the caller repairs interrupted calls or marks the turn idle.
            if let Some(work) = active {
                let started = {
                    let session = conversation.state.lock().expect("session lock");
                    let call = &session
                        .in_progress_response
                        .as_ref()
                        .expect("active response")
                        .items[&(next_index - 1)];
                    session
                        .entries
                        .by_id(call["call_id"].as_str().expect("validated call"))
                        .is_some()
                };
                if dropped && started {
                    recovery.notice(
                        conversation,
                        "Connection lost. Waiting for the running tool before reconnecting…".into(),
                    );
                }
                if !dropped || started {
                    // User Stop still reaches this child token. Do not cancel a working
                    // invocation just because the model connection disappeared.
                    work.await.context("Could not settle interrupted tool work. Inspect tool results and fix storage before continuing")?;
                }
            }
            if dropped && !cancel.is_cancelled() {
                let mut session = conversation.state.lock().expect("session lock");
                let progress = session
                    .in_progress_response
                    .as_ref()
                    .expect("active response");
                let completed_ids: HashSet<_> = progress
                    .items
                    .values()
                    .filter_map(|item| {
                        progress
                            .text_rows
                            .get(item["id"].as_str()?)
                            .map(String::as_str)
                    })
                    .collect();
                let partial: Vec<_> = session
                    .entries
                    .range(snapshot.entries.len())
                    .filter(|e| {
                        (matches!(e.kind, EntryKind::Reply { .. } | EntryKind::Reasoning))
                            && !completed_ids.contains(e.id.as_str())
                    })
                    .map(|e| e.id.clone())
                    .collect();
                let pending: Vec<_> = progress
                    .items
                    .iter()
                    .filter(|(index, item)| {
                        is_tool_call(item) && !progress.results.contains_key(index)
                    })
                    .map(|(i, item)| (*i, item.clone()))
                    .collect();
                for id in partial {
                    let entry = session.entries.by_id_mut(&id).expect("partial entry");
                    entry.kind = EntryKind::Notice;
                    entry
                        .text
                        .insert_str(0, "Interrupted partial output (not sent on retry):\n");
                }
                for (index, item) in pending {
                    let id = item["call_id"].as_str().expect("validated call");
                    if session.entries.by_id(id).is_some() {
                        bail!(
                            "Connection lost during tool {id}. Its outcome is unknown. Inspect its effects before continuing."
                        );
                    }
                    let kind = if item["type"] == "custom_tool_call" {
                        "custom_tool_call_output"
                    } else {
                        "function_call_output"
                    };
                    session.in_progress_response.as_mut().expect("active response").results.insert(index,
                        json!({"type":kind, "call_id":id, "output":"Not executed: the connection was lost before this call was dispatched."}));
                }
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
    // The provider validates completed calls before emitting them.
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
    conversation.set_status(ConversationStatus::RunningTool(name.clone()));
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
                command_status: None,
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
    let result = tokio::task::spawn_blocking(move || {
        conv.code
            .invoke(&name, &input, &parent, &token, &conv.live_calls)
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
