# Context Compaction (F2)

Rolling summarization of old conversation turns to stay within context limits.

## How It Works

1. After each run, check conversation length against the session cap
2. If near cap, `compact_history()` sends the oldest N messages to the LLM as a one-shot summarization chat
3. The summary replaces those messages, embedded as a `[CONTEXT SUMMARY]` marker
4. Recent messages stay intact — only the tail is compressed

## Session Format v2

Compaction summaries persist across restarts via the session file format v2:

```json
{
  "version": 2,
  "summary": "User asked about X; we decided Y...",
  "messages": [...]
}
```

Format v1 (bare message array) is still readable — backward compatible.

## Config

```toml
[gateway]
context_compaction = true
```
