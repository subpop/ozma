pub mod command_palette;
pub mod conversation_view;
pub mod home;
pub mod input;
pub mod model_picker;
pub mod param_input;
pub mod persona_picker;
pub mod session_list;

use color_eyre::eyre::Result;
use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::action::Action;
use crate::event::Event;

/// Trait for UI components that encapsulate their own state, event handling, and rendering.
///
/// Each component in the application implements this trait. The application dispatches
/// events and actions to components, and components render themselves to the terminal.
pub trait Component {
    /// Called once when the component is first registered. Use for one-time setup.
    fn init(&mut self) -> Result<()> {
        Ok(())
    }

    /// Handle a raw event and return an action. The default implementation dispatches
    /// to `handle_key_events` or `handle_mouse_events` as appropriate.
    fn handle_event(&mut self, event: &Event) -> Action {
        match event {
            Event::Quit => Action::Quit,
            Event::Tick => Action::Tick,
            Event::Render => Action::Render,
            Event::Key(key_event) => self.handle_key_event(*key_event),
            Event::Mouse(mouse_event) => self.handle_mouse_event(*mouse_event),
            Event::Resize(w, h) => Action::Resize(*w, *h),
            Event::StreamChunk(chunk) => Action::AppendAssistantChunk(chunk.clone()),
            Event::StreamDone { usage } => Action::AssistantMessageComplete {
                usage: usage.clone(),
            },
            Event::StreamDoneWithToolCalls { tool_calls, usage } => Action::ToolCallsReceived {
                tool_calls: tool_calls.clone(),
                usage: usage.clone(),
            },
            Event::StreamError(err) => Action::StreamError(err.clone()),
            Event::ToolExecutionResult {
                call_id,
                fn_name,
                content,
                success,
                blobs,
            } => Action::ToolResultReady {
                call_id: call_id.clone(),
                fn_name: fn_name.clone(),
                content: content.clone(),
                success: *success,
                blobs: blobs.clone(),
            },
            Event::CompactionResult(Ok((summary, ids, usage))) => Action::CompactionComplete {
                summary: summary.clone(),
                compacted_ids: ids.clone(),
                usage: usage.clone(),
            },
            Event::CompactionResult(Err(err)) => Action::CompactionError(err.clone()),
            Event::TitleGenerated(Ok((title, usage))) => Action::TitleGenerated {
                title: title.clone(),
                usage: usage.clone(),
            },
            Event::TitleGenerated(Err(err)) => {
                tracing::warn!("Title generation failed: {}", err);
                Action::Noop
            }
            Event::PricingLoaded(catalog) => Action::PricingLoaded(catalog.clone()),
            Event::ProviderModelsFetched(name, models) => {
                Action::ProviderModelsFetched(name.clone(), models.clone())
            }
            Event::Paste(text) => Action::PasteText(text.clone()),
            Event::AcpPermissionRequest {
                request_id,
                call_id: _,
                tool_name,
                permission_level,
            } => Action::ToolPermissionRequest {
                call_id: request_id.clone(),
                tool_name: tool_name.clone(),
                arguments: serde_json::json!({}),
                permission_level: permission_level.clone(),
            },
            Event::AcpToolCall(info) => Action::AcpToolCallDisplay(info.clone()),
            Event::AcpToolCallUpdate(update) => Action::AcpToolCallUpdateDisplay(update.clone()),
            Event::AcpUserMessageChunk(text) => Action::AcpUserMessageReplay(text.clone()),
            Event::AcpSessionTitle(title) => Action::TitleGenerated {
                title: title.clone(),
                usage: None,
            },
            Event::AcpUsageUpdate(usage) => Action::AcpUsageUpdate(usage.clone()),
            Event::AcpSessionLoadComplete => Action::AcpSessionLoadComplete,
            _ => Action::Noop,
        }
    }

    /// Handle a key event. Override this in your component.
    fn handle_key_event(&mut self, _key: KeyEvent) -> Action {
        Action::Noop
    }

    /// Handle a mouse event. Override this in your component.
    fn handle_mouse_event(&mut self, _mouse: MouseEvent) -> Action {
        Action::Noop
    }

    /// Process an action and optionally return a follow-up action.
    /// Called after handle_event, and recursively for any returned actions.
    fn update(&mut self, _action: &Action) -> Option<Action> {
        None
    }

    /// Render the component to the given frame area.
    fn render(&mut self, frame: &mut Frame, area: Rect);
}
