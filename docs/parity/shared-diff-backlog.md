# Shared-Different Backlog

Generated: `2026-11-20T22:14:55.793018+00:00`

## Summary

- Total shared-different paths: `1508`
- Pending classification: `0`
- Pending functional review: `0`
- Deferred review (counted, not cleared): `22`
- Cleared non-runtime: `320`
- Cleared intentional divergence: `1166`

## Status Counts

| Status | Count |
| --- | ---: |
| `cleared_intentional_divergence` | 1166 |
| `cleared_non_runtime` | 320 |
| `deferred_review` | 22 |

## Workstream Counts

| Workstream | Count |
| --- | ---: |
| `WS3` | 134 |
| `WS4` | 229 |
| `WS5` | 435 |
| `WS6` | 697 |
| `WS8` | 13 |

## Pending Classification

| Path | Workstream | Action |
| --- | --- | --- |

## Pending Functional Review By Prefix

| Classification Path | Count |
| --- | ---: |

## Deferred

Not yet shown to match in Rust; each row is counted and has a review date.

| Path | Review date | Ledger | Reason |
| --- | --- | --- | --- |
| `plugins/dashboard_auth/basic/__init__.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream reworked the dashboard basic-auth provider (session revocation list, token kind and expiry checks, non-interactive mixin). Rust does not run this Python plugin, and no Rust code implements a basic-auth dashboard provider. Deferred until the dashboard auth providers are ported or dropped. |
| `plugins/google_meet/README.md` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream's README now says realtime mode is speak-only and installs with `hermes meet install`. Rust does not run this Python plugin, and no Rust code implements Google Meet. Deferred with the Google Meet plugin. |
| `plugins/google_meet/SKILL.md` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream's SKILL.md replaces the pip install steps with `hermes meet install`. Rust does not run this Python plugin, and no Rust code implements Google Meet. Deferred with the Google Meet plugin. |
| `plugins/google_meet/node/client.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: the node client now leaves byte decoding to protocol.decode. Rust does not run this Python plugin, and no Rust code implements Google Meet. Deferred with the Google Meet plugin. |
| `plugins/google_meet/node/protocol.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream's node protocol now imports hmac for the token check and decodes bytes itself. Rust does not run this Python plugin, and no Rust code implements Google Meet. Deferred with the Google Meet plugin. |
| `plugins/hermes-achievements/dashboard/plugin_api.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream rewrote the achievements dashboard API (460 lines added, 627 removed). Rust does not run this Python plugin, and no Rust code implements the achievements dashboard. Deferred until that plugin is ported or dropped. |
| `plugins/model-providers/alibaba/__init__.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream added the alibaba-cn, alibaba-token-plan and alibaba-token-plan-cn profiles, the aliyun alias and DashScope's preserve-thinking extras. The Rust CLI already maps aliyun to alibaba (crates/hermes-cli/src/main/auth_providers.rs); the Rust provider tables (crates/hermes-agent/src/provider_profiles.rs, crates/hermes-core/src/providers.rs) have none of the 3 profiles or the extras. Deferred until ported. |
| `plugins/platforms/discord/voice_mixer.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream refactored its Discord voice mixer (child names and the speech flag removed, the mixer now a discord.AudioSource, ffmpeg resolution added). This tree's Rust mixer (crates/hermes-gateway/src/voice_mixer.rs) ports the b51fbc73 design; whether the mixing behaviour changed was not established. Deferred until compared. |
| `plugins/platforms/teams/plugin.yaml` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream declares 4 new Teams settings (TEAMS_HOST, TEAMS_REQUIRE_MENTION, TEAMS_INCOMING_WEBHOOK_URL and TEAMS_GRAPH_ACCESS_TOKEN). Rust reads TEAMS_INCOMING_WEBHOOK_URL and TEAMS_GRAPH_ACCESS_TOKEN for the meeting pipeline only (crates/hermes-cli/src/teams_pipeline_cli.rs, crates/hermes-tools/src/teams_pipeline/graph_backend.rs), and neither TEAMS_HOST nor TEAMS_REQUIRE_MENTION. Deferred until ported. |
| `tests/gateway/test_complete_path_at_filter.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream added 7 path-completion cases (remote-backend completion, leading slash, fuzzy matching outside a git repository) and removed 10. No Rust test for these completion cases was found. Deferred until compared. |
| `tests/gateway/test_goal_max_turns_config.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream added test_goal_command_slow_db_init_still_persists. Whether the Rust /goal command persists when the session database is slow to open was not established. Deferred until compared. |
| `tests/gateway/test_notice_delivery.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream removed 2 test cases in its suite-wide pruning (39975613) and added none, and the kept case now expects a private notice's thread metadata to carry the author's user_id (upstream's per-turn identity stamp). No Rust code matches notice_delivery or private_notice under crates/, so the private notice path was not compared. Deferred until compared with the Rust gateway's notice delivery. |
| `tests/gateway/test_pre_gateway_dispatch.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream added 13 cases on webhook and relay admission (the pre-dispatch hook fires once at ingress, resends and rewritten deliveries keep or refuse their admission) and removed 4. Rust has no equivalent admission records. Deferred until ported. |
| `tests/gateway/test_running_agent_session_toggles.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream added test_fresh_ancient_turn_remains_controllable and removed 3 cases. Not compared with the Rust session toggles. Deferred until compared. |
| `tests/gateway/test_runtime_footer.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream added 8 runtime-footer cases (turn latency and an opt-in served-model field) and removed 14. No Rust footer renders latency. Deferred until ported. |
| `tests/gateway/test_safe_adapter_disconnect.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream added test_safe_disconnect_detaches_cancellation_swallowing_disconnect. Not compared with Rust adapter shutdown. Deferred until compared. |
| `tests/gateway/test_session_model_reset.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream removed 2 cases and now checks that /new clears the session's reasoning override while other sessions keep theirs. No Rust code names a per-session reasoning override. Deferred until compared. |
| `tests/gateway/test_shared_group_sender_prefix.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream now includes the Slack author's mention in a shared thread's sender prefix. No Rust code adds it. Deferred until ported. |
| `tests/gateway/test_steer_command.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream added cases where /steer reaches a long-running turn and where a steer with media queues the whole event. No Rust code queues a steer's media. Deferred until ported. |
| `tests/gateway/test_unknown_command.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream added cases for /egress status and for running a synchronous plugin command off the event loop, and removed 7 command-hook cases. /egress is not ported (slash allowlist). Deferred until ported. |
| `tests/gateway/test_webhook_signature_rate_limit.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream now answers a bad webhook signature with the 404 an unknown route gets, so route names cannot be probed, and removed 2 rate-limit cases. Since PR #164 Rust gives a bad signature the unknown-route reply (crates/hermes-gateway/src/platforms/webhook.rs, test a_bad_signature_reads_as_an_unknown_route), with an empty body where upstream's says 'Unknown route'. Rust checks the signature before the rate limit, but no Rust test sends more bad requests than the limit as upstream's remaining case does. Deferred until that case has a Rust test. |
| `tests/gateway/test_yolo_command.py` | 2027-02-18 | R2338 | Compared b51fbc73 with v0.23.2: upstream added 3 cases on /yolo persistence (it survives a gateway restart, dies at the session boundary, and a revocation outlives the next turn). Not compared with the Rust /yolo handling. Deferred until compared. |
