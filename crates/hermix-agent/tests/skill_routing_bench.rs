//! 技能路由量化基准：误激活率 / 漏激活率 / 分数分布。
//!
//! ## 为什么有这个文件
//!
//! 2026-10-20 的实测里，一句 "what is the best practice for compression"
//! 同时激活了 `context-compression`、`memory-systems`、`tool-design` 三个
//! 技能——后两个与查询无关。`p2_skill_activation_test` 把这种现象钉成了
//! 失败，但失败信息只有名字，没有"差多少、为什么"。
//!
//! 阈值该调到多少不能靠感觉。这个基准输出三件事：
//! 1. **误激活**：与所有技能无关的查询，有几个技能被激活、分数多少；
//! 2. **漏激活**：明确相关的查询，期望技能是否进 top-N；
//! 3. **阈值扫描**：0.00–1.00 每 0.05 一档的 (误激活, 漏激活)  trade-off。
//!
//! 语料是**确定性**的（自建临时技能目录，不读用户 `~/.hermix/skills`），
//! 所以可以进 CI。`--nocapture` 看明细。

use std::fs;

use hermix_agent::skill_loader::SkillLoader;

/// 三个技能，与 `p2_skill_activation_test::seed_three_skills` 同源——两个
/// 文件测同一套数据，基准结果可以直接解释那个失败。
fn seed_skills(dir: &std::path::Path) {
    let skills: &[(&str, &str, &[&str])] = &[
        (
            "context-compression",
            "Best practices for lossy compression of conversation history.",
            &["compression", "context"],
        ),
        (
            "memory-systems",
            "Architecture patterns for persistent agent memory across sessions.",
            &["memory", "architecture"],
        ),
        (
            "tool-design",
            "Principles for designing LLM-callable tools that are safe and effective.",
            &["tools", "design"],
        ),
        // 中文描述技能：专门用来钉住"向量阈值按语言分层"——英文查询走 0.45，
        // 中文查询走 0.18（两条不同的底噪，见 `semantic_min_score_cjk`）。
        (
            "visual-design",
            "界面美化、配色方案与排版布局规范。",
            &["design"],
        ),
    ];
    for (slug, desc, tags) in skills {
        let d = dir.join(slug);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("SKILL.md"),
            format!(
                "---\nname: {slug}\ndescription: {desc}\ntags: [{}]\n---\n# {slug}\n",
                tags.join(", ")
            ),
        )
        .unwrap();
    }
}

/// (查询, 期望技能, 说明)。期望为空串 = 负样本（不应激活任何技能）。
const CASES: &[(&str, &str, &str)] = &[
    // —— 正样本：查询与某个技能明确相关 ——
    ("how do I do context compression", "context-compression", "直给关键词"),
    ("tell me about persistent agent memory", "memory-systems", "直给关键词"),
    ("design a safe tool for the LLM", "tool-design", "直给关键词"),
    ("compres", "context-compression", "单词模糊（缺尾字母）"),
    ("memry layout", "memory-systems", "单词模糊（缺字母）"),
    ("I want memory compression", "memory-systems", "两个关键词"),
    ("压缩对话历史的方案", "context-compression", "CJK 触发"),
    // 2026-10-20 的实测误激活案例：context-compression 激活是**对的**
    // （查询就是问 compression），误激活的是同时蹦出来的 tool-design
    // (0.25) 与 memory-systems (0.33)。按期望技能记，FP 由扫描统计。
    ("what is the best practice for compression", "context-compression", "实测误激活案例"),
    // —— 负样本：与这三个技能都无关 ——
    ("帮我把这些照片按日期整理一下", "", "CJK 无关"),
    ("how do I configure nginx reverse proxy", "", "运维无关"),
    ("写一个快速排序的实现", "", "编码无关"),
    ("explain quantum entanglement simply", "", "科普无关"),
    ("帮我写一份周报", "", "CJK 写作"),
    ("translate this paragraph into french", "", "翻译"),
    ("the printer is not responding", "", "硬件无关"),
    ("review this pull request please", "", "评审无关"),
    ("最好的设计工具是什么", "", "CJK 含设计但无关"),
    ("如何清理 c 盘空间", "", "CJK 存储类"),
    // —— 中文向量层：证明阈值按语言分层（英文 0.45 / 中文 0.18）——
    ("帮我美化一下界面配色", "visual-design", "CJK 释义（关键词够不到）"),
    ("帮我清理C盘垃圾文件", "", "CJK 负样本（同一技能的对照）"),
];

fn loader_for(dir: &std::path::Path, min_score: f32) -> SkillLoader {
    let mut l = SkillLoader::with_dir(dir).with_min_score(min_score);
    l.scan();
    l
}

/// 单阈值下的 (误激活数, 漏激活数, 明细)。
fn evaluate(dir: &std::path::Path, min_score: f32) -> (usize, usize, Vec<String>) {
    let loader = loader_for(dir, min_score);
    let mut false_positives = 0usize;
    let mut misses = 0usize;
    let mut detail = Vec::new();

    for (query, expected, note) in CASES {
        let hits = loader.match_message(query);
        let names: Vec<&str> = hits.iter().map(|m| m.name.as_str()).collect();
        let scores: Vec<String> = hits
            .iter()
            .map(|m| format!("{}={:.2}", m.name, m.score))
            .collect();

        if expected.is_empty() {
            if !names.is_empty() {
                false_positives += 1;
                detail.push(format!("  FP   [{note}] {query:?} -> {scores:?}"));
            }
        } else if !names.contains(expected) {
            misses += 1;
            detail.push(format!(
                "  MISS [{note}] {query:?} -> {scores:?} (期望 {expected})"
            ));
        }
    }
    (false_positives, misses, detail)
}

#[test]
fn bench_reports_false_positive_rate_by_threshold() {
    let tmp = tempfile::tempdir().unwrap();
    seed_skills(tmp.path());

    let negatives = CASES.iter().filter(|(_, e, _)| e.is_empty()).count();
    let positives = CASES.len() - negatives;

    println!("\n=== 技能路由阈值扫描（{positives} 正样本 / {negatives} 负样本）===");
    println!("{:<10} {:>8} {:>8} {:>10}", "min_score", "误激活", "漏激活", "误激活率");

    let mut best: Option<(f32, usize, usize)> = None;
    for step in 0..=20 {
        let min = step as f32 * 0.05; // 0.00 .. 1.00
        let (fp, miss, _) = evaluate(tmp.path(), min);
        let rate = fp as f32 / negatives as f32 * 100.0;
        println!("{:<10.2} {:>8} {:>8} {:>9.0}%", min, fp, miss, rate);
        let better = match best {
            None => true,
            Some((_, b_fp, b_miss)) => (fp, miss) < (b_fp, b_miss),
        };
        if better {
            best = Some((min, fp, miss));
        }
    }

    let (best_min, best_fp, best_miss) = best.expect("扫描必须产出结果");
    println!(
        "\n最优阈值: min_score={best_min:.2} -> 误激活 {best_fp}/{negatives}，漏激活 {best_miss}/{positives}"
    );
    println!("\n=== 该阈值下的明细 ===");
    let (_, _, detail) = evaluate(tmp.path(), best_min);
    for line in detail {
        println!("{line}");
    }

    // 每个负样本在各阈值下的最高分——用来判断阈值该卡在哪。
    println!("\n=== 全量分数分布（min_score=0.00，即未过滤）===");
    let loader = loader_for(tmp.path(), 0.0);
    let mut fp_max = 0.0f32;
    let mut pos_min = 1.0f32;
    for (query, expected, note) in CASES {
        let hits = loader.match_message(query);
        let scores: Vec<String> = hits
            .iter()
            .map(|m| format!("{}={:.2}", m.name, m.score))
            .collect();
        let top = hits.first().map(|m| m.score).unwrap_or(0.0);
        let label = if expected.is_empty() { "NEG" } else { "POS" };
        println!("  {label} [{note}] {query:?} -> {scores:?}");
        if expected.is_empty() {
            fp_max = fp_max.max(top);
        } else if let Some(hit) = hits.iter().find(|m| m.name == *expected) {
            pos_min = pos_min.min(hit.score);
        }
    }
    println!(
        "\n分离度: 负样本最高分 {fp_max:.2} < 正样本最低分 {pos_min:.2} → {}",
        if fp_max < pos_min {
            format!("阈值可取 ({fp_max:.2}, {pos_min:.2}) 开区间内任一点，误激活与漏激活同时为 0")
        } else {
            "正负样本分数有重叠，纯调阈值无法同时为 0（需要改打分，不是改阈值）".to_string()
        }
    );

    // 分解：matched_on 指出分数来自哪个字段（name/description/tags/cjk），
    // 与分值一起看就能判断误激活走的是精确词还是模糊子序列。
    println!("\n=== 命中来源（括号内为 matched_on）===");
    let loader = loader_for(tmp.path(), 0.0);
    for (query, _, note) in CASES {
        let hits = loader.match_message(query);
        let parts: Vec<String> = hits
            .iter()
            .map(|m| format!("{}={:.2}({})", m.name, m.score, m.matched_on))
            .collect();
        println!("  [{note}] {query:?} -> {parts:?}");
    }
}

/// 用**真实技能库**（`~/.hermix/skills`）跑观察报告。
///
/// 合成语料（`seed_skills`）验证的是打分器的**性质**，真实库验证的是阈值
/// 在**真实分布**上是否还成立——真实技能的描述长度、语言混用、tag 习惯都
/// 与合成语料不同。没有 ground truth，所以本测试不做硬断言，产出：
/// 1. 全部技能的清单与数量；
/// 2. 一组中性查询（与任何技能都无关的日常请求）在真实库上的激活情况——
///    被激活的就是候选误激活，需要人工判读；
/// 3. 每条激活的分数与命中来源。
///
/// `--ignored --nocapture` 运行。
#[test]
#[ignore = "runs against the real ~/.hermix/skills library; prints a report"]
fn bench_against_real_library() {
    let home = hermix_config::paths::hermix_home();
    let skills_dir = home.join("skills");
    if !skills_dir.exists() {
        eprintln!("SKIP: no real skills library at {}", skills_dir.display());
        return;
    }

    let mut loader = SkillLoader::with_dir(&skills_dir);
    loader.scan();
    let total = loader.orchestrator().get_commands().len();
    println!("\n=== 真实技能库：{} 个技能（{}）===", total, skills_dir.display());
    for (key, info) in loader.orchestrator().get_commands() {
        println!(
            "  {:<28} tags={:?} desc={}",
            info.name,
            info.tags,
            info.description.chars().take(60).collect::<String>()
        );
        let _ = key;
    }

    // 中性查询：日常请求，与任何技能描述都不应有实质关联。被激活的每一
    // 条都是候选误激活，按分数降序人工判读。
    let probes = [
        "帮我把这些照片按日期整理一下",
        "写一个快速排序的实现",
        "explain quantum entanglement simply",
        "the printer is not responding",
        "review this pull request please",
        "how do I configure nginx reverse proxy",
        "帮我写一份周报",
        "translate this paragraph into french",
        "what is the capital of Australia",
        "帮我把这个表格里的重复行删掉",
    ];
    println!("\n=== 中性查询探测（不应激活任何技能）===");
    let mut activations = 0usize;
    for q in probes {
        let hits = loader.match_message(q);
        if hits.is_empty() {
            println!("  [clean] {q:?}");
        } else {
            activations += 1;
            let scores: Vec<String> = hits
                .iter()
                .map(|m| format!("{}={:.2}({})", m.name, m.score, m.matched_on))
                .collect();
            println!("  [FIRED] {q:?} -> {scores:?}");
        }
    }
    println!(
        "\n{activations}/{} 条中性查询发生了激活——每一条都需要人工判读是否真误激活。\
         若误激活成规模，先看命中来源（vector 层的抬高 semantic_min_score_cjk / \
         semantic_min_score，keyword 层的改打分）。",
        probes.len()
    );
}

/// 默认阈值下的硬约束。这两个数字是**当前行为**的快照：修阈值时会红，
/// 那意味着"行为变了"，必须同步更新这里与决策笔记，而不是悄悄放宽。
///
/// 2026-10-20：`semantic_min_score` 0.18 → 0.45（量化依据见 bench 的
/// 「命中来源」段：全部误激活都来自 vector 层，0.19–0.38；keyword/CJK/fuzzy
/// 三路零误激活）。同一次修掉了 `semantic_threshold` 的条件互换——它在
/// min_score ∈ (0.30, 0.45] 时会把向量阈值**降**到 min_score，扫描在 0.35
/// 档捕捉到一次误激活反弹。
#[test]
fn default_threshold_behaviour_is_pinned() {
    let tmp = tempfile::tempdir().unwrap();
    seed_skills(tmp.path());
    let (fp, miss, _) = evaluate(tmp.path(), 0.3);

    assert_eq!(
        fp, 0,
        "默认阈值下负样本零误激活。若非 0，说明又有一条噪声路径越过了阈值——\
         请先跑 bench --nocapture 看它属于哪一层（keyword/cjk/fuzzy/vector），\
         再决定抬阈值还是改打分，不要直接放宽这里。"
    );
    assert_eq!(
        miss, 1,
        "唯一漏激活是纯 CJK 查询：cjk_bigram_hit 只与含 CJK 的 tag/name 比对，\
         而语料里三个技能的 tag 都是英文。这是已知设计限制（中文释义召回），\
         不是阈值问题——修它需要 CJK→英文触发词映射，属另一项工作。"
    );
}

/// 向量阈值在**任何** min_score 下都不能低于自身默认值。上面那个
/// 条件互换的回归哨兵：0.35 曾是死角（向量阈值被降到 0.35 < 0.45）。
#[test]
fn operator_strictness_never_lowers_the_vector_bar() {
    let tmp = tempfile::tempdir().unwrap();
    seed_skills(tmp.path());
    for min in [0.0f32, 0.2, 0.3, 0.35, 0.4, 0.45, 0.5, 0.99] {
        let (fp, _, _) = evaluate(tmp.path(), min);
        assert_eq!(
            fp, 0,
            "min_score={min} 出现误激活：operator 严格度不应削弱向量阈值"
        );
    }
}
