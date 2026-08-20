# Tool Approval

Interactive approval gate for tool execution via Telegram inline keyboards.

## How It Works

1. Agent wants to execute a tool
2. Gateway sends a prompt with `[✅ Approve] [❌ Deny]` inline buttons
3. User taps a button → Telegram sends a `callback_query`
4. The query is routed to a per-tool-call `oneshot` channel
5. Agent proceeds (approve) or skips with a denial message (deny)
6. Timeout → treated as denial

## Config

```toml
[gateway]
tool_approval = true
```

## Implementation

- `kerux-core/src/approval.rs` — approval gate manager
- `callback_query` handling in `TelegramAdapter::poll_updates`
- `answerCallbackQuery` ack so the button press registers in the Telegram UI
