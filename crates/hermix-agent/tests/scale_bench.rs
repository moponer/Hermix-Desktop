//! 规模压力下的两个真实问题：路由质量与持久化成本。
//!
//! ## 为什么要有这个文件
//!
//! Decision 18/26a 把自动沉淀接进 run loop 后，`skills/auto/` 会随使用线性
//! 增长，而它们和手写技能同构、同样进匹配池。Decision 27 加了两层治理
//! （降权 0.75 + 门槛不软化 0.3），但**没有在规模下验证过**——治理是否真的
//! 挡得住"沉淀 50 个技能"的情形，只有压出来才知道。
//!
//! 第二个问题是持久化：Decision 12 把每轮全表重写改成增量前缀合并，预期把
//! 长会话的 IO 从 O(n²) 降到 O(增量)。这里用 500 条消息量一次墙钟时间。
//!
//! `--ignored --nocapture` 运行。

use std::fs;
use std::path::Path;
use std::time::Instant;

use hermix_agent::skill_loader::SkillLoader;
use hermix_core::Message;

/// 中性查询：日常请求，不应激活任何技能。
const NEUTRAL: &[&str] = &[
    "帮我把这些照片按日期整理一下",
    "写一个快速排序的实现",
    "explain quantum entanglement simply",
    "the printer is not responding",
    "how do I configure nginx reverse proxy",
    "translate this paragraph into french",
    "what is the capital of Australia",
    "最好的设计工具是什么",
    "如何清理 c 盘空间",
    "review this pull request please",
];

/// 从真实技能库复制一份到 `dst`（只复制 SKILL.md，够 loader 用）。
fn copy_real_library(src: &Path, dst: &Path) -> usize {
    let mut copied = 0;
    let Ok(cats) = fs::read_dir(src) else {
        return 0;
    };
    for cat in cats.flatten() {
        let cat_path = cat.path();
        if !cat_path.is_dir() {
            continue;
        }
        let cat_name = cat.file_name();
        if cat_name == "auto" {
            // 基线不含沉淀产物——它们正是被测对象。
            continue;
        }
        let Ok(skills) = fs::read_dir(&cat_path) else {
            continue;
        };
        for skill in skills.flatten() {
            let md = skill.path().join("SKILL.md");
            if !md.is_file() {
                continue;
            }
            let target_dir = dst.join(&cat_name).join(skill.file_name());
            if fs::create_dir_all(&target_dir).is_err() {
                continue;
            }
            if fs::copy(&md, target_dir.join("SKILL.md")).is_ok() {
                copied += 1;
            }
        }
    }
    copied
}

/// 生成 `n` 个"沉淀产物"。
///
/// **必须用与 [`NEUTRAL`] 不相干的文本。** 第一版把它们设成了中性查询的原话，
/// 结果"照片整理"的沉淀技能被"照片整理"查询命中——那是**正确激活**（下次复用
/// 正是目的），却被我记成了污染。真实要测的是：沉淀了一堆**别的任务**的技能
/// 之后，无关查询会不会被带出来。
fn seed_auto_skills(dst: &Path, n: usize) -> usize {
    let sources = [
        "部署 kubernetes 集群并配置 ingress",
        "分析销售数据并生成月度报告",
        "给用户表加索引优化慢查询",
        "批量重命名发票扫描件",
        "把会议录音转成纪要和待办",
    ];
    for i in 0..n {
        let src = sources[i % sources.len()];
        let dir = dst.join("auto").join(format!("auto-{i:03}"));
        if fs::create_dir_all(&dir).is_err() {
            continue;
        }
        let body = format!(
            "---\nname: auto-{i:03}\ndescription: {src}\ntags: [auto]\n---\n\
             # auto-{i:03}\n\n本技能由 agent 自动沉淀。\n"
        );
        let _ = fs::write(dir.join("SKILL.md"), body);
    }
    n
}

fn hits_for(dir: &Path) -> Vec<(usize, Vec<String>)> {
    let mut loader = SkillLoader::with_dir(dir);
    loader.scan();
    NEUTRAL
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let hits: Vec<String> = loader
                .match_message(q)
                .iter()
                .map(|m| format!("{}={:.2}", m.name, m.score))
                .collect();
            (i, hits)
        })
        .collect()
}

#[test]
#[ignore = "prints a report; run with --ignored --nocapture"]
fn auto_skill_pileup_does_not_pollute_routing() {
    let home = hermix_config::paths::hermix_home();
    let real = home.join("skills");
    if !real.is_dir() {
        eprintln!("SKIP: no real skills library at {}", real.display());
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let baseline = tmp.path().join("baseline");
    let pressured = tmp.path().join("pressured");
    fs::create_dir_all(&baseline).unwrap();
    fs::create_dir_all(&pressured).unwrap();

    let real_count = copy_real_library(&real, &baseline);
    let _ = copy_real_library(&real, &pressured);
    let auto_count = seed_auto_skills(&pressured, 50);

    println!("\n=== 路由规模压力测试 ===");
    println!("基线：{real_count} 个手写技能");
    println!("压力：{real_count} 个手写 + {auto_count} 个沉淀产物\n");

    let base = hits_for(&baseline);
    let press = hits_for(&pressured);

    let mut polluted = 0usize;
    for i in 0..NEUTRAL.len() {
        let b = &base[i].1;
        let p = &press[i].1;
        // 压力组里多出来的、分数 >= 0.3 的命中 = 被沉淀产物污染。
        let extra: Vec<&String> = p
            .iter()
            .filter(|h| !b.contains(h))
            .filter(|h| {
                h.rsplit('=')
                    .next()
                    .and_then(|s| s.parse::<f32>().ok())
                    .is_some_and(|s| s >= 0.3)
            })
            .collect();
        if !extra.is_empty() {
            polluted += 1;
            println!("  [污染] {:?}\n    基线: {b:?}\n    压力: {p:?}\n    多出: {extra:?}", NEUTRAL[i]);
        } else {
            println!("  [ok]   {:?} → {p:?}", NEUTRAL[i]);
        }
    }

    println!(
        "\n结果：{polluted}/{} 条中性查询被沉淀产物污染（阈值 >= 0.3 才算污染）",
        NEUTRAL.len()
    );
    assert_eq!(
        polluted, 0,
        "治理失效：沉淀技能堆积后污染了路由。检查 AUTO_SKILL_SCORE_FACTOR 与 AUTO_SKILL_MIN_SCORE。"
    );
}

#[test]
#[ignore = "prints a report; run with --ignored --nocapture"]
fn incremental_persist_cost_at_500_messages() {
    use hermix_agent::session_persistence::SessionPersistence;

    let tmp = tempfile::tempdir().unwrap();
    let sp = SessionPersistence::new(tmp.path());

    let mut msgs: Vec<Message> = Vec::new();
    for i in 0..500 {
        if i % 2 == 0 {
            msgs.push(Message::user(format!("问题 {i}：这是一段用于测试持久化成本的中文文本")));
        } else {
            msgs.push(Message::assistant(format!("回答 {i}：同样长度的回复内容用于模拟真实会话")));
        }
    }

    // 首轮：写入 500 条（冷启动，全量）。
    let t = Instant::now();
    sp.persist_session_with_compression(
        "bench-500", &msgs, Some("m"), Some("local"), None, None, None, None, None, None,
    )
    .expect("initial flush");
    let cold = t.elapsed();

    // 稳态：追加 2 条（一轮的典型增量）。
    let mut turn2 = msgs.clone();
    turn2.push(Message::user("第二轮问题"));
    turn2.push(Message::assistant("第二轮回答"));
    let t = Instant::now();
    sp.persist_session_with_compression(
        "bench-500", &turn2, Some("m"), Some("local"), None, None, None, None, None, None,
    )
    .expect("steady flush");
    let steady = t.elapsed();

    // 再跑 5 轮增量，取均值。
    let mut acc = Vec::new();
    let mut cur = turn2.clone();
    for i in 0..5 {
        cur.push(Message::user(format!("追加问题 {i}")));
        cur.push(Message::assistant(format!("追加回答 {i}")));
        let t = Instant::now();
        sp.persist_session_with_compression(
            "bench-500", &cur, Some("m"), Some("local"), None, None, None, None, None, None,
        )
        .expect("appended flush");
        acc.push(t.elapsed());
    }
    let avg: std::time::Duration = acc.iter().sum::<std::time::Duration>() / acc.len() as u32;

    // 分离成本：`persist_pre_turn` 走同一套 DB 事务但**跳过 markdown 导出**
    // （Decision 13）。两者的差值就是 `.md` 全量重写的代价——如果它占大头，
    // 那"增量 flush 已经很快"这个说法成立，剩下的瓶颈在导出而不在 DB。
    let mut acc_db = Vec::new();
    for i in 0..5 {
        cur.push(Message::user(format!("测量问题 {i}")));
        cur.push(Message::assistant(format!("测量回答 {i}")));
        let t = Instant::now();
        sp.persist_pre_turn("bench-500", &cur, Some("m"), Some("local"))
            .expect("db-only flush");
        acc_db.push(t.elapsed());
    }
    let avg_db: std::time::Duration =
        acc_db.iter().sum::<std::time::Duration>() / acc_db.len() as u32;

    println!("\n=== 持久化成本（500 条消息 + 每轮追加 2 条）===");
    println!("首轮全量写入 500 条    : {cold:?}");
    println!("第二轮（增量 + md 导出）: {steady:?}");
    println!("后续 5 轮平均          : {avg:?}");
    println!("仅 DB（跳过 md 导出）  : {avg_db:?}");
    println!(
        "增量相对全量的加速     : {:.1}×",
        cold.as_secs_f64() / avg.as_secs_f64().max(1e-9)
    );
    println!(
        "其中 markdown 导出占比 : {:.0}%",
        (avg.as_secs_f64() - avg_db.as_secs_f64()) / avg.as_secs_f64().max(1e-9) * 100.0
    );

    // 进一步分离：JSONL 镜像的成本。
    // 它是"DB COMMIT 之后的 best-effort 挂接"，若这里占大头，说明 DB 已经不慢，
    // 慢的是每轮的镜像维护。
    let t = Instant::now();
    sp.save_message_mirror("bench-mirror", &cur).expect("mirror cold");
    let mirror_cold = t.elapsed();
    let mut mirror_warm = Vec::new();
    for i in 0..3 {
        let mut m = cur.clone();
        m.push(Message::user(format!("镜像追加 {i}")));
        m.push(Message::assistant(format!("镜像回复 {i}")));
        let t = Instant::now();
        sp.save_message_mirror("bench-mirror", &m).expect("mirror warm");
        mirror_warm.push(t.elapsed());
    }
    let mirror_avg: std::time::Duration =
        mirror_warm.iter().sum::<std::time::Duration>() / mirror_warm.len() as u32;
    println!("JSONL 镜像首轮（全量） : {mirror_cold:?}");
    println!("JSONL 镜像增量平均     : {mirror_avg:?}");

    // 关键诊断：用小会话重复同样流程。若耗时与 500 条场景接近，说明剩余成本
    // 是**事务固定开销**（WAL 提交 + fsync），与消息数无关；若明显更低，说明
    // 增量快路径没命中、仍在全量扫描。
    let small = tempfile::tempdir().unwrap();
    let sp_small = SessionPersistence::new(small.path());
    let mut small_msgs: Vec<Message> = (0..50)
        .map(|i| {
            if i % 2 == 0 {
                Message::user(format!("小会话问题 {i}"))
            } else {
                Message::assistant(format!("小会话回答 {i}"))
            }
        })
        .collect();
    sp_small
        .persist_session_with_compression(
            "bench-50", &small_msgs, Some("m"), Some("local"), None, None, None, None, None, None,
        )
        .expect("small cold");
    let mut small_acc = Vec::new();
    for i in 0..5 {
        small_msgs.push(Message::user(format!("小追加问 {i}")));
        small_msgs.push(Message::assistant(format!("小追加答 {i}")));
        let t = Instant::now();
        sp_small
            .persist_session_with_compression(
                "bench-50", &small_msgs, Some("m"), Some("local"), None, None, None, None, None,
                None,
            )
            .expect("small warm");
        small_acc.push(t.elapsed());
    }
    let small_avg: std::time::Duration =
        small_acc.iter().sum::<std::time::Duration>() / small_acc.len() as u32;
    println!("50 条会话增量平均      : {small_avg:?}");
    println!(
        "500 条 / 50 条 耗时比  : {:.2}×（≈1.0 = 与消息数无关，成本在事务）",
        avg.as_secs_f64() / small_avg.as_secs_f64().max(1e-9)
    );

    let loaded = sp.load_session("bench-500").unwrap();
    assert_eq!(loaded.len(), 500 + 2 + 10 + 10, "数据必须一条不少");
}
