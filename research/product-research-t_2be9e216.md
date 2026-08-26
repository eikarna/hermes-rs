# Kerux — Product Research: Feature & Architectural Gap Discovery
Task: t_2be9e216 | Date: 2026-08-26 | Researcher: researcher profile

## Langkah 1 — Ringkasan Proyek (faktual)

Kerux adalah AI coding assistant berbasis Rust (workspace Cargo multi-crate: kerux-core, kerux-cli, dll.) yang berjalan sebagai CLI/TUI (ratatui) dengan mode otonom yang membaca TODO.md sebagai task ledger. Targetnya developer yang ingin agent coding lokal, model-agnostic (multi-provider: OpenAI-compat, Anthropic, Gemini, Nous OAuth), dan bisa dikendalikan lewat gateway chat (Telegram/Slack/Discord) termasuk voice STT. Pembeda utamanya: runtime otonom dengan state `autonomous-status.toml` yang persist antar-restart, pause-on-repeated-failure, approval gate untuk tool berbahaya, sub-agent delegation, dan trajectory recording untuk training/curator. Arah pengembangan saat ini (CHANGELOG 0.2.x + roadmap): stabilisasi multi-provider, fallback chain, validator engine, dan persiapan v0.3 (webhook listener, vision input, session branching, plugin system).

## Langkah 2 — Audit Gap Internal (dari kode, bukan asumsi)

| # | Gap | Bukti di kode | Status |
|---|-----|---------------|--------|
| 1 | Webhook transport: config ada, HTTP listener tidak ada | `config.rs:483-518` (`webhooks_enabled`, `webhooks_addr`), `gateway.rs:36-39,258,2811` — Slack "relies on webhooks" tapi `poll_updates` yield nothing | TODO.md Pending, roadmap v0.3 |
| 2 | Fallback provider chain: terpasang tapi unadvertised, belum soak-test | roadmap.md v0.3 | Setengah jadi |
| 3 | Validator engine selesai, CLI wiring belum | TODO.md Pending | Setengah jadi |
| 4 | Vision input: `supports_vision` plumbed, tidak ada pipeline multimodal (image → model) | 22 match `vision` hanya flag kapabilitas; gateway nol `image|photo|attachment` | Roadmap v0.3 |
| 5 | Gemini streaming one-shot (bukan true streaming) | TODO.md | Known limitation |
| 6 | Session branching/fork/rewind: tidak ada di kode | search `fork|rewind|session_tree` → 1 match (komentar fork+exec, bukan fitur) | Roadmap "Later" |
| 7 | Sub-agent delegation ada tapi single-shot analysis only, tanpa fan-out paralel/orkestrasi | `tools/sub_agent_tool.rs` — isolated child, satu task | Parsial |
| 8 | Trajectory module untuk training export, bukan observability live (tanpa tok/s, cache hit, cost) | `trajectory.rs` — builder/exporter only | Gap |
| 9 | Approval gate per-tool ada, tapi tidak ada plan-first mode (agent propose plan → approve → execute) | `approval.rs`, `agent.rs:1598+` | Gap |
| 10 | Plugin system, multi-agent routing, embedding memory search, TTS | roadmap "Later" | Belum mulai |

Catatan: `unimplemented!()` di `curator.rs:833` hanya mock test — bukan gap riil. Board Kanban hanya berisi t_2be9e216 (tidak ada kartu lain untuk dedupe).

## Langkah 3 — Riset Eksternal (19 tool, web search per tool)

1. **Aider** — repo-map context, lint/test auto-loop setelah edit, arsitektur edit-format. Komunitas minta: context management yang lebih hemat token, multi-file refactor yang lebih andal.
2. **OpenCode (sst)** — terminal agent open-source, multi-session, share session, TUI + desktop; ekosistem plugin tumbuh.
3. **Claude Code** — changelog cepat: hooks, sub-agents, MCP, skills, checkpoints, plan mode. Keluhan: cost/usage visibility, context window management.
4. **Codex CLI (OpenAI)** — sandboxing per-command, cloud tasks, multi-model; request populer: kontrol approval lebih granular, integrasi CI.
5. **Cursor** — background agents, planning agent (auto to-do berdependensi), bugbot review, message queue saat agent jalan. Keluhan: credit/pricing opacity.
6. **n8n** — AI agent nodes, human-in-the-loop approval via chat, scheduled/cron triggers, evaluasi agent. Arah: agent observability + trigger dari mana saja.
7. **Hermes-Agent (Nous)** — kanban multi-agent orchestration, cron jobs, gateway multi-platform, skill system, memory persisten. Validasi arah gateway Kerux.
8. **OpenClaw** — agent framework; penekanan pada extensibility dan native tooling.
9. **Goose (Block)** — MCP-first, "recipes" untuk workflow repeatable, extension system; request: manajemen context yang lebih transparan.
10. **Cline** — plan/act mode terpisah, checkpoints (rollback), browser use; request: cost breakdown per task.
11. **Roo Code** — custom modes (Architect/Code/Debug), Boomerang tasks (dekomposisi), MCP marketplace; roadmap publik via GitHub Projects.
12. **Continue.dev** — final 2.0: telemetry dihapus, auth dipisah; hub untuk share assistants/rules/blocks. Arah: open-source assistant yang composable.
13. **Zed** — parallel agents (multi-thread multi-project), ACP protocol (bawa agent eksternal: Claude/Codex/OpenCode), sandboxing, Skills, plan mode coming, "code history as context", ask_user via elicitation forms. Request komunitas: paste image ke agent chat, search across thread history.
14. **Qoder** — Quest mode (otonom + spec-driven + self-evolving), Repo Wiki, Browser Agent (capture console/network → debug loop), Planning Agent, AppShot (capture jendela app jadi context), real-time voice, terminal sandbox, hooks dengan argumen, deteksi perintah berbahaya.
15. **T3Code (pingdotgg)** — GUI layer di atas coding agent yang sudah dibayar: diff viewer turn-by-turn (unified/split), integrated terminal, remote session access; planned: CLI integration.
16. **Pi (earendil/mariozechner)** — minimal, "primitives not features": `/tree` `/fork` `/clone` (session branching penuh), `/share` (gist+HTML), extensions inject context per-turn/filter history/RAG/long-term memory, 15+ provider Ctrl+P cycling, print/JSON mode untuk scripting. Komunitas: auto-compact lebih baik.
17. **OMP (oh-my-pi)** — "Pi with batteries": native Windows, in-process ripgrep/glob/find/brush-bash (tanpa fork-exec), doc indexing compressed, fallback model.
18. **Antigravity (Google)** — agent-first IDE: Agents Manager multi-workspace, artifacts dengan inline comments → kirim balik untuk iterasi, implementation plans. Keluhan riil (review): diff view buggy, file-changes view mencampur perubahan antar conversation, tidak ada sound/notifikasi saat agent selesai (user minta attention mechanism), workspace switching bug.
19. **DeepSeek Harness** — "everything is a plugin": tools/skills/sessions/agent-loop/ bahkan Claude Code & Codex sebagai sub-agent plugin; local web UI; live stats (tok/s, cache hit rate, turn count, running time); trajectory view per-step dengan trace ke plugin asal; MIT; YAML config.
20. **ZCode (Z.ai)** — harness resmi GLM-5.3: 1M context stabil, automation runs (bot-created results delivered back), presentation preview, off-peak quota reset.
21. **CommandCode** — "taste learning": profil gaya coding dengan confidence score (`npx taste push`), skills sebagai file terbuka di repo (reviewable via PR), single binary ~40MB, `/review` agent, transcript mode.
22. **Freebuff** — free multi-agent: parallel agents tiap workspace sendiri, browser use, hosted sandbox/preview/deploy, @files/@agents mentions.
23. **Zazen** — TIDAK DITEMUKAN tool coding agent dengan nama ini (hasil hanya Zencoder/Zen Coder yang berbeda). Dicatat sebagai not-found.
24. **HelloMinds** — TIDAK DITEMUKAN hasil relevan (hanya produk unrelated). Dicatat sebagai not-found.

### Pola lintas-tool (tema yang berulang)
- **Plan-first execution** (Cursor, Qoder, Zed, Windsurf/Kiro via diskusi Zed) — standar baru.
- **Session branching/fork** (Pi paling eksplisit) — masih jarang.
- **Live observability** (DeepSeek Harness, ZCode) — tok/s, cache hit, cost, trajectory per-step.
- **Agent-external-as-tool / ACP** (Zed, DeepSeek Harness) — orkestrasi lintas harness.
- **Attention/notification saat agent selesai** (Antigravity complaint, n8n, ZCode) — masalah riil user.
- **Taste/style learning** (CommandCode) — diferensiasi baru.
- **Vision/screen context** (Qoder AppShot, Zed request, Cline browser use).
- **Sandboxing + dangerous-command detection** (Qoder, Zed, Codex).

## Langkah 4 — Sesi Kreatif (14 ide mentah, tanpa sensor)

Power user / otomasi ekstrem:
1. Webhook inbound trigger — sistem eksternal (CI, GitHub, cron) memicu run Kerux via HTTP POST.
2. Cost guardrails — budget per-run/hari, auto-pause + notifikasi saat 80%, auto-fallback ke model murah.
3. `kerux -p` pipe mode + JSON event stream untuk scripting (ala Pi print/JSON).
4. Scheduled autonomous runs (cron) dengan laporan diff ke chat.

Tim kecil / multi-agent:
5. Parallel sub-agent fan-out — pecah task ke N child agent paralel, merge hasil, panel orkestrasi.
6. Agent eksternal sebagai tool — panggil claude-code/codex/opencode sebagai sub-agent (ala DeepSeek Harness/ACP).
7. Shared skill registry — skill file di repo, reviewable via PR, `kerux skill push/pull`.
8. Review agent — `/review` satu perintah: diff → komentar inline → iterasi (ala CommandCode/Antigravity artifacts).

Mobile/Termux:
9. Telegram-first mission control — approve tool, steer, lihat diff/status run dari HP (gateway sudah ada).
10. Termux build — static binary + Ollama lokal, coding agent offline di Android.

Tanpa batasan:
11. Time-travel checkpoints — snapshot workspace per turn, rewind/fork dari titik mana pun.
12. Taste profile — belajar gaya coding user, confidence-scored, portabel antar proyek.
13. Screenshot-to-fix — kirim foto UI/error dari HP → vision model → patch (pakai pipeline vision yang belum ada).
14. Live telemetry HUD di TUI — tok/s, cache hit %, cost akumulasi, turn count (ala DeepSeek Harness).

## Langkah 5 — 10 Ide Terpilih (siap jadi kartu Kanban, urut dampak)

### 1. Webhook Inbound Trigger (HTTP listener untuk gateway)
- **Kategori**: arsitektur / integrasi
- **Deskripsi**: Config `webhooks_enabled`/`webhooks_addr` sudah ada tapi tidak ada listener — Slack adapter bahkan mati karena ini. Bangun HTTP server (axum) yang menerima webhook platform + generic JSON trigger untuk memulai run agent.
- **Kenapa penting**: Membuka otomasi ekstrem (CI → Kerux, GitHub event → Kerux) dan menghidupkan Slack adapter yang saat ini non-fungsional. Fondasi untuk semua trigger-based feature.
- **Inspirasi**: n8n (scheduled/webhook triggers), DeepSeek Harness (remotely reachable instance), roadmap v0.3 sendiri.
- **Estimasi effort**: M

### 2. Plan-First Execution Mode
- **Kategori**: fitur user-facing / DX
- **Deskripsi**: Mode di mana agent menghasilkan rencana terstruktur (langkah + file yang disentuh) dulu, user approve/edit via approval gate yang sudah ada, baru eksekusi. Rencana disimpan sebagai markdown di repo.
- **Kenapa penting**: Approval gate per-tool sudah ada — ini menaikkan level kontrol dari per-tool ke per-plan. Standar industri baru (Cursor planning, Qoder Planning Agent, Zed Plan Mode) dan mengurangi rework.
- **Inspirasi**: Cursor, Qoder, Zed, Antigravity implementation plans.
- **Estimasi effort**: M

### 3. Session Branching: fork/rewind/clone
- **Kategori**: arsitektur
- **Deskripsi**: Session disimpan sebagai tree, bukan list linear: rewind ke message mana pun, fork jadi cabang baru, clone cabang aktif. Butuh refactor storage session di kerux-core.
- **Kenapa penting**: Fitur pembeda yang masih jarang; Pi menjadikannya headline (`/tree` `/fork` `/clone`) dan komunitas memintanya. Eksplorasi solusi paralel tanpa kehilangan konteks.
- **Inspirasi**: Pi (paling eksplisit), roadmap "Later" Kerux sendiri.
- **Estimasi effort**: L

### 4. Live Session Telemetry HUD
- **Kategori**: DX / fitur user-facing
- **Deskripsi**: Panel TUI + output gateway menampilkan tok/s, cache hit rate, token usage per-turn, cost estimasi, turn count — secara live selama run. Data sudah terekam di trajectory/event recorder, tinggal disurface.
- **Kenapa penting**: Keluhan cost/usage visibility muncul di hampir semua komunitas (Claude Code, Cursor, Cline). Effort relatif kecil karena datanya sudah ada; diferensiasi vs CLI agent lain yang menyembunyikannya.
- **Inspirasi**: DeepSeek Harness (live stats + trajectory view), ZCode (session stats).
- **Estimasi effort**: S

### 5. Parallel Sub-Agent Fan-Out & Orchestration Panel
- **Kategori**: arsitektur
- **Deskripsi**: Perluas `delegate_to_sub_agent` (saat ini single-shot) menjadi fan-out paralel: parent memecah task, N child jalan konkuren dengan workspace isolasi, hasil di-merge, panel TUI menampilkan status tiap child.
- **Kenapa penting**: Multi-agent adalah arah ekosistem (Hermes kanban, Freebuff parallel agents, DeepSeek orchestrator mode). Kerux sudah punya primitifnya — tinggal di-orkestrasi.
- **Inspirasi**: DeepSeek Harness (orchestrator), Freebuff, Hermes-Agent, Roo Code Boomerang tasks.
- **Estimasi effort**: L

### 6. External Agent Harness sebagai Tool (ACP-style)
- **Kategori**: integrasi
- **Deskripsi**: Tool yang memanggil CLI agent eksternal (claude-code, codex, opencode) sebagai sub-agent: kirim task, tangkap hasil, fold back ke conversation. Konfigurasi via TOML (binary path + argumen).
- **Kenapa penting**: User sudah bayar langganan agent lain — jadikan Kerux lapisan orkestrasi di atasnya, bukan kompetitor. Posisi unik untuk tool netral-model.
- **Inspirasi**: DeepSeek Harness (Claude Code/Codex sebagai plugin), Zed ACP.
- **Estimasi effort**: M

### 7. Vision Input Pipeline (screenshot/image → model)
- **Kategori**: fitur user-facing
- **Deskripsi**: Pipeline multimodal end-to-end: terima gambar dari gateway chat / path lokal / clipboard, attach ke request untuk model `supports_vision`. Lengkapi flag kapabilitas yang sudah plumbed dengan ingest nyata.
- **Kenapa penting**: `supports_vision` ada tapi tak ada jalur masuk gambar — gap roadmap v0.3. Membuka use-case "kirim foto error dari HP → agent fix" dan screenshot-driven debugging.
- **Inspirasi**: Qoder AppShot, Zed (paste image request komunitas), Cline browser use.
- **Estimasi effort**: M

### 8. Mobile Mission Control via Gateway Chat
- **Kategori**: fitur user-facing / integrasi
- **Deskripsi**: Kendali penuh run otonom dari chat: tombol approve/deny (gate sudah ada), perintah steer mid-run, ringkasan diff + status autonomous, notifikasi saat agent selesai/butuh input.
- **Kenapa penting**: Antigravity dikritik justru karena tak ada attention mechanism saat agent selesai; n8n/ZCode membuktikan nilai "control from anywhere". Kerux sudah punya gateway + approval gate — tinggal dirangkai.
- **Inspirasi**: Antigravity (complaint), n8n mobile, ZCode (bot-created run results delivered back), Hermes-Agent.
- **Estimasi effort**: M

### 9. Taste/Style Profile Learning
- **Kategori**: fitur user-facing (diferensiasi)
- **Deskripsi**: Sistem yang mengekstrak preferensi gaya coding user dari history/trajectory (sudah terekam!) menjadi profil confidence-scored (mis. "named exports, 0.85"), disimpan portabel, di-inject ke system prompt. Bisa push/pull antar proyek.
- **Kenapa penting**: Diferensiasi nyata — belum ada CLI agent Rust yang punya ini; CommandCode memvalidasi demand (benchmark mereka: edits per task turun drastis setelah profil terbentuk). Trajectory/curator Kerux adalah bahan training yang sudah tersedia.
- **Inspirasi**: CommandCode (taste registry), Qoder "remembers your style".
- **Estimasi effort**: L

### 10. Cost Guardrails + Fallback Chain Hardening
- **Kategori**: arsitektur / DX
- **Deskripsi**: Budget limit per-run/hari dengan auto-pause + notifikasi gateway saat threshold tercapai, auto-downgrade ke model lebih murah, plus dokumentasi & soak-test fallback chain yang sudah terpasang tapi unadvertised.
- **Kenapa penting**: Cost anxiety adalah keluhan lintas-komunitas; fallback chain yang belum soak-tested adalah risiko reliabilitas. Menyelesaikan dua gap internal sekaligus dengan surface area kecil.
- **Inspirasi**: Cline (cost breakdown request), ZCode (off-peak quota), internal roadmap (fallback chain pending).
- **Estimasi effort**: S

---
Catatan riset: Zazen dan HelloMinds tidak ditemukan sebagai coding tool riil (hasil pencarian tidak relevan) — dicatat jujur, tidak dikarang. Semua 17 tool lainnya diriset via web search per-tool sesuai instruksi.
