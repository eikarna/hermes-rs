# Voice Note STT (F4)

Telegram voice notes are transcribed to text before hitting the agent.

## How It Works

1. Incoming message with a `voice` attachment detected
2. `getFile` API → download the `.oga` audio via the bot token
3. POST multipart to `/v1/audio/transcriptions` (OpenAI-compatible endpoint, same credentials as the primary client)
4. Transcript injected as the user message text

## Config

OFF unless `stt_model` is set:

```toml
[gateway]
stt_model = "gemini/gemini-2.5-flash"
```

Any model the provider supports on the transcriptions endpoint works.
