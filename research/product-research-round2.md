# Kerux — Product Research Round 2: Feature & Gap Discovery

Task: `t_a518e412` · Lanjutan round 1 (`research/product-research-t_2be9e216.md`).
Round 1 menghasilkan 10 ide; semuanya sudah diimplementasikan dan DONE di board
(webhook, plan-first, branching, telemetry HUD, fan-out, harness-as-tool, vision,
mission control, taste, cost guardrails). Round 2 mencari gelombang berikutnya —
berdasarkan kondisi kode SEKARANG dan lanskap tool per Agustus 2026.

---

## Langkah 1 — Ringkasan Proyek (faktual, per 28 Agu 2026)

- Release line `0.3.x`, Rust workspace: `kerux-core` (agent, tools, gateway,
  memory, taste, cost, validation, repomap, scheduler, skills, MCP) +
  `kerux-cli` (TUI ratatui, autonomous mode, wizard, model picker, probe).
- Fitur yang sudah shipped sejak round 1: webhook inbound trigger, plan-first
  mode, session branching (fork/rewind/clone), live telemetry HUD, parallel
  sub-agent fan-out + orchestration panel, external agent harness (ACP-style),
  vision input pipeline, mobile mission control via gateway chat, taste/style
  profile learning (ekstraksi + push/pull), cost guardrails + fallback chain
  (budget per-run/harian, auto-pause, auto-downgrade, soak-tested).
- CLI subcommands terpasang: `run`, `autonomous`, `tools`, `chat`, `serve`,
  `test`, `auth`, `taste`, `runs`, `wizard`, `model`, `screenshot`,
  `providers`, `login/set-api-key/set-bearer-token/list/logout`, `push/pull`.
- Context files loader sudah membaca `AGENTS.md`, `CLAUDE.md`, `.cursorrules`
  (`context_files.rs:29-35`) + prompt-injection scan.
- Skills sudah format `SKILL.md` + YAML front matter (kompatibel Claude Code
  Agent Skills) — `skills.rs:5,125`.
- Git harness internal ada: `checkpoint()`, `snapshot()`, `commit_transaction()`
  (`githarness.rs:96-195`) — dipakai autonomous mode, belum user-facing.

## Langkah 2 — Audit Gap Internal (dari kode, bukan asumsi)

| # | Gap | Bukti di kode | Status |
|---|-----|---------------|--------|
| 1 | Validator engine selesai tapi TIDAK terpasang di loop/CLI | `run_validation_pass` di `validators.rs:271` hanya dipanggil dari test (`validators.rs:426-564`); nol pemanggil di `main.rs`/`agent.rs` | Setengah jadi (sisa round-1 roadmap) |
| 2 | Edit-format metrics tidak persisten & tidak per-model | `edit_metrics.rs` — `EditMetricsTracker` in-memory only; tidak ada save/load, tidak ada pemilihan format per model | Gap |
| 3 | Approval gate biner, tanpa deny-rules / alasan / review | `approval.rs:22-57` — `requires_approval()` + outcome enum; tidak ada rules persist, tidak ada "kenapa ditolak" | Gap vs kompetitor |
| 4 | Scheduler hanya interval `<n><unit>`, tanpa cron expression / one-shot | `scheduler.rs:7-11` — `30m/2h/1d` saja, min 60s | Gap |
| 5 | Memory blocks tanpa trust tagging / asal sumber / masking | `memory.rs:16-80` — importance+tags+pinned ada, source trust tidak ada | Gap keamanan |
| 6 | Tool-call args tidak di-repair saat malformed | grep `repair` di `tools/*.rs` → hanya `serde(rename_all)`; parse gagal = tool gagal | Gap |
| 7 | MCP server di-spawn eager saat connect | `mcp.rs:426-442` — `cmd.spawn()` langsung; tidak ada lazy/deferred | Gap efisiensi |
| 8 | Edit atomik ada, tapi tanpa stale-write guard (mtime check) | `edit_block_tool.rs:4,148` — atomic write-back, tidak ada cek modified-since-read | Gap |
| 9 | Taste belajar dari trajectory saja, tanpa sinyal accept/reject/revert eksplisit | grep `accept\|reject\|revert` di `taste*.rs` → 0 match | Gap |
| 10 | Git checkpoint internal belum jadi `/undo` user-facing | `githarness.rs:96` dipakai autonomous; tidak ada command undo di CLI | Peluang murah |
| 11 | Tidak ada session sharing / live transcript export | grep `share\|relay` di gateway → hanya chat adapter | Gap |
| 12 | `unimplemented!()` di `curator.rs:833` hanya mock test (StubProvider) — bukan gap riil | verifikasi langsung | Non-issue |

## Langkah 3 — Riset Eksternal (19 tool, web search per tool)

### Aider
- v0.86.x: auto-pick edit format per model family, `reasoning_effort` setting,
  auto-test-and-lint loop (lint+test tiap change, self-repair di percakapan sama).
- Architect mode dengan editor-subagent. `/undo` commit rollback.
- Cadence 2026 melambat; bentuk stabil.

### OpenCode
- v1.17.x: session snapshots & revert (termasuk file changes), yolo dari TUI,
  adaptive thinking, stored provider credentials.
- v1.16.x: Agent Skills compat (SKILL.md), glob-pattern bash permissions
  (allow/ask/deny per pola command, last-match-wins), managed configs
  (/etc/opencode, MDM), ACP server.
- v1.15.x: pinned sessions, Scout agent riset repo, `/share` link publik.
- Fix berulang: auto-compaction berulang setelah compaction; session stuck
  setelah cancel; truncation file besar.

### Claude Code
- v2.1.197: Sonnet 5 default, native 1M context.
- Post-mortem Apr 2026 (sinyal besar): 3 bug harness terbaca sebagai "model
  bodoh" — thinking effort turun diam-diam high→medium; cache bug menghapus
  reasoning history tiap turn; /context salah hitung window. Pelajaran:
  regresi harness = regresi model di mata user; user menuntut visibilitas
  effort/compaction.
- Security: `.mcp.json` dari repo tidak auto-spawn (Pending approval di
  workspace untrusted); PermissionDenied hook; PID namespace isolation.
- Fitur: /powerup tutorial, Focus View, Monitor tool, background jobs, remote
  sessions, voice dictation.
- Keluhan: sesi panjang tanpa narasi visible (observability gap); pricing
  ditarik ke Max $100/bulan.

### OpenClaw
- 250k+ bintang, rilis mingguan. Task Brain control plane, Active Memory
  plugin (retrieval dinamis vs MEMORY.md statis), /tasks board chat-native.
- Roadmap v4.0: multi-agent orchestration (supervisor pattern) = fitur paling
  diminta; Plugin SDK v2 (typed, validasi install-time); vector memory built-in.
- Issue panas: filesystem sandboxing #7722; dynamic model discovery #10687;
  masked secrets / agent dilarang baca API key mentah #10659; memory trust
  tagging by source #7707 (defense vs memory poisoning).
- Reliability: gateway memory leak 15.5GB RSS.

### Codex CLI
- v0.130-0.132: remote-control daemon headless, Python SDK publik (concurrent
  turn routing, approval controls), `codex doctor`, resume + output-schema
  untuk chained workflows di CI/CD.
- v0.129: /vim modal editing, resume/fork picker, /hooks browser.
- v0.128: persistent /goal workflows, permission profiles, MultiAgentV2.
- Auto reviewer agent: approval prompt lewat reviewer dulu, tampil status +
  risk level. `--approve-for-me` flag. Import Cursor-managed skills; sync
  Claude/Cursor conversation imports.

### byNara / NaraCLI
- Suite Indonesia; NaraCLI = fork Pi.dev, 6 persona agent, orkestrasi
  auto/manual, 65 on-demand skills, MCP.
- NaraRouter: gateway LLM free-tier (5-7M token/hari, tanpa kartu,
  OpenAI-compatible + Anthropic-compatible /v1/messages). Sinyal: free router
  tier sekarang jadi kanal distribusi coding agent.

### T3Code
- Harness web GUI mengorkestrasi Claude Code/OpenCode/Codex/Cursor; 15k bintang.
- Diff panel untuk diff BESAR (collapsing, scope switching); dev instances
  shareable via Tailscale; QR pairing; iOS git progress overlay; worktree
  isolation per dev-state.
- Issue: interrupt turn tidak membunuh child process (thread tak bisa
  dihentikan). Tema changelog: review diff prioritas; mobile digarap serius.

### Pi (pi.dev)
- Harness minimal: system prompt ~200-1000 token, 4 tool inti
  (read/write/edit/bash), 37+ model. MIT.
- Benchmark internal Databricks: pass rate tertinggi di Opus 4.8 xhigh dengan
  biaya lebih rendah — ~3x lebih sedikit konteks per turn. Minimalisme =
  efisiensi token.
- v0.84: fullscreen TUI, Mermaid+LaTeX rendering di transcript, fix JSONL
  delta kuadratik, atomic JSONL publication via rename.
- Session tree-structured (rewind, side quest, merge summary balik).
  Deliberately skips: MCP, sub-agents, plan mode, permission popups, todos.
  SDK-first: print/JSON/RPC untuk wrapping di cron/verification gates.

### OMP / oh-my-pi
- Fork Pi oleh Can Bölük; batteries-included. 19k bintang, v17.0.7, ~80-100k
  baris Rust core, native Windows. ACP + SDK + NDJSON RPC.
- LSP + DAP debugging di tool surface; subagents; browser automation; memory
  persisten (mnemopi, SQLite lokal).
- GitHub sebagai virtual filesystem: `pr://1428`, `issue://1234`,
  `pr://1428/diff/3` — tanpa tool gh_* khusus.
- `omp commit`: split perubahan unrelated jadi commit atomik berurutan.
- Config inheritance: baca `.claude`, `.cursor`, `.windsurf`, `.gemini`,
  `.codex`, `.cline`, `.github/copilot`, `.vscode` saat first run — migrasi nol.
- `/collab`: session sharing live via relay E2E-encrypted (link + QR).
- 4 strategi compaction; snapcompact on-device.
- Advisor role: model KEDUA membaca setiap turn agen utama, menyuntikkan
  catatan/koncern/hard blocker dengan konteksnya sendiri.
- Mid-turn user steer via wire-only interjection envelope.
- Tuning edit-format per model terukur: Grok 4 Fast -61% token; MiniMax 2.1x
  pass rate saat edit format berhenti melawan model.
- Server-side session stores: Redis, Postgres, MySQL, SQLite.

### Google Antigravity
- I/O 2026 (19 Mei): Antigravity 2.0 — bukan lagi IDE. Lima surface: desktop
  agent app, IDE (demosi), CLI (agy), SDK (pip, Apache 2.0, MCP + 9 lifecycle
  hooks), Managed Agents (satu API call → agen di sandbox Linux terisolasi).
- Technical Director: parent agent decompose task, spawn subagent paralel
  (demo: 93 subagent, OS framework ~12 jam, <$1000).
- Slash commands baru: /goal, /grill-me (clarifying questions dulu),
  /schedule (cron-like recurring), /browser. Voice transcription live.
- BACKLASH keras: forced auto-update menghapus editor & config; thread "V2.0
  is a disaster"; terminal/source-control/Remote-WSL hilang; kuota Ultra $100
  habis dalam menit. Pelajaran: perubahan surface paksa menghancurkan trust;
  user mau surface stabil, lokal, bisa diprediksi.

### n8n
- v2.x: native MCP server (29 Apr) — Claude/Cursor/ChatGPT deskripsikan
  workflow, n8n build+validasi+test-run+self-fix. AI Agent node (loop
  LangChain-style di canvas) dengan memory/tool/vector subnodes, Ollama lokal.
- 2.0: Code node execution isolated by default; canvas spatial clusters; HTTP
  node OAuth PKCE + retry/backoff.
- Roadmap: streaming AI responses, Postgres-backed queue, observability stuck
  jobs, native evaluation harness.
- Pain: agent node infinite loop pada tool call ambigu; memory stateless
  default; webhook-heavy workflow memblokir eksekusi lain.

### DeepSeek Harness
- Rilis 13 Agu 2026, MIT, v0.1 dev preview ("Black Whale"), 33k bintang dalam
  hitungan jam. Tagline: "Everything is a Plugin." Model + Harness = Agent.
- Dibangun di meta-framework Cordis: model, tools, skills, sessions, sandboxes,
  storage, agent LOOP, scheduling, UI — semua plugin swappable dari config.
  Agent loop adalah Cordis Service yang bisa diganti di runtime boundary.
- 4 preset mode: Standard, Minimal (untuk benchmarking), Creator (inspect
  runtime, test plugin in-memory, bikin mode baru secara konversasional),
  + web UI/headless.
- Same-day: DeepSeek-V4-Pro GA (96.4% SWE-bench Verified, #2 di bawah Opus 5);
  harga API naik 50-1100%.

### Zazen
- Fork Freebuff; #4 leaderboard app OpenRouter: 308.2B token/hari, +334.9%
  dalam 7 hari (app coding tercepat tumbuh yang terlacak).
- ZazenCodes: manajemen vault Obsidian dengan agen 4 level (lokal CC/Codex →
  remote → always-on VPS dengan akses HP + git sync → steward vault custom).
  Hermes sebagai Lead Developer via Telegram; workflow agen menggantikan
  script; AI bookkeeper.
- Sinyal: agen + knowledge-base stewardship use case baru; agen always-on
  diakses dari HP.

### Freebuff
- Varian free ad-supported dari Codebuff (YC F24). Klaim 5-10x lebih cepat
  dari Claude Code.
- Multi-agent by default: agen spesialis (context understanding, web research,
  code reviewer pass) terkoordinasi.
- Per-task model routing: DeepSeek V4 Pro utama, V4 Flash untuk low-stakes,
  Gemini 3.1 Flash Lite untuk file-finding/riset. BYOK Claude; GPT-5.4 via
  langganan ChatGPT tersambung.
- 9 subagent spesialis bundled; browser-use subagent built-in tanpa setup MCP.
  Slash: /interview (flush requirements), /plan (spec tertulis), /review, /deploy.
- Freebuff Cloud: app builder browser dengan sandbox + preview dev server.
- Tesis mereka: inferensi 10x lebih murah; agent loop mostly solved; pilihan
  model per-task > loyalitas vendor; subagent = diferensiasi berikutnya;
  eksekusi lokal menang latensi/biaya/privasi.

### CommandCode
- taste-1: model meta proprietary dengan RL kontinu dari perilaku
  accept/reject/edit developer — coding harian = data training, tanpa feedback
  eksplisit. Belajar pnpm-vs-npm, naming, pola arsitektur.
- v1: rewrite runtime. Single permission engine (urutan keputusan fixed),
  5 permission mode (CC-compatible), deny/ask/allow rules, read-only fast path.
- 40+ tool lewat pipeline bersama: schema-driven input REPAIR (tool call
  malformed diperbaiki, bukan gagal), workspace-boundary enforcement,
  stale-write protection dengan atomic writes, output truncation.
- plan_review tool (buka review plan on demand — "first of its kind"); /goal
  supervision; cron scheduled agents; /agents create-by-asking; edit_file
  fuzzy matching + stale-write guards.
- Config: setiap setting CLI /config otomatis muncul di Settings UI. Image
  consent terikat persis pada model vision yang di-approve.

### ZCode
- Harness resmi Z.ai untuk GLM-5.2/5.3. App desktop gratis, ~1 Jul 2026, 8
  rilis dalam 8 hari, kini v3.8.x.
- Goal Mode: `/goal <objective>`, agen self-verify tiap ronde;
  /goal replace|pause|resume. Lima execution mode via Shift+Tab (Default,
  Confirm Before Changes, Auto Edit, Plan Mode, Full Access).
- Remote control HP + channel bot chat; custom subagents (beta); plugin
  marketplace; SSH remote sync skills/plugins; MCP OAuth; Hooks config
  workspace-level; knowledge base proyek dengan retry/timeout; browser
  built-in dengan rekaman video; preview presentasi.
- v3.8.1: manajemen kapabilitas agen global+workspace visual; recommended
  prompts; off-peak quota reset; file references + grouping tool call.
- Fix berulang: task index korup auto-recovery; remote session resend update
  yang terlewat setelah reconnect; provider quota error stop retry.

### Cursor
- Cursor 3 (Apr 2026): Agents Window jadi workspace default, IDE jadi view
  switchable. Backlash forum: "agent comes first and code comes later",
  "return the per-iteration diff", friksi workflow file-heavy.
- Analisis 500+ post Reddit: 55% positif; keluhan terbesar = limit premium
  request (500/bulan habis 1-2 minggu), pricing tak terprediksi (Pro diam-diam
  jadi usage-based; "$28 jadi $500 dalam 3 hari"), concern telemetry,
  autocomplete inkonsisten.
- Forum Agu 2026: model selector memburuk; "asking me questions on every
  turn"; MCP UI buruk; request native dynamic workflows/subagent orchestration
  (parity Claude Code); "no new IDE features since May"; base VSCode stale
  7-8 bulan merusak extension. Composer menulis ulang kode yang tak diminta.
- Tab completion model tetap kekuatan eksklusif (prediksi cursor-jump).

### HelloMinds
- Minds by Animoca Brands: agen AI sovereign always-on persisten tanpa server
  lokal. Positioning Web4/agentic-web. Program investasi $10M untuk builder.
- Agen peran terspesialisasi: Game Designer Mind (prompt satu kalimat →
  konsep game terdesain penuh & seimbang).
- Sinyal: agen persisten role-specific sebagai produk; spesialisasi vertikal
  di atas coding umum.

### Hermes Agent
- v0.19 Quicksilver (20 Jul 2026): TTFT turn pertama ~80% lebih rendah
  (4.3s→0.9s cold start); reasoning stream live default; desktop 14x lebih
  cepat streaming markdown, diff virtualized.
- Smart approvals: reviewer LLM menilai command yang di-flag default
  (verdict per-command persis); deny rules definisi user (blokir bahkan di
  yolo); `/deny <reason>` memberi tahu agen kenapa ditolak agar koreksi arah.
- SecretSource interface: Bitwarden + 1Password (ref op://) saat load,
  multi-vault, precedence deterministik, provenance per-variabel.
- Live subagent transcripts + delegasi background durable; ledger delivery
  durable (respons selesai selamat dari crash gateway).
- Rilis Reach: iMessage/WeChat, 16+ platform messaging, Automation Blueprints,
  browser profile builder, atomic memory batch ops.
- v0.9: Termux/Android mobile; dashboard web lokal; API server
  OpenAI-compatible + REST cron mgmt; proxy langganan lokal untuk provider
  OAuth; export trajectory training untuk SFT/RL.
- Kanban sebagai platform multi-agen; promptware defense.

### Pola lintas-tool (tema berulang)
1. **Edit-format tuning per-model terukur** (omp: -61% token, 2.1x pass rate;
   Aider auto-pick per family).
2. **Permission engine konvergen**: satu engine, modes, deny-rules bahkan di
   yolo, approval di-review LLM, denial dengan alasan (/deny <reason>).
3. **Session sharing/collab**: relay E2E + QR (omp /collab), transcript live
   (Hermes), Tailscale instances (T3Code).
4. **Config inheritance** dari tool lain (.claude/.cursor/.codex…) = migrasi
   nol (omp; Codex import Cursor skills).
5. **GitHub sebagai path** (pr://1428), bukan tool khusus (omp).
6. **Model kedua sebagai advisor/reviewer** mengawasi tiap turn (omp advisor;
   Codex auto reviewer; CC PermissionDenied hooks).
7. **Scheduled/cron agents di mana-mana** (CommandCode, ZCode /schedule,
   Antigravity /schedule, Hermes cron, OpenClaw Task Brain).
8. **Schema-driven tool-call repair** — call malformed diperbaiki, bukan gagal
   (CommandCode).
9. **Model kecil on-device** untuk housekeeping (omp: titling, ekstraksi memori).
10. **Free tier/router sebagai distribusi** (Freebuff, NaraRouter, ZCode free);
    per-task model routing sebagai arsitektur inti.
11. **Backlash trust/observability**: perubahan UI paksa (Antigravity,
    Cursor 3), perubahan effort diam-diam (CC post-mortem), kejutan kuota →
    user menghukum opasitas. Visibilitas effort/compaction = fitur.
12. **Keamanan memori**: trust tagging by source, masked secrets, memory
    firewall, promptware defense (OpenClaw issues, Hermes).
13. **Runtime everything-is-a-plugin** — loop agen sendiri swappable
    (DeepSeek Harness/Cordis).
14. **Mobile/remote**: remote control HP (ZCode), Termux (Hermes), iOS overlay
    (T3Code), QR pairing.
15. **Plan review sebagai artefak eksplisit** (CommandCode plan_review);
    /interview flush requirements (Freebuff); /grill-me (Antigravity).

## Langkah 4 — Sesi Kreatif (14 ide mentah, tanpa sensor)

1. Pasang validator engine ke agent loop — post-edit gate otomatis.
2. Edit-format learning per-model: persist metrics ke disk, auto-pilih format
   terbaik per model family, laporkan penghematan token.
3. Permission engine v2: deny/ask/allow rules persist, berlaku di yolo,
   reviewer LLM opsional, `/deny <reason>` → alasan masuk konteks agen.
4. Advisor model kedua: baca tiap turn, suntik catatan/koncern/blocker dengan
   konteksnya sendiri (sub_agent_tool sudah ada — tinggal wiring).
5. Schema-driven tool-call repair: args JSON malformed diperbaiki sebelum
   gagal (schema tool sudah ada di `schema.rs`).
6. Session sharing: link relay E2E + QR untuk live transcript; `kerux share`.
7. Scheduler upgrade: cron expression 5-field + one-shot timestamp + job
   menjalankan run agen (bukan cuma prompt chat).
8. Memory firewall: trust tagging per sumber (user/agent/web/file), masked
   secrets di memory blocks, redaction diperluas ke store.
9. Lazy MCP: spawn on-demand saat tool pertama dipanggil, idle-timeout unload.
10. Stale-write guard: cek mtime antara read dan write; atomic rename sudah ada.
11. `/undo` user-facing dari GitHarness checkpoint — satu command, rollback
    file + session state.
12. Taste RL loop: sinyal accept/reject/revert eksplisit (tombol TUI +
    command) masuk ekstraksi taste — data training dari penggunaan harian.
13. Config inheritance penuh: baca `.codex/`, `.gemini/`, `.windsurf/`,
    `.cline/`, `.github/copilot-instructions.md` (fondasi sudah ada di
    `context_files.rs`).
14. `/interview` mode: agen menggali requirements dengan pertanyaan terstruktur
    dulu, menghasilkan spec markdown, baru eksekusi (gabungan /grill-me +
    /interview + plan-first yang sudah ada).

Terlempar juga (tidak masuk shortlist): GitHub-as-paths (butuh auth surface
baru, ROI rendah vs gh CLI), on-device tiny model (infra besar), runtime
plugin penuh ala Cordis (rewrite arsitektur — terlalu dini), vertical role
agents ala HelloMinds (di luar positioning coding agent).

## Langkah 5 — Ide Terpilih (siap jadi kartu Kanban, urut dampak)

### 1. Wire Validator Engine ke Agent Loop (post-edit verification gate)
- **Kategori**: arsitektur / kualitas — menyelesaikan utang round-1
- **Deskripsi**: `run_validation_pass` (`validators.rs:271`) lengkap tapi hanya
  dipanggil test. Pasang ke agent loop: setelah edit tool sukses, jalankan
  pass validasi (fmt/check/test sesuai config project), hasil masuk kembali ke
  konteks agen untuk self-repair — loop Aider-style. Tambah `kerux validate`
  CLI untuk manual run.
- **Kenapa penting**: Engine sudah ada dan teruji — ini wiring, bukan riset.
  Langsung menaikkan kualitas output otonom; Aider membuktikan loop
  test-and-lint adalah pembeda kualitas utama.
- **Inspirasi**: Aider auto-test-and-lint; DeepSeek Harness preset modes;
  n8n self-fixing MCP build.
- **Estimasi effort**: S

### 2. Edit-Format Learning per-Model (persist + auto-pick)
- **Kategori**: efisiensi token / kualitas edit
- **Deskripsi**: `EditMetricsTracker` sudah menghitung success/repair per
  format tapi in-memory only. Persist ke `~/.kerux/edit-metrics.json` per
  model; saat session start, pilih format dengan pass-rate terbaik untuk model
  aktif (fallback ke ladder yang ada). Tampilkan statistik di telemetry HUD.
- **Kenapa penting**: omp mengukur hasil nyata: -61% token (Grok 4 Fast) dan
  2.1x pass rate (MiniMax) hanya dari berhenti melawan model. Aider melakukan
  auto-pick per family. Kerux sudah punya ladder + override + metrics —
  kurang persistensi dan auto-pick.
- **Inspirasi**: omp per-model tuning; Aider auto-picked edit format.
- **Estimasi effort**: S

### 3. Permission Engine v2 (deny-rules + LLM reviewer + /deny reason)
- **Kategori**: keamanan / kontrol
- **Deskripsi**: Upgrade `approval.rs` dari biner ke rules engine: deny/ask/allow
  per pola command (glob), persist per-project + global, deny berlaku bahkan
  di yolo. Opsional: reviewer LLM menilai command yang di-flag sebelum prompt
  user (Codex auto-reviewer style). `/deny <reason>` — alasan user disuntikkan
  ke konteks agen agar koreksi arah, bukan mengulang.
- **Kenapa penting**: Pola konvergen 2026 (CommandCode single engine 5 mode;
  Hermes smart approvals + deny rules; OpenCode glob permissions). Yolo tanpa
  deny-rules adalah liability; denial tanpa alasan bikin agen mengulang kesalahan.
- **Inspirasi**: CommandCode v1; Hermes v0.19 smart approvals; OpenCode
  glob-pattern bash permissions; Codex --approve-for-me.
- **Estimasi effort**: M

### 4. Advisor Model Kedua (reviewer di atas setiap turn)
- **Kategori**: kualitas / orkestrasi
- **Deskripsi**: Model kedua (murah/kecil) membaca tiap turn agen utama dan
  menyuntikkan catatan inline — koncern, koreksi, hard blocker — dengan
  konteksnya sendiri yang terpisah. Pakai sub_agent infrastructure yang sudah
  ada; toggle per-session, tampil di Reasoning panel.
- **Kenapa penting**: omp advisor + Codex auto reviewer agent = arah industri.
  Menangkap drift sebelum jadi rework; biaya kecil jika pakai model flash.
  Kerux sudah punya fan-out + cost guardrails — kombinasi natural.
- **Inspirasi**: omp advisor role; Codex automatic reviewer agent; Claude Code
  PermissionDenied hooks.
- **Estimasi effort**: M

### 5. Schema-Driven Tool-Call Repair
- **Kategori**: robustness / efisiensi token
- **Deskripsi**: Saat args JSON dari model gagal parse terhadap `ToolSchema`
  (`schema.rs`), coba repair deterministik (field hilang diisi default, tipe
  salah dicoerce, JSON rusak dibersihkan) sebelum menyatakan gagal. Catat
  repair di telemetry.
- **Kenapa penting**: CommandCode membuktikan ini worth it untuk 40+ tool.
  Setiap tool-call gagal = 1 turn ekstra = token + waktu. Kerux sudah punya
  schema lengkap — repair layer adalah tambahan kecil dengan payoff di semua
  model, terutama model kecil/lokal.
- **Inspirasi**: CommandCode schema-driven input repair.
- **Estimasi effort**: S

### 6. Scheduler Upgrade: cron expression + one-shot + agent runs
- **Kategori**: fitur user-facing / otomasi
- **Deskripsi**: `scheduler.rs` sekarang hanya interval. Tambah: cron
  expression 5-field (parser stdlib-only kecil), one-shot timestamp, dan job
  yang memicu run agen penuh (bukan cuma prompt ke chat channel). `/cron`
  command yang sudah ada jadi surface-nya.
- **Kenapa penting**: Scheduled agents ada di mana-mana per Agustus 2026
  (ZCode /schedule, Antigravity /schedule, CommandCode cron_, Hermes cron,
  OpenClaw Task Brain). Kerux sudah punya scheduler + webhook + mission
  control — upgrade kecil ini melengkapi story otomasi 24/7.
- **Inspirasi**: ZCode; Antigravity; CommandCode; Hermes cron.
- **Estimasi effort**: S-M

### 7. Memory Firewall (trust tagging + masked secrets)
- **Kategori**: keamanan
- **Deskripsi**: Setiap `MemoryBlock` bawa `source` (user/agent/web/file/tool)
  dan trust score; konten dari sumber rendah tidak pernah masuk prompt tanpa
  penanda. Masking secrets di store (pakai `redaction.rs` yang sudah ada).
  Tolak tulis memori yang mengandung pola kredensial.
- **Kenapa penting**: OpenClaw community menyebut ini eksplisit (#7707 memory
  trust tagging, #10659 masked secrets); Hermes bangun promptware defense.
  Memory poisoning adalah vektor serangan nyata untuk agen persisten; Kerux
  sudah punya memory store + redaction — tinggal policy layer.
- **Inspirasi**: OpenClaw issues; Hermes promptware defense; omp memory.
- **Estimasi effort**: M

### 8. Lazy MCP Spawn + Idle Unload
- **Kategori**: efisiensi / startup time
- **Deskripsi**: `mcp.rs` spawn semua server saat connect. Ubah: daftar tool
  diambil dari cache/manifest; proses di-spawn saat tool pertama dipanggil;
  unload setelah idle timeout. Claude Code pattern: deferred list sampai
  @mention pertama.
- **Kenapa penting**: Startup time dan RAM adalah keluhan universal harness
  dengan banyak MCP. Claude Code sudah deferred resources; Codex mempercepat
  MCP/plugin startup. Kerux TUI harus terasa instan.
- **Inspirasi**: Claude Code deferred MCP resources; Codex faster MCP startup.
- **Estimasi effort**: S

### 9. Stale-Write Guard untuk Edit Tools
- **Kategori**: robustness / keamanan data
- **Deskripsi**: Rekam mtime+hash saat file dibaca; sebelum write-back atomik,
  verifikasi tidak berubah di luar (user edit manual, tool lain). Jika stale →
  tolak dengan pesan yang menyuruh model re-read. `edit_block_tool` sudah
  atomic — tinggal guard.
- **Kenapa penting**: CommandCode menyebut stale-write protection + atomic
  writes sebagai fitur inti v1. Dengan session branching + autonomous mode,
  risiko write collision nyata. Murah dibangun, mencegah korupsi senyap.
- **Inspirasi**: CommandCode stale-write protection; omp atomic publication.
- **Estimasi effort**: S

### 10. `/undo` User-Facing + Taste Accept/Reject Signals
- **Kategori**: DX / data flywheel
- **Deskripsi**: Dua paruh murah: (a) expose `GitHarness::checkpoint` sebagai
  `/undo` di TUI + CLI — rollback file ke checkpoint terakhir, Aider-style;
  (b) tambah sinyal accept/reject/revert eksplisit (keybind TUI, `/reject`)
  yang mengalir ke taste extraction — penggunaan harian jadi data training
  taste tanpa feedback eksplisit tambahan.
- **Kenapa penting**: (a) Aider `/undo` dan OpenCode snapshots membuktikan
  rollback adalah trust feature utama; infra sudah ada internal. (b)
  CommandCode taste-1 dibangun persis di atas sinyal accept/reject/edit —
  Kerux sudah punya taste pipeline, kurang sinyal eksplisit.
- **Inspirasi**: Aider /undo; OpenCode session snapshots & revert; CommandCode
  taste-1 continuous RL.
- **Estimasi effort**: S-M

---

### Catatan dedupe vs Round 1
Kesepuluh ide di atas TIDAK tumpang tindih dengan 10 ide round-1 (semua sudah
diimplementasikan). Ide #1 menyelesaikan sisa roadmap lama (validator wiring)
yang tercatat di TODO.md Pending — sisanya baru. Urutan mencerminkan rasio
dampak/effort: empat effort-S dengan engine sudah ada (#1, #2, #5, #8, #9)
adalah kemenangan tercepat.
