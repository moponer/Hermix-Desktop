"""全新安装场景的后端契约实测。

模拟「安装包装到另一台电脑、没有任何模型配置」：用临时目录做 HERMIX_HOME，
并清掉从宿主机继承的所有 *_API_KEY，然后起真实 hermix.exe --stdio，
走 stdio 帧把前端依赖的接口逐条打一遍。

场景 A：零 key —— 不该写 config.yaml，模型信息应为「未配置」
场景 B：只有 DEEPSEEK_API_KEY（写在 .env）—— 启动即应自动路由到
        deepseek:deepseek-chat 并落盘
场景 C：PUT /api/model/list 必须把 config.model 同步成 default_model
        （前端 setDefault 依赖这条契约，删掉冗余回写的前提）

用法：python scripts/verify-fresh-model-config.py <hermix.exe 路径>
"""

import collections
import json
import os
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time

EXE = sys.argv[1] if len(sys.argv) > 1 else r"<repo>\target\debug\hermix.exe"

results = []


def check(label, ok, detail=""):
    results.append((label, ok, detail))
    print(f"  [{'PASS' if ok else 'FAIL'}] {label}" + (f"  -> {detail}" if detail else ""))


class Sidecar:
    """stdio JSON Lines 客户端：一帧一行，按 request_id 配对响应。"""

    def __init__(self, home, pre_env=None):
        self.home = home
        env = dict(os.environ)
        env["HERMIX_HOME"] = home
        env.pop("HERMIX_PORTABLE_DATA_DIR", None)
        # 全新机器上没有这些 key —— 必须清掉宿主机继承值，否则测得不是"零配置"
        for k in list(env):
            if k.endswith("_API_KEY") or k in ("GITHUB_COPILOT_TOKEN", "MINIMAX_API_KEY"):
                env.pop(k)
        env.update(pre_env or {})
        self.proc = subprocess.Popen(
            [EXE, "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            cwd=home,
        )
        self.q = queue.Queue()
        self.rid = 0
        self.stderr_tail = collections.deque(maxlen=40)
        threading.Thread(target=self._pump, daemon=True).start()
        # stderr 必须持续排空：stdio 模式下日志全走 stderr，启动瞬间会灌
        # 十几 KB（seeded builtin skills 等）。不读的话匿名管道 64KB 写满后
        # sidecar 会阻塞在写日志上，永远进不了 stdio 循环 —— 表现就是
        # "发帧没回执"，很容易误判成协议不对。
        threading.Thread(target=self._drain_stderr, daemon=True).start()
        self._wait_ready()

    def _pump(self):
        for line in self.proc.stdout:
            self.q.put(line.decode("utf-8", "replace").strip())

    def _drain_stderr(self):
        for line in self.proc.stderr:
            self.stderr_tail.append(line.decode("utf-8", "replace").rstrip())

    def _wait_ready(self, attempts=40):
        """等 sidecar 进入 stdio 循环。

        实测算命点：启动阶段（seeded builtin skills → task_tracker 注册）约需
        10s（debug 版），**这期间写进 stdin 的帧会被丢弃**（stdio 循环尚未
        开始读→对端提前 close 时只看到 EOF）。所以这里反复发探测帧直到拿到
        回执，不能"发一帧然后睡等"。
        """
        for _ in range(attempts):
            try:
                st, _ = self.call("GET", "/health", timeout=3)
                if st == 200:
                    return
            except (TimeoutError, AssertionError):
                pass
            time.sleep(1)
        print("  sidecar stderr 末尾：")
        for line in self.stderr_tail:
            print(f"    {line}")
        raise TimeoutError("sidecar 未在预期时间内就绪")

    def call(self, method, path, body=None, timeout=30):
        self.rid += 1
        rid = str(self.rid)
        inner = {"method": method, "path": path, "request_id": rid}
        if body is not None:
            inner["body"] = json.dumps(body)
        frame = {"channel": "http", "payload": json.dumps(inner)}
        self.proc.stdin.write((json.dumps(frame) + "\n").encode("utf-8"))
        self.proc.stdin.flush()
        while True:
            try:
                line = self.q.get(timeout=timeout)
            except queue.Empty:
                raise TimeoutError(f"no reply for {method} {path}")
            if not line:
                continue
            try:
                env = json.loads(line)
            except json.JSONDecodeError:
                # stdout 只应有协议帧；出现别的行说明日志污染了协议通道
                raise AssertionError(f"stdout 出现非协议输出: {line[:200]}")
            if env.get("request_id") != rid:
                continue
            payload = env.get("payload")
            payload = json.loads(payload) if isinstance(payload, str) else payload
            body_txt = payload.get("body") or ""
            if payload.get("enc") == "base64":
                import base64

                body_txt = base64.b64decode(body_txt).decode("utf-8", "replace")
            try:
                parsed = json.loads(body_txt) if body_txt else None
            except json.JSONDecodeError:
                parsed = body_txt
            return payload.get("status"), parsed

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def read_yaml(path):
    with open(path, "r", encoding="utf-8") as f:
        return f.read()


def scenario_a():
    print("\n场景 A｜全新电脑，零 API Key")
    home = tempfile.mkdtemp(prefix="hermix-freshA-")
    s = Sidecar(home)
    try:
        st, _ = s.call("GET", "/health")
        check("sidecar 启动 + /health 可用", st == 200, f"status={st}")

        st, info = s.call("GET", "/api/model/info")
        check(
            "未配置时 /api/model/info 明确报「无模型」而非静默兜底 gpt-4o",
            st == 200 and info["model"] == "(no model configured)" and info["provider"] == "auto",
            f"{info.get('provider')}:{info.get('model')}",
        )

        st, lst = s.call("GET", "/api/model/list")
        check(
            "未配置时 /api/model/list 为空",
            st == 200 and lst["models"] == [] and lst["default_model"] is None,
            json.dumps(lst, ensure_ascii=False),
        )

        st, meta = s.call("GET", "/api/providers/meta")
        by_id = {m["id"]: m for m in meta} if st == 200 else {}
        check(
            "providers/meta 下发权威默认模型（openai→gpt-4o）",
            by_id.get("openai", {}).get("default_model") == "gpt-4o",
            json.dumps(by_id.get("openai"), ensure_ascii=False),
        )
        check(
            "未收录厂商的占位符 default 不下发成模型名",
            by_id.get("baichuan", {}).get("default_model") is None
            and all(m.get("default_model") != "default" for m in meta),
            f"baichuan.default_model={by_id.get('baichuan', {}).get('default_model')!r}",
        )

        st, cfg = s.call("GET", "/api/config")
        check(
            "未配置时 config.model 为空且 llm_providers 为空",
            st == 200 and not (cfg or {}).get("model") and not (cfg or {}).get("llm_providers"),
            f"model={cfg.get('model')!r} providers={list((cfg.get('llm_providers') or {}).keys())}",
        )

        cfg_yaml = os.path.join(home, "config.yaml")
        check(
            "零 key 时不为用户生成半成品 config.yaml",
            not os.path.exists(cfg_yaml),
            cfg_yaml if os.path.exists(cfg_yaml) else "(未创建，符合预期)",
        )
    finally:
        s.close()
        shutil.rmtree(home, ignore_errors=True)


def scenario_b():
    print("\n场景 B｜全新电脑，只有 DEEPSEEK_API_KEY（写在 .env）")
    home = tempfile.mkdtemp(prefix="hermix-freshB-")
    with open(os.path.join(home, ".env"), "w", encoding="utf-8") as f:
        f.write('DEEPSEEK_API_KEY="sk-fake-for-verification"\n')
    s = Sidecar(home)
    try:
        st, info = s.call("GET", "/api/model/info")
        check(
            "启动即自动路由到有 key 的厂商（deepseek:deepseek-chat）",
            st == 200 and info["provider"] == "deepseek" and info["model"] == "deepseek-chat",
            f"{info.get('provider')}:{info.get('model')}",
        )

        cfg_yaml = os.path.join(home, "config.yaml")
        check("自动路由结果落盘到 config.yaml", os.path.exists(cfg_yaml))
        if os.path.exists(cfg_yaml):
            text = read_yaml(cfg_yaml)
            check("config.yaml 里 model 为 deepseek:deepseek-chat", "deepseek:deepseek-chat" in text)

        st, info2 = s.call("GET", "/api/model/info")
        check("provider 与实际 model 一致（无 UI/后端漂移）", info2.get("provider") == "deepseek")

        st, ac = s.call("POST", "/api/config/auto-correct")
        check(
            "配置已正确时 auto-correct 幂等（不重复改）",
            st == 200 and (ac or {}).get("report") is None,
            (ac or {}).get("message", ""),
        )
    finally:
        s.close()
        shutil.rmtree(home, ignore_errors=True)


def scenario_c():
    print("\n场景 C｜新装用户在前端填完 key + 选默认模型的等价调用")
    home = tempfile.mkdtemp(prefix="hermix-freshC-")
    s = Sidecar(home)
    try:
        # 1. 前端「供应商」页填 key → PUT /api/env
        st, ok = s.call("PUT", "/api/env", {"key": "DEEPSEEK_API_KEY", "value": "sk-fake-for-verification"})
        check("PUT /api/env 写入 key", st == 200 and ok.get("ok") is True, json.dumps(ok, ensure_ascii=False))
        env_file = os.path.join(home, ".env")
        check(
            ".env 落盘（重启后能恢复）",
            os.path.exists(env_file) and "DEEPSEEK_API_KEY" in read_yaml(env_file),
            read_yaml(env_file).strip() if os.path.exists(env_file) else "(缺失)",
        )
        st, revealed = s.call("POST", "/api/env/reveal", {"key": "DEEPSEEK_API_KEY"})
        check("POST /api/env/reveal 能取回 key（前端直连厂商 /models 用）", (revealed or {}).get("value") == "sk-fake-for-verification")

        # 2. 前端选默认模型 → writeModelConfigAndNotify → PUT /api/model/list
        st, ok = s.call(
            "PUT",
            "/api/model/list",
            {
                "models": [{"provider_id": "deepseek", "model_id": "deepseek-chat"}],
                "default_model": "deepseek:deepseek-chat",
            },
        )
        check("PUT /api/model/list 接受选择器 + 默认模型", st == 200 and ok.get("ok") is True)

        st, lst = s.call("GET", "/api/model/list")
        check(
            "回读选择器（重启后 UI 不丢模型）",
            st == 200
            and lst["default_model"] == "deepseek:deepseek-chat"
            and lst["models"] == [{"provider_id": "deepseek", "model_id": "deepseek-chat"}],
            json.dumps(lst, ensure_ascii=False),
        )

        # 3. 关键契约：PUT /api/model/list 必须同步 config.model，否则聊天跑的不是 UI 选的模型
        st, cfg = s.call("GET", "/api/config")
        check(
            "PUT /api/model/list 同步了 config.model（聊天实际用这个）",
            (cfg or {}).get("model") == "deepseek:deepseek-chat",
            f"model={cfg.get('model')!r}",
        )

        st, info = s.call("GET", "/api/model/info")
        check(
            "model/info 反映用户所选默认模型",
            info.get("provider") == "deepseek" and info.get("model") == "deepseek-chat",
            f"{info.get('provider')}:{info.get('model')}",
        )

        # 4. 前端在落盘之后调 auto-correct：不应把用户选的模型改掉
        st, ac = s.call("POST", "/api/config/auto-correct")
        st2, cfg2 = s.call("GET", "/api/config")
        check(
            "auto-correct 不会改掉用户刚选的默认模型",
            (cfg2 or {}).get("model") == "deepseek:deepseek-chat",
            f"after auto-correct: model={cfg2.get('model')!r}, report={(ac or {}).get('report')!r}",
        )

        # 5. 只有 deepseek 有 key 时，providers 列表不应把别的厂商算进来
        st, lst2 = s.call("GET", "/api/model/list")
        check(
            "未配 key 的厂商条目被过滤（防孤儿模型）",
            all(m["provider_id"] == "deepseek" for m in lst2["models"]),
            json.dumps(lst2["models"], ensure_ascii=False),
        )
    finally:
        s.close()
        shutil.rmtree(home, ignore_errors=True)


if __name__ == "__main__":
    if not os.path.exists(EXE):
        print(f"hermix.exe 不存在: {EXE}")
        sys.exit(2)
    print(f"被验证的二进制: {EXE}")
    scenario_a()
    scenario_b()
    scenario_c()

    failed = [r for r in results if not r[1]]
    print(f"\n{'=' * 60}\n合计 {len(results)} 项，通过 {len(results) - len(failed)}，失败 {len(failed)}")
    for label, _, detail in failed:
        print(f"  FAIL: {label}  -> {detail}")
    sys.exit(1 if failed else 0)
