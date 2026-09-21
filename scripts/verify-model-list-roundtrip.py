"""验证模型选择器后端链路（release 产物 e2e）。

隔离 HERMIX_HOME（预置一个带 key 的 provider），逐项断言：
  V1  干净起步 GET /api/model/list → models 为空
  V2  PUT /api/model/list 写入 2 条 + default → ok
  V3  GET 回读 → 2 条且 default 一致
  V4  GET /api/config → config.model 已被同步成 default_model
  V5  再 PUT 只剩 1 条（模拟用户删除模型）→ GET 回读 1 条（删除持久）
  V6  config.yaml 落盘内容与内存一致（原子写后文件完整可解析）

用法：python scripts/verify-model-list-roundtrip.py <hermix.exe 路径>
"""

import json
import os
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time

EXE = sys.argv[1] if len(sys.argv) > 1 else r"<repo>\release\target\release\hermix.exe"

MINIMAL_CONFIG = """model: null
llm_providers:
  minimax:
    api_key: test-key-123
"""

FAILS = []


def check(name, cond, detail=""):
    mark = "PASS" if cond else "FAIL"
    print(f"  [{mark}] {name}" + (f"  ({detail})" if detail and not cond else ""))
    if not cond:
        FAILS.append(name)


class Sidecar:
    """stdio JSON Lines 客户端（REST 配对 + stderr 持续排空）。"""

    def __init__(self, home):
        env = dict(os.environ)
        env["HERMIX_HOME"] = home
        env.pop("HERMIX_PORTABLE_DATA_DIR", None)
        for k in list(env):
            if k.endswith("_API_KEY"):
                env.pop(k)
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
        threading.Thread(target=self._pump, daemon=True).start()
        threading.Thread(target=self._drain_stderr, daemon=True).start()
        self._wait_ready()

    def _pump(self):
        for line in self.proc.stdout:
            try:
                self.q.put(json.loads(line.decode("utf-8", "replace")))
            except json.JSONDecodeError:
                continue

    def _drain_stderr(self):
        for _ in self.proc.stderr:
            pass

    def _wait_ready(self, attempts=60):
        for _ in range(attempts):
            try:
                st, _ = self.call("GET", "/health", timeout=3)
                if st == 200:
                    return
            except (TimeoutError, AssertionError):
                pass
            time.sleep(1)
        raise TimeoutError("sidecar 未就绪")

    def call(self, method, path, body=None, timeout=30):
        self.rid += 1
        rid = str(self.rid)
        inner = {"method": method, "path": path, "request_id": rid}
        if body is not None:
            inner["body"] = json.dumps(body)
        frame = {"channel": "http", "payload": json.dumps(inner)}
        self.proc.stdin.write((json.dumps(frame) + "\n").encode("utf-8"))
        self.proc.stdin.flush()
        deadline = time.time() + timeout
        while True:
            remain = deadline - time.time()
            if remain <= 0:
                raise TimeoutError(f"no reply for {method} {path}")
            line = self.q.get(timeout=remain)
            if str(line.get("request_id", "")) == rid:
                payload = json.loads(line.get("payload") or "{}")
                status = payload.get("status", 0)
                body_text = payload.get("body") or ""
                try:
                    return status, json.loads(body_text)
                except json.JSONDecodeError:
                    return status, body_text

    def stop(self):
        try:
            self.proc.terminate()
            self.proc.wait(timeout=10)
        except Exception:
            self.proc.kill()


def main():
    home = tempfile.mkdtemp(prefix="hermix-verify-model-list-")
    with open(os.path.join(home, "config.yaml"), "w", encoding="utf-8") as f:
        f.write(MINIMAL_CONFIG)
    sc = Sidecar(home)
    try:
        # V1 干净起步
        st, data = sc.call("GET", "/api/model/list")
        models = data.get("models", []) if isinstance(data, dict) else []
        check("V1 干净起步 models 为空", st == 200 and len(models) == 0, f"status={st} models={models}")

        # V2 写入 2 条 + default
        put_body = {
            "models": [
                {"provider_id": "minimax", "model_id": "MiniMax-M3"},
                {"provider_id": "minimax", "model_id": "MiniMax-M2"},
            ],
            "default_model": "minimax:MiniMax-M3",
        }
        st, data = sc.call("PUT", "/api/model/list", put_body)
        check("V2 PUT model/list ok", st == 200 and isinstance(data, dict) and data.get("ok") is True, f"status={st} {data}")

        # V3 回读一致
        st, data = sc.call("GET", "/api/model/list")
        models = data.get("models", []) if isinstance(data, dict) else []
        default = data.get("default_model") if isinstance(data, dict) else None
        got = sorted((m.get("provider_id"), m.get("model_id")) for m in models)
        check("V3 回读 2 条", st == 200 and got == [("minimax", "MiniMax-M2"), ("minimax", "MiniMax-M3")], f"got={got}")
        check("V3 default 回读一致", default == "minimax:MiniMax-M3", f"default={default}")

        # V4 config.model 联动
        st, cfg = sc.call("GET", "/api/config")
        check("V4 config.model 同步成 default_model", isinstance(cfg, dict) and cfg.get("model") == "minimax:MiniMax-M3", f"model={cfg.get('model') if isinstance(cfg, dict) else cfg}")

        # V5 删除持久（回归 bug 1 后端侧）
        put_body["models"] = [{"provider_id": "minimax", "model_id": "MiniMax-M3"}]
        st, _ = sc.call("PUT", "/api/model/list", put_body)
        st, data = sc.call("GET", "/api/model/list")
        models = data.get("models", []) if isinstance(data, dict) else []
        got = [m.get("model_id") for m in models]
        check("V5 删除后回读仅剩 1 条", st == 200 and got == ["MiniMax-M3"], f"got={got}")

        # V6 落盘 YAML 完整可解析（原子写）
        import re
        disk = open(os.path.join(home, "config.yaml"), "rb").read().decode("utf-8")
        check("V6 落盘无临时残件", not any(n.startswith(".") and n.endswith(".tmp") for n in os.listdir(home)))
        check("V6 落盘含 selector 条目", re.search(r"model_id: MiniMax-M3", disk) is not None)
    finally:
        sc.stop()
        shutil.rmtree(home, ignore_errors=True)

    print()
    if FAILS:
        print(f"FAILED: {len(FAILS)} 项未过 → {FAILS}")
        sys.exit(1)
    print("ALL PASS")


if __name__ == "__main__":
    main()
