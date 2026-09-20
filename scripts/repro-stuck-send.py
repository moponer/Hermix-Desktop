"""复现「发送你好后一直显示正在回复中」。

场景 R1（干净）：建会话 → 发「你好」→ 收流式事件直到 Done/Error（90s 超时）。
场景 R2（写盘压力）：同 R1，但发送后每 1.5s POST /api/config/auto-correct
（模拟前端 autoCorrectConfig 循环），验证 save_config_yaml 非原子写
（loader.rs std::fs::write = truncate+write）是否会让 agent loop 启动期
load_config 读到空/半截 YAML，导致 providers 丢失 / 链路挂起。

用法：python scripts/repro-stuck-send.py [hermix.exe 路径]
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

EXE = sys.argv[1] if len(sys.argv) > 1 else r"<repo>\target\debug\hermix.exe"
REAL_HOME = os.path.expanduser("~/.hermix")
SEND_TEXT = "你好"
STREAM_WAIT_SECS = 90


class Sidecar:
    """stdio JSON Lines 客户端：REST request-response + stream 通道事件收集。"""

    def __init__(self, home):
        self.home = home
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
        self.stream_events = queue.Queue()
        self.rid = 0
        self.stderr_tail = collections_deque()
        threading.Thread(target=self._pump, daemon=True).start()
        threading.Thread(target=self._drain_stderr, daemon=True).start()
        self._wait_ready()

    def _pump(self):
        for line in self.proc.stdout:
            text = line.decode("utf-8", "replace").strip()
            try:
                env = json.loads(text)
            except json.JSONDecodeError:
                continue
            ch = env.get("channel", "")
            if ch.startswith("stream:"):
                self.stream_events.put(env)
            else:
                self.q.put(env)

    def _drain_stderr(self):
        for line in self.proc.stderr:
            self.stderr_tail.append(line.decode("utf-8", "replace").rstrip())

    def _wait_ready(self, attempts=60):
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
        while True:
            try:
                line = self.q.get(timeout=timeout)
            except queue.Empty:
                raise TimeoutError(f"no reply for {method} {path}")
            if not line:
                continue
            if line.get("request_id") != rid:
                continue
            payload = line.get("payload")
            payload = json.loads(payload) if isinstance(payload, str) else payload
            body_txt = payload.get("body") or ""
            try:
                parsed = json.loads(body_txt) if body_txt else None
            except json.JSONDecodeError:
                parsed = body_txt
            return payload.get("status"), parsed

    def send_stream(self, session_id, payload):
        frame = {"channel": f"stream:{session_id}", "payload": json.dumps(payload)}
        self.proc.stdin.write((json.dumps(frame) + "\n").encode("utf-8"))
        self.proc.stdin.flush()

    def collect_stream(self, deadline):
        events = []
        while time.time() < deadline:
            try:
                env = self.stream_events.get(timeout=0.5)
            except queue.Empty:
                if self.proc.poll() is not None:
                    events.append(("PROC_EXITED", self.proc.returncode))
                    break
                continue
            payload = env.get("payload")
            payload = json.loads(payload) if isinstance(payload, str) else payload
            ev = payload.get("event") or payload
            etype = ev.get("type") if isinstance(ev, dict) else str(ev)
            events.append((etype, ev))
            if etype in ("done", "error"):
                break
        return events

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def collections_deque():
    return _Tail()


class _Tail(list):
    def append(self, x):
        super().append(x)
        if len(self) > 40:
            del self[0]


def make_home():
    home = tempfile.mkdtemp(prefix="hermix-repro-")
    # 拷主人的 provider 配置（含 minimax key），数据库全新
    shutil.copy(os.path.join(REAL_HOME, "config.yaml"), os.path.join(home, "config.yaml"))
    env_path = os.path.join(REAL_HOME, ".env")
    if os.path.exists(env_path):
        shutil.copy(env_path, os.path.join(home, ".env"))
    return home


def create_session(sc):
    st, body = sc.call("POST", "/v1/sessions", body={"title": "repro"}, timeout=20)
    print(f"  POST /v1/sessions -> {st}: {json.dumps(body, ensure_ascii=False)[:200]}")
    if isinstance(body, dict):
        for key in ("id", "session_id"):
            if body.get(key):
                return body[key]
        sessions = body.get("sessions")
        if sessions:
            return sessions[0].get("id")
    raise RuntimeError("无法获取 session id")


def run_scenario(name, with_pressure):
    print(f"\n=== 场景 {name}（auto-correct 压力: {with_pressure}）===")
    home = make_home()
    sc = Sidecar(home)
    try:
        sid = create_session(sc)
        print(f"  session = {sid}")

        stop = threading.Event()

        def pressure():
            # PUT /api/model/list：update_model_list 无条件 save_config_yaml
            # （truncate+write 非原子）+ set_config —— 模拟前端配置模型的真实写盘压力。
            body = {
                "models": [
                    {"provider_id": "minimax", "model_id": "MiniMax-M3"},
                    {"provider_id": "deepseek", "model_id": "deepseek-chat"},
                ],
                "default_model": "minimax:MiniMax-M3",
            }
            n = 0
            while not stop.is_set():
                n += 1
                try:
                    sc.call("PUT", "/api/model/list", body=body, timeout=5)
                except Exception:
                    pass
                time.sleep(1.5)

        if with_pressure:
            # 预热：先打 5 轮 PUT（模拟主人配置模型期间的连续写盘+set_config），
            # 然后保持每 1.5s 一轮直到场景结束 —— 主人卡住时正是
            # 「最后一轮 set_config (:43.504) 与发送同秒」。
            body = {
                "models": [
                    {"provider_id": "minimax", "model_id": "MiniMax-M3"},
                    {"provider_id": "deepseek", "model_id": "deepseek-chat"},
                ],
                "default_model": "minimax:MiniMax-M3",
            }
            for _ in range(5):
                try:
                    sc.call("PUT", "/api/model/list", body=body, timeout=5)
                except Exception:
                    pass
                time.sleep(0.3)
            threading.Thread(target=pressure, daemon=True).start()

        t0 = time.time()
        sc.send_stream(sid, {"text": SEND_TEXT, "request_id": f"repro-{int(t0)}", "model": "minimax:MiniMax-M3"})
        print(f"  已发送『{SEND_TEXT}』(model=minimax:MiniMax-M3)，收流 {STREAM_WAIT_SECS}s ...")

        events = sc.collect_stream(time.time() + STREAM_WAIT_SECS)
        stop.set()
        elapsed = time.time() - t0

        types = [e[0] for e in events]
        print(f"  收到 {len(events)} 个事件，耗时 {elapsed:.1f}s")
        print(f"  事件序列: {types[:30]}")
        for etype, ev in events:
            if etype in ("done", "error", "PROC_EXITED"):
                print(f"  终态 {etype}: {json.dumps(ev, ensure_ascii=False)[:400]}")
                break
        else:
            print(f"  !! {STREAM_WAIT_SECS}s 内未收到 done/error —— 复现「正在回复中」卡住")
            if with_pressure:
                print("  stderr 末尾：")
                for line in sc.stderr_tail[-15:]:
                    print(f"    {line}")
            # 复现成功后查消息落库状态
        check_db(home, sid)
    finally:
        sc.close()
        if not with_pressure or "OK" not in str(events if 'events' in dir() else ''):
            pass
    return events


def check_db(home, sid):
    import sqlite3
    db_path = os.path.join(home, "data", "sessions.db")
    if not os.path.exists(db_path):
        print(f"  [DB] {db_path} 不存在")
        return
    try:
        db = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
        n = db.execute("SELECT COUNT(*) FROM messages WHERE session_id=?", (sid,)).fetchone()[0]
        print(f"  [DB] session {sid[:8]} 消息数 = {n}")
        db.close()
    except Exception as e:
        print(f"  [DB] 查询失败: {e}")


if __name__ == "__main__":
    run_scenario("R1 干净", with_pressure=False)
    run_scenario("R2 写盘压力", with_pressure=True)
