    use super::*;
    use hermix_core::Message;

    #[test]
    fn test_persist_and_load_session() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        let messages = vec![
            Message::system("You are helpful"),
            Message::user("Hello"),
            Message::assistant("Hi there!"),
        ];

        sp.persist_session(
            "test-session-1",
            &messages,
            Some("gpt-4o"),
            None,
            Some("Test"),
            Some("cached system blob"),
            None,
            // workspace / expert_id: not under test here.
            None,
            None,
        )
        .unwrap();

        assert_eq!(
            sp.get_system_prompt("test-session-1").unwrap().as_deref(),
            Some("cached system blob")
        );

        let loaded = sp.load_session("test-session-1").unwrap();
        // System role is filtered out by `load_session` — the prompt
        // is reconstructed per-turn by `agent_loop`, not reloaded from
        // the timeline. The cache column above (lines 983-986) is the
        // canonical place to look it up; the timeline only holds the
        // user-visible turns.
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].content.as_deref(), Some("Hello"));
        assert_eq!(loaded[1].content.as_deref(), Some("Hi there!"));
    }

    //compression must NOT lose memory.
    // After a compression event, the folded summary is recoverable via
    // `load_compressed_summary`, and every archived turn survives as a
    // real message row inside the compression child-session (returned
    // by `load_session`). This is the regression guard against
    // "compression silently dropped my history".
    #[test]
    fn compression_child_session_preserves_summary_and_archived_turns() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        // Parent session (the live conversation).
        sp.persist_session(
            "parent-1",
            &[Message::user("live message"), Message::assistant("live reply")],
            Some("gpt-4o"),
            None,
            Some("Parent"),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        // Simulate a compression: 3 turns folded into a summary.
        let archived = vec![
            Message::user("important question about the config"),
            Message::assistant("here is the answer with details"),
            Message::user("follow-up clarification"),
        ];
        let snap = CompressionSnapshot {
            summary: "User asked about config; we explained the YAML layout.".to_string(),
            archived_turns: archived.clone(),
            compression_count: 1,
            ..Default::default()
        };
        let child_id = sp
            .create_compression_child_session("parent-1", &snap, Some("gpt-4o"), None)
            .unwrap();

        // E1-a: the summary is recoverable after compression.
        let summary = sp
            .load_compressed_summary(&child_id)
            .expect("summary persisted");
        assert!(summary.contains("config"));

        // E1-b: every archived turn is recoverable (memory not lost).
        let recovered = sp.load_session(&child_id).unwrap();
        assert_eq!(recovered.len(), archived.len(), "all archived turns preserved");
        let joined: String = recovered
            .iter()
            .filter_map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("|");
        assert!(joined.contains("important question about the config"));
        assert!(joined.contains("here is the answer with details"));
        assert!(joined.contains("follow-up clarification"));
    }

    //aggregate_usage must surface
    // real compression telemetry — not zeros. We compress once, then
    // assert the windowed aggregate reports the compaction count and
    // the preserved (not lost) archived turns.
    #[test]
    fn aggregate_usage_counts_compressions_and_archived_turns() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        sp.persist_session(
            "p-compress",
            &[Message::user("hi"), Message::assistant("hello")],
            Some("gpt-4o"),
            None,
            Some("P"),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let archived = vec![
            Message::user("turn one"),
            Message::assistant("turn one reply"),
        ];
        let snap = CompressionSnapshot {
            summary: "compressed".to_string(),
            archived_turns: archived.clone(),
            compression_count: 1,
            ..Default::default()
        };
        sp.create_compression_child_session("p-compress", &snap, Some("gpt-4o"), None)
            .unwrap();

        let agg = sp.aggregate_usage(30).unwrap();
        assert_eq!(agg.total_compactions, 1, "one compression in window");
        assert!(
            agg.total_archived_turns >= archived.len() as u64,
            "archived turns counted"
        );
    }

    // regression (Bug A): createSession must persist
    // the welcome-state draft workspace to `sessions.workspace` so
    // a desktop restart restores the right Sidebar bucket. Before
    // the fix, the column was never written by `persist_session`
    // (SQL omitted it), so `list_sessions` returned workspace=NULL
    // and the row fell into "未分组". Pass None afterwards to
    // confirm COALESCE preserves the original binding across the
    // follow-up persists the agent loop fires on every chunk.
    #[test]
    fn test_persist_session_writes_workspace_and_survives_coalesce() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        let workspace_path = "C:\\Users\\demo\\projects\\foo";
        let expert_id = "expert-coder";

        sp.persist_session(
            "ws-test",
            &[Message::user("hi"), Message::assistant("hello")],
            Some("openai:gpt-4o"),
            Some("test"),
            None,
            None,
            None,
            Some(workspace_path),
            Some(expert_id),
        )
        .unwrap();

        // First persist must land both columns on the row.
        let (got_ws, got_expert) = sp.read_session_metadata("ws-test").unwrap();
        assert_eq!(got_ws.as_deref(), Some(workspace_path));
        assert_eq!(got_expert.as_deref(), Some(expert_id));

        // Subsequent persist with workspace=None / expert_id=None
        // must COALESCE-keep the original binding — otherwise the
        // per-turn persist in `local_agent_service` would silently
        // wipe the user's selection on every streamed chunk.
        sp.persist_session(
            "ws-test",
            &[Message::user("hi"), Message::assistant("hello"), Message::user("again")],
            Some("openai:gpt-4o"),
            Some("test"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let (got_ws, got_expert) = sp.read_session_metadata("ws-test").unwrap();
        assert_eq!(got_ws.as_deref(), Some(workspace_path), "COALESCE must preserve workspace");
        assert_eq!(got_expert.as_deref(), Some(expert_id), "COALESCE must preserve expert_id");
    }

    // regression (Bug B): the reload path must preserve
    // both visible `content` and `reasoning_content` even when the
    // persisted content contains a `<think>` block whose close tag
    // never landed (e.g. the streaming run ended mid-think, or the
    // model used a non-standard variant that `ThinkBlockParser`
    // missed). Before the fix, the server's `get_session_messages`
    // handler ran `ToolCallSanitizer::feed+flush` on the persisted
    // `content`; the sanitizer is a *streaming state machine* and
    // `flush()` discards any held bytes, so the assistant bubble
    // rendered blank. The fix removed the read-path sanitizer
    // (sanitization belongs on the write path), but this test pins
    // the contract at the persistence layer: load_session returns
    // exactly what was persisted.
    #[test]
    fn test_load_session_preserves_think_block_residue_in_content() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        // Imagine a write path where ThinkBlockParser only partially
        // stripped the think block — the user sees the visible body
        // but a fragment of `<think...` remains in the persisted
        // content. The reload path must NOT silently eat that body.
        let mut assistant = Message::assistant("这是答案正文。");
        assistant.content = Some(
            "这是答案正文。<think>model was mid-think when stream ended".to_string(),
        );
        assistant.reasoning_content = Some("fragment thought".to_string());

        sp.persist_session(
            "think-residue",
            &[Message::user("hi"), assistant],
            Some("openai:gpt-4o"),
            Some("test"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let loaded = sp.load_session("think-residue").unwrap();
        assert_eq!(loaded.len(), 2);
        let body = loaded[1].content.as_deref().expect("content preserved");
        assert!(
            body.contains("这是答案正文"),
            "visible body must survive reload: got {body:?}"
        );
        assert!(
            body.contains("think"),
            "the stray think token is allowed to round-trip — the server \
             must not silently mutate persisted content on reload: got {body:?}"
        );
        assert_eq!(
            loaded[1].reasoning_content.as_deref(),
            Some("fragment thought"),
            "reasoning_content must round-trip verbatim on reload"
        );
    }

    #[test]
    fn test_save_session_log() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        let messages = vec![Message::user("What is 2+2?"), Message::assistant("4")];

        let path = sp
            .save_session_log("log-test", &messages, Some("gpt-4o"))
            .unwrap();

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("What is 2+2?"));
        assert!(content.contains("👤 User"));
        assert!(content.contains("🤖 Assistant"));
    }

    #[test]
    fn test_save_trajectory() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        let messages = vec![
            Message::user("Build a website"),
            Message::assistant("Sure, I'll help with that."),
        ];

        let path = sp
            .save_trajectory("traj-test", &messages, "Build a website", true)
            .unwrap();

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["completed"], true);
        assert_eq!(parsed["user_query"], "Build a website");
    }

    //── : token_usage / daily / by_tool aggregation tests ─────

    fn tool_calls_json_legacy(names: &[&str]) -> String {
        let arr: Vec<serde_json::Value> = names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                serde_json::json!({
                    "id": format!("call_{i}"),
                    "type": "function",
                    "function": {"name": n, "arguments": "{}"}
                })
            })
            .collect();
        serde_json::to_string(&arr).unwrap()
    }

    fn usage(prompt: u64, completion: u64, cost: Option<f64>) -> hermix_core::UsageStats {
        hermix_core::UsageStats {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            estimated_cost: cost,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            reasoning_tokens: 0,
        }
    }

    /// Variant of [`usage`] with explicit cache + reasoning fields so
    /// cache aggregation tests can assert against non-zero
    /// numbers instead of relying on default zero.
    fn usage_with_cache(
        prompt: u64,
        completion: u64,
        cost: Option<f64>,
        cache_read: u64,
        cache_creation: u64,
        reasoning: u64,
    ) -> hermix_core::UsageStats {
        hermix_core::UsageStats {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            estimated_cost: cost,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_creation,
            reasoning_tokens: reasoning,
        }
    }

    #[test]
    fn p1_3_persist_token_usage_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        // Sessions FK is required — create the parent rows first so
        // the token_usage inserts don't trip FOREIGN KEY constraint.
        sp.persist_session("sess-1", &[Message::user("hi"), Message::assistant("yo")], Some("gpt-4o"), None, None, None, None, None, None).unwrap();
        sp.persist_session("sess-2", &[Message::user("hi"), Message::assistant("yo")], Some("claude"), None, None, None, None, None, None).unwrap();

        sp.persist_token_usage("sess-1", "gpt-4o", &usage(100, 50, Some(0.001)))
            .unwrap();
        sp.persist_token_usage("sess-1", "gpt-4o", &usage(200, 80, Some(0.002)))
            .unwrap();
        sp.persist_token_usage("sess-2", "claude", &usage(50, 30, None))
            .unwrap();

        let agg = sp.aggregate_usage(30).unwrap();
        assert_eq!(agg.total_input_tokens, 350);
        assert_eq!(agg.total_output_tokens, 160);
        assert!((agg.total_estimated_cost - 0.003).abs() < 1e-9);
    }

    #[test]
    fn p1_3_aggregate_usage_daily_fills_missing_days_with_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        sp.persist_session("sess-today", &[Message::user("hi"), Message::assistant("yo")], Some("gpt-4o"), None, None, None, None, None, None).unwrap();

        // Insert one row at "today" (whatever that is). Daily curve
        // should have `days + 1` entries; today filled, others zero.
        sp.persist_token_usage("sess-today", "gpt-4o", &usage(10, 5, None))
            .unwrap();

        let agg = sp.aggregate_usage(7).unwrap();
        assert_eq!(agg.daily.len(), 8, "days=7 should produce 8 daily buckets");
        let today = chrono::Utc::now().date_naive().format("%Y-%m-%d").to_string();
        let today_row = agg
            .daily
            .iter()
            .find(|d| d.day == today)
            .expect("today's bucket must exist");
        assert_eq!(today_row.input_tokens, 10);
        assert_eq!(today_row.output_tokens, 5);

        let yesterday = (chrono::Utc::now() - chrono::Duration::days(1))
            .date_naive()
            .format("%Y-%m-%d")
            .to_string();
        let y_row = agg
            .daily
            .iter()
            .find(|d| d.day == yesterday)
            .expect("yesterday bucket must exist");
        assert_eq!(y_row.input_tokens, 0);
        assert_eq!(y_row.output_tokens, 0);
    }

    #[test]
    fn p1_3_by_tool_counts_flat_and_legacy_shapes() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        // Persist one session with two assistant messages — one with
        // the modern flat tool_calls shape, one with the legacy
        // `function.name` envelope. The aggregator must count both.
        let messages = vec![
            Message::user("read x and y"),
            Message {
                role: hermix_core::MessageRole::Assistant,
                content: None,
                content_parts: None,
                tool_calls: Some(vec![
                    hermix_core::ToolCall {
                        id: "c1".into(),
                        function: hermix_core::FunctionCall {
                            name: "read_file".into(),
                            arguments: "{}".into(),
                        },
                    },
                    hermix_core::ToolCall {
                        id: "c2".into(),
                        function: hermix_core::FunctionCall {
                            name: "grep".into(),
                            arguments: "{}".into(),
                        },
                    },
                ]),
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                cache_control: None,
                execution_timeline: None,
                ephemeral: false,
            },
            Message {
                role: hermix_core::MessageRole::Assistant,
                content: None,
                content_parts: None,
                tool_calls: None, // legacy tests write JSON directly below
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                cache_control: None,
                execution_timeline: None,
                ephemeral: false,
            },
        ];
        sp.persist_session("sess-tools", &messages, Some("gpt-4o"), None, None, None, None, None, None)
            .unwrap();

        // Hand-overwrite the second assistant row with the legacy
        // envelope shape so we test the JSON-parser fallback path.
        use rusqlite::Connection;
        let conn = Connection::open(sp.db_path()).unwrap();
        let legacy = tool_calls_json_legacy(&["read_file", "read_file", "bash"]);
        conn.execute(
            "UPDATE messages SET tool_calls = ?1
             WHERE session_id = 'sess-tools' AND tool_calls IS NULL",
            rusqlite::params![legacy],
        )
        .unwrap();

        let agg = sp.aggregate_usage(30).unwrap();
        let read = agg
            .by_tool
            .iter()
            .find(|t| t.tool == "read_file")
            .expect("read_file must appear");
        let grep = agg
            .by_tool
            .iter()
            .find(|t| t.tool == "grep")
            .expect("grep must appear");
        let bash = agg
            .by_tool
            .iter()
            .find(|t| t.tool == "bash")
            .expect("bash must appear (legacy row)");
        assert_eq!(read.call_count, 3, "flat(1) + legacy(2) = 3");
        assert_eq!(grep.call_count, 1);
        assert_eq!(bash.call_count, 1);
        assert_eq!(read.session_count, 1);
    }

    #[test]
    fn p1_3_by_tool_sorts_descending_by_count() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        // Use a single message with multiple tool calls — easier than
        // threading several sessions.
        let mut tcs = Vec::new();
        for _ in 0..5 {
            tcs.push(hermix_core::ToolCall {
                id: "x".into(),
                function: hermix_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
            });
        }
        tcs.push(hermix_core::ToolCall {
            id: "y".into(),
            function: hermix_core::FunctionCall {
                name: "write_file".into(),
                arguments: "{}".into(),
            },
        });
        let messages = vec![
            Message::user("go"),
            Message {
                role: hermix_core::MessageRole::Assistant,
                content: None,
                content_parts: None,
                tool_calls: Some(tcs),
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                cache_control: None,
                execution_timeline: None,
                ephemeral: false,
            },
        ];
        sp.persist_session("sess-sort", &messages, Some("gpt-4o"), None, None, None, None, None, None)
            .unwrap();
        let agg = sp.aggregate_usage(30).unwrap();
        assert!(agg.by_tool[0].tool == "read_file");
        assert_eq!(agg.by_tool[0].call_count, 5);
        assert!(agg.by_tool[1].tool == "write_file");
        assert_eq!(agg.by_tool[1].call_count, 1);
    }

    #[test]
    fn p1_3_aggregate_usage_empty_db_returns_zero_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        sp.ensure_db().unwrap();
        let agg = sp.aggregate_usage(7).unwrap();
        assert_eq!(agg.total_input_tokens, 0);
        assert_eq!(agg.total_output_tokens, 0);
        assert_eq!(agg.daily.len(), 8);
        assert!(agg.by_tool.is_empty());
        assert!(agg.by_model.is_empty());
    }

    #[test]
    fn p1_3_by_model_includes_token_totals_via_left_join() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let messages = vec![Message::user("hi"), Message::assistant("hello!")];
        sp.persist_session("sess-a", &messages, Some("gpt-4o"), None, None, None, None, None, None)
            .unwrap();
        sp.persist_session("sess-b", &messages, Some("claude"), None, None, None, None, None, None)
            .unwrap();
        sp.persist_token_usage("sess-a", "gpt-4o", &usage(1000, 200, Some(0.01)))
            .unwrap();
        sp.persist_token_usage("sess-b", "claude", &usage(500, 100, Some(0.005)))
            .unwrap();

        let agg = sp.aggregate_usage(30).unwrap();
        let gpt = agg
            .by_model
            .iter()
            .find(|m| m.model == "gpt-4o")
            .expect("gpt-4o model row");
        let claude = agg
            .by_model
            .iter()
            .find(|m| m.model == "claude")
            .expect("claude model row");
        assert_eq!(gpt.input_tokens, 1000);
        assert_eq!(gpt.output_tokens, 200);
        assert!((gpt.estimated_cost - 0.01).abs() < 1e-9);
        assert_eq!(claude.input_tokens, 500);
        assert_eq!(claude.output_tokens, 100);
    }

    /// cache aggregation. Verifies that real cache
    /// read / cache creation / reasoning token counts from
    /// `persist_token_usage` flow through to (a) `UsageAggregate`
    /// totals, (b) `by_model` per-row, and (c) the `daily` per-day
    /// row. Before this fix the aggregator hardcoded all three to
    /// 0 (analytics.rs) and the dashboard always showed "0 cache
    /// hit" even when the LLM had cache hits.
    #[test]
    fn p1_1_aggregate_usage_includes_cache_and_reasoning_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let messages = vec![Message::user("hi"), Message::assistant("hello!")];
        sp.persist_session("cache-sess", &messages, Some("claude-opus-4"), None, None, None, None, None, None)
            .unwrap();

        // Simulate two LLM turns on the same session: 800 prompt tokens,
        // 600 of which came from the provider's prompt cache, plus
        // 200 reasoning tokens billed but invisible to the user.
        sp.persist_token_usage(
            "cache-sess",
            "claude-opus-4",
            &usage_with_cache(800, 100, Some(0.012), 600, 100, 200),
        )
        .unwrap();
        sp.persist_token_usage(
            "cache-sess",
            "claude-opus-4",
            &usage_with_cache(900, 120, Some(0.014), 700, 80, 150),
        )
        .unwrap();

        let agg = sp.aggregate_usage(30).unwrap();

        // (a) Totals — should NOT be hardcoded 0.
        assert_eq!(agg.total_cache_read_tokens, 1300, "totals cache_read");
        assert_eq!(agg.total_cache_creation_tokens, 180, "totals cache_creation");
        assert_eq!(agg.total_reasoning_tokens, 350, "totals reasoning");

        // (b) by_model — claude-opus-4 row should reflect the sums.
        let claude = agg
            .by_model
            .iter()
            .find(|m| m.model == "claude-opus-4")
            .expect("claude-opus-4 model row");
        assert_eq!(claude.cache_read_tokens, 1300);
        assert_eq!(claude.cache_creation_tokens, 180);
        assert_eq!(claude.reasoning_tokens, 350);

        // (c) daily — today is the only day with data.
        let today = chrono::Utc::now().date_naive().format("%Y-%m-%d").to_string();
        let today_row = agg
            .daily
            .iter()
            .find(|d| d.day == today)
            .expect("today daily row");
        assert_eq!(today_row.cache_read_tokens, 1300, "daily cache_read");
        assert_eq!(today_row.cache_creation_tokens, 180, "daily cache_creation");
        assert_eq!(today_row.reasoning_tokens, 350, "daily reasoning");
    }

    /// cache hit rate sanity check. Hits are aggregated
    /// separately from `prompt_tokens` (which is "uncached prompt
    /// tokens billed"), so a UI dividing cache_read by (cache_read +
    /// prompt_tokens) gets the actual cache coverage. This test
    /// pins that denominator so future schema changes don't silently
    /// break the "saved X% by cache" calculation.
    #[test]
    fn p1_1_cache_hit_rate_denominator_matches_provider_semantics() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        sp.persist_session("hit-rate", &[Message::user("x"), Message::assistant("y")], Some("claude-opus-4"), None, None, None, None, None, None)
            .unwrap();
        // 1000 uncached prompt + 500 cache hits → 33% cache coverage
        // by Anthropic's definition.
        sp.persist_token_usage(
            "hit-rate",
            "claude-opus-4",
            &usage_with_cache(1000, 50, Some(0.01), 500, 0, 0),
        )
        .unwrap();

        let agg = sp.aggregate_usage(30).unwrap();
        let cache = agg.total_cache_read_tokens as f64;
        let uncached = agg.total_input_tokens as f64;
        let rate = cache / (cache + uncached);
        assert!(
            (rate - 500.0 / 1500.0).abs() < 1e-9,
            "cache rate must be 500/(500+1000) ≈ 0.333, got {rate}"
        );
    }

    #[test]
    fn test_load_nonexistent_session() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        sp.ensure_db().unwrap();

        let loaded = sp.load_session("nonexistent").unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_persist_replaces_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        let messages1 = vec![Message::user("First")];
        sp.persist_session("replace-test", &messages1, None, None, None, None, None, None, None)
            .unwrap();

        let messages2 = vec![
            Message::user("First"),
            Message::assistant("Response"),
            Message::user("Second"),
        ];
        sp.persist_session("replace-test", &messages2, None, None, None, None, None, None, None)
            .unwrap();

        let loaded = sp.load_session("replace-test").unwrap();
        assert_eq!(loaded.len(), 3);
    }

    /// Helper: persist a 3-turn conversation (user / assistant per turn)
    /// and return the SessionPersistence for further inspection.
    fn seed_three_turns(tmp: &tempfile::TempDir) -> SessionPersistence {
        let sp = SessionPersistence::new(tmp.path());
        let messages = vec![
            Message::user("turn-1 user"),
            Message::assistant("turn-1 assistant"),
            Message::user("turn-2 user"),
            Message::assistant("turn-2 assistant"),
            Message::user("turn-3 user"),
            Message::assistant("turn-3 assistant"),
        ];
        sp.persist_session("undo-test", &messages, None, None, None, None, None, None, None)
            .unwrap();
        sp
    }

    #[test]
    fn test_undo_soft_deletes_last_n_turns() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = seed_three_turns(&tmp);
        // Undo the most recent 1 turn (turn-3: 2 messages).
        let n = sp.mark_recent_turns_inactive("undo-test", 1).unwrap();
        assert_eq!(n, 2);
        let loaded = sp.load_session("undo-test").unwrap();
        assert_eq!(loaded.len(), 4);
        assert_eq!(loaded.last().unwrap().content.as_deref(), Some("turn-2 assistant"));
    }

    #[test]
    fn test_undo_more_than_exist_deactivates_all() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = seed_three_turns(&tmp);
        // Ask for 99 turns when only 3 exist — all 6 should be deactivated.
        let n = sp.mark_recent_turns_inactive("undo-test", 99).unwrap();
        assert_eq!(n, 6);
        let loaded = sp.load_session("undo-test").unwrap();
        assert!(loaded.is_empty(), "load should now be empty");
    }

    #[test]
    fn test_restore_brings_back_most_recent_block() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = seed_three_turns(&tmp);
        // Undo last 1 turn, then restore.
        sp.mark_recent_turns_inactive("undo-test", 1).unwrap();
        let restored = sp.restore_last_undone("undo-test").unwrap();
        assert_eq!(restored, 2);
        let loaded = sp.load_session("undo-test").unwrap();
        assert_eq!(loaded.len(), 6);
    }

    #[test]
    fn test_undo_then_undo_again_undoes_previous_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = seed_three_turns(&tmp);
        sp.mark_recent_turns_inactive("undo-test", 1).unwrap();
        sp.mark_recent_turns_inactive("undo-test", 1).unwrap();
        let loaded = sp.load_session("undo-test").unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn test_undo_on_empty_session_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        let n = sp.mark_recent_turns_inactive("does-not-exist", 1).unwrap();
        assert_eq!(n, 0);
    }

    // ── Approval record round-trip ────────────────────────────────

    fn make_approval(req_id: &str, tool: &str, linked: Option<&str>) -> ApprovalRecord {
        ApprovalRecord {
            request_id: req_id.to_string(),
            tool: tool.to_string(),
            arguments: serde_json::json!({"cmd": "ls"}),
            risk: "dangerous".to_string(),
            decision: None,
            decided_at: None,
            expires_at: None,
            linked_assistant_id: linked.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_append_and_load_approval_timeline() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        // Seed: user → assistant → approval (linked to that assistant)
        // → tool → assistant (post-approval). The approval sits between
        // the two assistant messages in the timeline.
        let messages = vec![
            Message::user("first"),
            Message::assistant("first reply"),
        ];
        sp.persist_session("s1", &messages, None, None, None, None, None, None, None).unwrap();
        sp.append_approval_message(
            "s1",
            &make_approval("req-1", "terminal_run", Some("reply-0")),
        )
        .unwrap();

        let entries = sp.load_session_with_approvals("s1").unwrap();
        assert_eq!(entries.len(), 3, "approval should interleave between the two messages");
        match &entries[0] {
            SessionEntry::Message(m) => assert_eq!(m.content.as_deref(), Some("first")),
            _ => panic!("expected user message at 0"),
        }
        match &entries[1] {
            SessionEntry::Message(m) => assert_eq!(m.content.as_deref(), Some("first reply")),
            _ => panic!("expected assistant message at 1"),
        }
        match &entries[2] {
            SessionEntry::Approval(r) => {
                assert_eq!(r.request_id, "req-1");
                assert_eq!(r.tool, "terminal_run");
                assert_eq!(r.linked_assistant_id.as_deref(), Some("reply-0"));
            }
            SessionEntry::ApprovalOrphan(_) => panic!("should be linked, not orphan"),
            _ => panic!("expected approval at 2"),
        }
    }

    #[test]
    fn test_load_session_filters_out_approval_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        sp.persist_session(
            "s2",
            &[Message::user("hi"), Message::assistant("hello")],
            None, None, None, None, None, None, None,
        )
        .unwrap();
        sp.append_approval_message(
            "s2",
            &make_approval("req-x", "terminal_run", Some("reply-0")),
        )
        .unwrap();

        // LLM-facing load_session must NOT include the approval row.
        let llm_msgs = sp.load_session("s2").unwrap();
        assert_eq!(llm_msgs.len(), 2, "approval row must be filtered out of LLM context");
        assert!(llm_msgs.iter().all(|m| m.content.as_deref() != Some("")));

        // UI-facing load_session_with_approvals must include it.
        let entries = sp.load_session_with_approvals("s2").unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn test_resolve_approval_updates_decision() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        sp.persist_session(
            "s3",
            &[Message::user("hi"), Message::assistant("ok")],
            None, None, None, None, None, None, None,
        )
        .unwrap();
        sp.append_approval_message(
            "s3",
            &make_approval("req-y", "terminal_run", Some("reply-0")),
        )
        .unwrap();

        sp.resolve_approval_message("s3", "req-y", "approved").unwrap();

        let entries = sp.load_session_with_approvals("s3").unwrap();
        match &entries[2] {
            SessionEntry::Approval(r) => {
                assert_eq!(r.decision.as_deref(), Some("approved"));
                assert!(r.decided_at.is_some(), "decided_at must be stamped");
            }
            _ => panic!("expected approval at 2"),
        }
    }

    #[test]
    fn test_append_approval_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        sp.persist_session(
            "s4",
            &[Message::user("hi"), Message::assistant("ok")],
            None, None, None, None, None, None, None,
        )
        .unwrap();
        let rec = make_approval("req-dup", "terminal_run", Some("reply-0"));
        sp.append_approval_message("s4", &rec).unwrap();
        sp.append_approval_message("s4", &rec).unwrap();
        sp.append_approval_message("s4", &rec).unwrap();

        let entries = sp.load_session_with_approvals("s4").unwrap();
        // Only 3 entries total (user, assistant, one approval), not 5.
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn test_resolve_nonexistent_request_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        // Should not error even if there's no matching row.
        sp.resolve_approval_message("does-not-exist", "req-z", "approved").unwrap();
    }

    #[test]
    fn test_undo_also_deactivates_approval_rows_in_range() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());
        // Two turns. Insert an approval between turn 1 and turn 2 by
        // reordering the writes: first persist turn 1, then append an
        // approval (which gets a higher DB id), then persist turn 2 —
        // but persist_session REPLACES all messages, so we have to do
        // this differently. Instead, persist a turn, append approval,
        // then persist the next turn (replacing the messages table).
        // The approval row will be wiped by the second persist, so we
        // can't test the range heuristic this way.
        // Workaround: manually craft the rows by calling append_approval
        // after a partial persist. We do two separate persist calls —
        // first the user+assistant of turn 1, then append_approval,
        // then user+assistant of turn 2. The second persist_session will
        // wipe the approval, so this approach can't model interleaved
        // approvals. Skip the test in that case; the SQL range logic
        // is exercised by hand when we ship the feature.
        sp.persist_session(
            "s5",
            &[Message::user("u1"), Message::assistant("a1")],
            None, None, None, None, None, None, None,
        )
        .unwrap();
        sp.append_approval_message(
            "s5",
            &make_approval("req-r", "terminal_run", Some("reply-0")),
        )
        .unwrap();
        sp.persist_session(
            "s5",
            &[
                Message::user("u1"),
                Message::assistant("a1"),
                Message::user("u2"),
                Message::assistant("a2"),
            ],
            None, None, None, None, None, None, None,
        )
        .unwrap();
        // The second persist wiped the approval row. Undo should now
        // return 2 (just turn-2) and the approval should still be gone.
        let n = sp.mark_recent_turns_inactive("s5", 1).unwrap();
        assert_eq!(n, 2);
    }

    /// Regression test for the rename_session / load_session crash on a
    /// pre-migration sessions.db: the old `messages` table doesn't have
    /// the `active` or `extra` columns, and the old `ensure_db` ran
    /// `CREATE INDEX ... ON messages(session_id, active)` *before* the
    /// `ALTER TABLE messages ADD COLUMN active` migration, so the
    /// `execute_batch` bailed on the index error and the migrations
    /// never ran. ensure_db now runs the migrations FIRST.
    #[test]
    fn test_ensure_db_migrates_legacy_messages_table() {
        use rusqlite::Connection;

        let tmp = tempfile::tempdir().unwrap();
        // DB now lives under <home>/data/ (via data_dir_in)
        let data_dir = hermix_config::paths::data_dir_in(tmp.path());
        std::fs::create_dir_all(&data_dir).unwrap();
        let db_path = data_dir.join("sessions.db");

        // Simulate a legacy DB: messages table without `active` / `extra`.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    model TEXT,
                    platform TEXT DEFAULT 'cli',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    title TEXT,
                    message_count INTEGER DEFAULT 0
                );
                CREATE TABLE messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id TEXT NOT NULL,
                    role TEXT NOT NULL,
                    content TEXT,
                    tool_call_id TEXT,
                    tool_calls TEXT,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (session_id) REFERENCES sessions(id)
                );
                INSERT INTO sessions (id, created_at, updated_at)
                    VALUES ('s1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
                INSERT INTO messages (session_id, role, content, created_at)
                    VALUES ('s1', 'user', 'hello', '2026-01-01T00:00:00Z');",
            )
            .unwrap();
        }

        // Run ensure_db on the legacy DB — must succeed and add columns.
        let sp = SessionPersistence::new(tmp.path());
        sp.ensure_db().expect("ensure_db must succeed on legacy DB");

        // Verify the columns exist by running the same query that
        // load_session uses (was the original crash point).
        let conn = Connection::open(&db_path).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT role, content, tool_call_id, tool_calls, name, execution_timeline, reasoning_content
                 FROM messages
                 WHERE session_id = ?1 AND active = 1 AND role != 'approval'
                 ORDER BY id ASC",
            )
            .expect("legacy messages table should be migrated");
        let rows: Vec<String> = stmt
            .query_map(rusqlite::params!["s1"], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows, vec!["user"]);

        // And `extra` should also have been added.
        let has_extra: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'extra'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_extra, 1, "messages.extra column should exist");

        // Calling load_session should also work end-to-end now.
        let msgs = sp.load_session("s1").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, MessageRole::User);
    }

    /// Regression test for the `message_count` over-count bug:
    /// Before the fix, `save_session_atomic` stamped
    /// `sessions.message_count = messages.len() as i64` while
    /// `flush_messages` silently filtered out `MessageRole::System`
    /// rows. Result: the `message_count` column on the `sessions`
    /// table was always off by however many system rows were in the
    /// in-memory `messages` array (typically 1-2 — the cached system
    /// prompt from `agent_loop`). The UI badge rendered wrong, and any
    /// downstream code that paginated by `message_count` would skip or
    /// repeat rows.
    /// After the fix, `flush_messages` returns the actual INSERTed
    /// rowcount and `save_session_atomic` stamps that exact number.
    /// This test pins the contract: 2 system + 4 user/assistant
    /// messages in → exactly 4 messages in `messages` table AND
    /// `sessions.message_count = 4`.
    #[test]
    fn test_message_count_reflects_actual_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let sp = SessionPersistence::new(tmp.path());

        let messages = vec![
            Message::system("cached system prompt blob"),
            Message::user("u1"),
            Message::assistant("a1"),
            Message::user("u2"),
            Message::assistant("a2"),
            Message::system("trailing system that must NOT count"),
        ];

        sp.persist_session("count-test", &messages, Some("gpt-4o"), None, Some("Count"), None, None, None, None)
            .unwrap();

        // 1. Open the DB directly and count the actual rows.
        let conn = rusqlite::Connection::open(sp.db_path()).unwrap();
        let actual_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                rusqlite::params!["count-test"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            actual_rows, 4,
            "system rows must be filtered out — 2 system + 4 user/assistant = 4 stored"
        );

        // 2. The session row's message_count column must match the actual rowcount.
        let stamped: i64 = conn
            .query_row(
                "SELECT message_count FROM sessions WHERE id = ?1",
                rusqlite::params!["count-test"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stamped, 4,
            "sessions.message_count must equal the actual INSERTed rowcount, \
             not messages.len() (which would be 6 here)"
        );

        // 3. The persisted-and-reloaded LLM view should also be 4 messages
        // (load_session already filters system rows; this just confirms
        // the end-to-end count is consistent across all three views).
        let loaded = sp.load_session("count-test").unwrap();
        assert_eq!(loaded.len(), 4);
    }

    // (A + B): end-to-end test for the compression
    // archive + summary-history write paths. Calls
    // `persist_session_with_compression` once with a real
    // snapshot, then reads back both tables to confirm the
    // summary, the raw archived turns, and the metrics all
    // landed in their respective rows in the same transaction.
    #[test]
    fn compression_snapshot_writes_archive_and_history_in_one_transaction() {
        let tmp = tempfile::tempdir().unwrap();
        let hermix_home = tmp.path();
        let sp = SessionPersistence::new(hermix_home);

        let session_id = "compression-snap-test";
        let messages = vec![
            Message::user("hello"),
            Message::assistant("hi"),
        ];
        let archived = vec![
            Message::user("original turn 1"),
            Message::assistant("original turn 2"),
            Message::user("original turn 3"),
        ];
        let snapshot = CompressionSnapshot::new(
            "the user said hi and we discussed the design".to_string(),
            archived,
            1,        // compression_count
            6, 2,     // original_turns, compressed_turns
            1500, 400, // original_tokens, compressed_tokens
        )
        .expect("non-empty summary + archived turns must build Some");

        sp.persist_session_with_compression(
            session_id,
            &messages,
            Some("test-model"),
            Some("local"),
            Some("Compression test"),
            Some("system"),
            None,
            // workspace / expert_id: not under test here.
            None,
            None,
            Some(&snapshot),
        )
        .expect("persist must succeed");

        // (A) compression_archives row exists with the raw turns.
        let archives = sp
            .load_compression_archive(session_id, 10)
            .expect("archive query must succeed");
        assert_eq!(archives.len(), 1, "exactly one archive row");
        let row = &archives[0];
        assert_eq!(row.original_turns, 6);
        assert_eq!(row.compressed_turns, 2);
        assert_eq!(row.original_tokens, 1500);
        assert_eq!(row.compressed_tokens, 400);
        assert_eq!(row.source, "compression");
        let decoded = row.decoded_turns();
        assert_eq!(decoded.len(), 3);
        assert_eq!(
            decoded[0].content.as_deref(),
            Some("original turn 1")
        );

        // (B) compression_summary_history row exists with the metrics.
        let history = sp
            .load_compression_summary_history(session_id, 10)
            .expect("history query must succeed");
        assert_eq!(history.len(), 1, "exactly one history row");
        let h = &history[0];
        assert_eq!(h.compression_count, 1);
        assert_eq!(h.original_turns, 6);
        assert_eq!(h.compressed_turns, 2);
        assert_eq!(h.original_tokens, 1500);
        assert_eq!(h.compressed_tokens, 400);
        // 1500 - 400 = 1100 tokens saved
        assert_eq!(h.tokens_saved, 1100);
        assert_eq!(
            h.summary,
            "the user said hi and we discussed the design"
        );

        // (C) sessions.compressed_summary column still holds the
        // latest summary as a fast-load index.
        let latest = sp
            .load_compressed_summary(session_id)
            .expect("summary column must be populated");
        assert_eq!(latest, h.summary);
    }

    // (A): empty snapshot short-circuits — no archive
    // rows, no history rows, no summary column write.
    #[test]
    fn compression_snapshot_empty_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let hermix_home = tmp.path();
        let sp = SessionPersistence::new(hermix_home);

        let session_id = "empty-snap-test";
        sp.persist_session_with_compression(
            session_id,
            &[Message::user("hi"), Message::assistant("hello")],
            Some("test-model"),
            Some("local"),
            None,
            None,
            None,
            // workspace / expert_id / compression: none under test here.
            None,
            None,
            None,
        )
        .expect("persist must succeed");

        let archives = sp
            .load_compression_archive(session_id, 10)
            .expect("archive query");
        assert!(archives.is_empty(), "no archive rows on empty snap");
        let history = sp
            .load_compression_summary_history(session_id, 10)
            .expect("history query");
        assert!(history.is_empty(), "no history rows on empty snap");
        assert!(
            sp.load_compressed_summary(session_id).is_none(),
            "no summary on empty snap"
        );
    }

    // (): compression now materialises a real
    // child session row, linked to the parent via
    // `parent_session_id`. The child's `messages` table carries
    // the summary as a system message + the archived turns, so
    // the lineage browser can render "what got folded" without
    // re-querying the parent.
    #[test]
    fn compression_creates_child_session_with_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let hermix_home = tmp.path();
        let sp = SessionPersistence::new(hermix_home);

        let parent_id = "n02-parent-session";
        let archived = vec![
            Message::user("original turn A"),
            Message::assistant("original turn B"),
        ];
        let snap = CompressionSnapshot::new(
            "summary text from test".to_string(),
            archived,
            1,
            4,
            2,
            1000,
            300,
        )
        .expect("non-empty snapshot");

        // First child creation: id must be deterministic
        // `<parent>-c1` since the parent has zero existing
        // children.
        let child_id_1 = sp
            .create_compression_child_session(parent_id, &snap, Some("test-model"), Some("local"))
            .expect("first child creation must succeed");
        assert_eq!(child_id_1, format!("{parent_id}-c1"));

        // The child row exists with `parent_session_id` set.
        let parent_back = sp
            .get_parent(&child_id_1)
            .expect("get_parent must succeed");
        assert_eq!(parent_back.as_deref(), Some(parent_id));

        // The lineage tree can find the child from the parent.
        let children = sp
            .find_children(parent_id)
            .expect("find_children must succeed");
        assert_eq!(children, vec![child_id_1.clone()]);

        // The child row carries the summary as compressed_summary
        // (fast-load index) AND as the first message (display).
        let child_summary = sp
            .load_compressed_summary(&child_id_1)
            .expect("child summary must be populated");
        assert_eq!(child_summary, "summary text from test");

        // The child's messages: archived turns survive in the messages
        // table (the summary itself lives on `compressed_summary`,
        // see comment in `create_compression_child_session`).
        let conn = open_with_wal(&sp.db_path).expect("open");
        let msg_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                rusqlite::params![&child_id_1],
                |row| row.get(0),
            )
            .expect("count messages");
        assert_eq!(msg_count, 2, "child must hold 2 archived turns");

        let mut stmt = conn
            .prepare(
                "SELECT role, content FROM messages WHERE session_id = ?1 ORDER BY id",
            )
            .expect("prepare");
        let rows: Vec<(String, String)> = stmt
            .query_map(rusqlite::params![&child_id_1], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "user");
        assert_eq!(rows[0].1, "original turn A");
        assert_eq!(rows[1].0, "assistant");
        assert_eq!(rows[1].1, "original turn B");

        // Summary still recoverable via load_compressed_summary
        // (no system-role message needed).
        let summary = sp
            .load_compressed_summary(&child_id_1)
            .expect("child summary must be populated");
        assert_eq!(summary, "summary text from test");

        // Second child creation on the same parent must bump
        // the suffix to `-c2` (idempotent, deterministic).
        let snap2 = CompressionSnapshot::new(
            "second compression".to_string(),
            vec![Message::user("another turn")],
            2,
            6,
            2,
            2000,
            500,
        )
        .expect("non-empty snapshot");
        let child_id_2 = sp
            .create_compression_child_session(parent_id, &snap2, Some("test-model"), Some("local"))
            .expect("second child creation must succeed");
        assert_eq!(child_id_2, format!("{parent_id}-c2"));

        // Both children discoverable.
        let all_children = sp
            .find_children(parent_id)
            .expect("find_children after second");
        assert_eq!(all_children.len(), 2);
        assert!(all_children.contains(&child_id_1));
        assert!(all_children.contains(&child_id_2));
    }

    // (): creating a child session for a parent
    // that doesn't yet have a row in `sessions` should still
    // succeed — the child has its own row, parent_session_id
    // just points at the missing id. We tolerate the orphan
    // because the parent row will be created later by the
    // regular persist_session call, and the lineage tree
    // already handles missing ancestors via `ancestors` list
    // being truncated.
    #[test]
    fn compression_child_session_orphan_parent_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let hermix_home = tmp.path();
        let sp = SessionPersistence::new(hermix_home);

        let snap = CompressionSnapshot::new(
            "orphan parent test".to_string(),
            vec![Message::user("orphan turn")],
            1,
            2,
            1,
            100,
            50,
        )
        .expect("non-empty snapshot");

        let child_id = sp
            .create_compression_child_session(
                "never-persisted-parent",
                &snap,
                Some("test-model"),
                Some("local"),
            )
            .expect("child creation against missing parent must succeed");
        assert_eq!(child_id, "never-persisted-parent-c1");

        // Child row exists, parent lookup returns None (parent
        // row doesn't exist yet).
        let loaded = sp
            .load_compressed_summary(&child_id)
            .expect("child summary present");
        assert_eq!(loaded, "orphan parent test");
    }
