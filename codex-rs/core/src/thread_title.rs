use std::sync::Arc;

use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadNameUpdatedEvent;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tracing::warn;

use crate::Prompt;
use crate::ResponseEvent;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::event_mapping::is_contextual_user_message_content;
use crate::rollout::session_index;

const GENERATED_THREAD_TITLE_PROMPT: &str = include_str!("../templates/thread_title/prompt.md");
const GENERATED_THREAD_TITLE_MAX_LEN: usize = 50;

#[derive(Debug, Deserialize)]
struct GeneratedThreadTitle {
    title: String,
}

pub(crate) async fn maybe_generate_thread_title(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    user_message_text: String,
) {
    let (thread_name, session_source) = sess.thread_title_state().await;
    if thread_name.is_some()
        || matches!(
            session_source,
            codex_protocol::protocol::SessionSource::SubAgent(_)
        )
    {
        return;
    }

    let prompt_input = title_prompt_input(sess.clone_history().await.raw_items());

    let Some(prompt_input) = prompt_input else {
        return;
    };

    let mut client_session = sess.services.model_client.new_session();
    let prompt = Prompt {
        input: prompt_input,
        tools: Vec::new(),
        parallel_tool_calls: false,
        base_instructions: BaseInstructions {
            text: GENERATED_THREAD_TITLE_PROMPT.to_string(),
        },
        personality: None,
        output_schema: Some(json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" }
            },
            "required": ["title"],
            "additionalProperties": false
        })),
    };

    let mut stream = match client_session
        .stream(
            &prompt,
            &turn_context.model_info,
            &turn_context.session_telemetry,
            /*effort*/ None,
            turn_context.reasoning_summary,
            turn_context.config.service_tier,
            /*turn_metadata_header*/ None,
        )
        .await
    {
        Ok(stream) => stream,
        Err(err) => {
            warn!(error = %err, "failed to start generated thread title request");
            return;
        }
    };

    let mut result = String::new();
    while let Some(event) = stream.next().await {
        match event {
            Ok(ResponseEvent::OutputTextDelta(delta)) => result.push_str(&delta),
            Ok(ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }))
                if result.is_empty() =>
            {
                if let Some(text) = crate::compact::content_items_to_text(&content) {
                    result.push_str(&text);
                }
            }
            Ok(ResponseEvent::Completed { .. }) => break,
            Ok(_) => {}
            Err(err) => {
                warn!(error = %err, "generated thread title request failed");
                return;
            }
        }
    }

    let title = serde_json::from_str::<GeneratedThreadTitle>(&result)
        .ok()
        .and_then(|output| sanitize_generated_thread_title(&output.title))
        .or_else(|| sanitize_generated_thread_title(&result));
    let Some(title) = title else {
        return;
    };

    if title == strip_user_message_prefix(user_message_text.as_str()) {
        return;
    }

    if let Err(err) = persist_thread_name(&sess, turn_context.sub_id.clone(), title).await {
        warn!(error = %err, "failed to persist generated thread title");
    }
}

pub(crate) async fn persist_thread_name(
    sess: &Arc<Session>,
    event_id: String,
    name: String,
) -> anyhow::Result<()> {
    let Some(name) = crate::util::normalize_thread_name(&name) else {
        anyhow::bail!("thread name cannot be empty");
    };

    let persistence_enabled = {
        let rollout = sess.services.rollout.lock().await;
        rollout.is_some()
    };
    if !persistence_enabled {
        anyhow::bail!("session persistence is disabled");
    }

    let codex_home = sess.codex_home().await;
    session_index::append_thread_name(&codex_home, sess.conversation_id, &name).await?;

    sess.set_session_thread_name(Some(name.clone())).await;

    sess.send_event_raw(Event {
        id: event_id,
        msg: EventMsg::ThreadNameUpdated(ThreadNameUpdatedEvent {
            thread_id: sess.conversation_id,
            thread_name: Some(name),
        }),
    })
    .await;
    Ok(())
}

fn title_prompt_input(items: &[ResponseItem]) -> Option<Vec<ResponseItem>> {
    let mut first_user_index = None;
    let mut user_message_count = 0usize;

    for (idx, item) in items.iter().enumerate() {
        let ResponseItem::Message { role, content, .. } = item else {
            continue;
        };
        if role == "user" && !is_contextual_user_message_content(content) {
            user_message_count += 1;
            if first_user_index.is_none() {
                first_user_index = Some(idx);
            }
        }
    }

    let first_user_index = first_user_index?;
    if user_message_count != 1 {
        return None;
    }

    Some(items[..=first_user_index].to_vec())
}

fn sanitize_generated_thread_title(raw: &str) -> Option<String> {
    let title = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .trim_matches('"')
        .trim();
    if title.is_empty() {
        return None;
    }

    let truncated = if title.chars().count() > GENERATED_THREAD_TITLE_MAX_LEN {
        title
            .chars()
            .take(GENERATED_THREAD_TITLE_MAX_LEN.saturating_sub(3))
            .collect::<String>()
            + "..."
    } else {
        title.to_string()
    };
    crate::util::normalize_thread_name(&truncated)
}

fn strip_user_message_prefix(text: &str) -> &str {
    match text.find(codex_protocol::protocol::USER_MESSAGE_BEGIN) {
        Some(idx) => text[idx + codex_protocol::protocol::USER_MESSAGE_BEGIN.len()..].trim(),
        None => text.trim(),
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_generated_thread_title;
    use super::title_prompt_input;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;
    use pretty_assertions::assert_eq;

    fn user_message(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase: None,
        }
    }

    fn developer_message(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "developer".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase: None,
        }
    }

    fn assistant_message(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
                annotations: Vec::new(),
            }],
            end_turn: None,
            phase: None,
        }
    }

    #[test]
    fn title_prompt_input_uses_only_first_real_user_turn() {
        let items = vec![
            developer_message("context"),
            user_message("first prompt"),
            assistant_message("reply"),
        ];

        let prompt_input = title_prompt_input(&items).expect("prompt input");

        assert_eq!(prompt_input, items[..=1].to_vec());
    }

    #[test]
    fn title_prompt_input_skips_follow_up_turns() {
        let items = vec![user_message("first"), user_message("second")];

        assert_eq!(title_prompt_input(&items), None);
    }

    #[test]
    fn sanitize_generated_thread_title_keeps_first_non_empty_line() {
        assert_eq!(
            sanitize_generated_thread_title("\n  Investigate flaky test failures  \nextra"),
            Some("Investigate flaky test failures".to_string())
        );
    }

    #[test]
    fn sanitize_generated_thread_title_truncates_long_output() {
        assert_eq!(
            sanitize_generated_thread_title(
                "This title is definitely much longer than fifty characters for the sidebar"
            ),
            Some("This title is definitely much longer than fi...".to_string())
        );
    }
}
