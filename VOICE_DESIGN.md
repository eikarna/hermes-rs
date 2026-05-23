# Rust-Native Voice Mode Design

Hermes should add low-latency voice interactions without a Python sidecar or external voice framework. The voice runtime should live in Rust and reuse the existing Hermes agent loop for conversation state, tool execution, workspace context, memory, and streaming assistant events.

## Current Runtime

The first runtime slice is a Rust frame pipeline with a local transcript transport:

- `VoiceFrame` carries input audio, user transcript deltas/finals, assistant text deltas/finals, output audio, interruptions, errors, and shutdown.
- `VoiceInput` and `VoiceOutput` abstract transports.
- `SpeechToText` and `TextToSpeech` abstract audio adapters.
- `VoiceResponder` connects transcript turns to `HermesAgent`.
- `HermesVoiceResponder` uses `HermesAgent::with_events` so streamed `AgentEvent::Content` deltas can flow through the voice pipeline before the final assistant message is complete.

Run the current text transport with:

```toml
[voice]
enabled = true
transport = "local"
allow_interruptions = true
```

```bash
hermes voice
```

Type one transcript per turn. Use `/interrupt` to send an interruption frame and `/quit` to end the session.

## Goals

- Keep voice mode optional so existing `run`, `chat`, TUI, and autonomous flows work without audio dependencies.
- Capture and play audio through Rust interfaces.
- Support local microphone/headphone sessions first, then networked WebRTC sessions.
- Stream assistant text deltas from `AgentEvent::Content` into TTS as soon as they arrive.
- Preserve interruption handling so user speech can cancel queued or active assistant audio.

## Non-Goals

- Do not add a Python sidecar or external voice runtime.
- Do not require voice dependencies for non-voice Hermes commands.
- Do not expose client-side API keys through direct browser-to-model voice paths.

## Proposed Architecture

```text
local microphone or WebRTC peer
        |
        v
Rust voice transport
        |
        v
audio input -> turn detection -> speech-to-text
        |
        v
HermesAgent + ToolRegistry + workspace context
        |
        v
assistant deltas -> text-to-speech -> audio output
```

The first Rust API surface should be trait-based:

- `AudioInput`: reads fixed-size PCM frames from a local device or network transport.
- `AudioOutput`: writes synthesized PCM frames to a local device or network transport.
- `TurnDetector`: emits speech start, speech end, and interruption events.
- `SpeechToText`: converts completed or partial speech turns into user text.
- `TextToSpeech`: converts assistant text deltas into audio frames.
- `VoiceSession`: owns the event loop that connects these traits to `HermesAgent`.

`VoiceSession` should reuse `HermesAgent::with_events` so the runtime can forward streamed assistant content to TTS before the final assistant message is complete.

## Transport Defaults

The default config uses `local` while the implemented runtime is transcript-backed. `webrtc` should become the default once remote duplex media exists. `websocket` is kept for controlled server-to-server experiments and should not be the preferred browser voice transport.

## PR Sequence

1. Add TOML config and design docs for the Rust-native voice surface.
2. Add core voice traits and frame/event types in `hermes-core`.
3. Add a local `hermes voice` loop with transcript input to validate turn flow.
4. Add local audio-device input/output behind the existing traits.
5. Add STT/TTS adapters behind Rust traits.
6. Add WebRTC transport and signaling after local voice works end to end.
7. Add TUI status and controls after the standalone voice command is stable.

## Current Config Surface

```toml
[voice]
enabled = false
transport = "local"
bind_addr = "127.0.0.1:8787"
# input_device = "default"
# output_device = "default"
sample_rate_hz = 48000
frame_ms = 20
allow_interruptions = true
```
