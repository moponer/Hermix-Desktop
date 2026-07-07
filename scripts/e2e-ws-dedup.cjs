// E2E WS dedup test v2 — more verbose, checks WS state, listens for ALL events.

const SESSION_ID = `e2e-dedup-${Date.now()}`;
const URL = `ws://127.0.0.1:3001/v1/ws-stream/${SESSION_ID}`;
const TEXT = `hello e2e dedup test ${Date.now()}`;

(async () => {
  console.log(`Connecting to ${URL}`);
  const ws = new WebSocket(URL);
  let allEvents = [];
  let senderResolve;
  const senderPromise = new Promise((r) => (senderResolve = r));

  ws.addEventListener("open", () => {
    console.log(`WS open. readyState=${ws.readyState}`);
  });

  ws.addEventListener("message", (e) => {
    let env;
    try {
      env = JSON.parse(e.data);
    } catch {
      env = { _raw: e.data };
    }
    allEvents.push(env);
    const tag = env?.event?.type || "?";
    const fr = env?.event?.finish_reason;
    console.log(`  << [${tag}${fr ? `/${fr}` : ""}]`, JSON.stringify(env));
    if (tag === "done" || tag === "error") {
      senderResolve(env);
    }
  });

  ws.addEventListener("close", (e) => {
    console.log(`WS close. code=${e.code} reason=${e.reason}`);
  });
  ws.addEventListener("error", (e) => {
    console.log(`WS error:`, e.message || e);
  });

  // wait for Connected
  await new Promise((r) => setTimeout(r, 300));
  console.log(`\n[1st send] text="${TEXT}"`);
  ws.send(JSON.stringify({ type: "send", text: TEXT }));

  // wait for 1st send to settle
  await senderPromise;
  await new Promise((r) => setTimeout(r, 800));
  console.log(`\n[2nd send] text="${TEXT}" (same as 1st)`);
  console.log(`  WS state before 2nd send: readyState=${ws.readyState}`);

  // Reset resolver
  senderResolve = null;
  let p2done;
  const p2 = new Promise((r) => (p2done = r));
  const newHandler = (e) => {
    let env;
    try {
      env = JSON.parse(e.data);
    } catch {
      env = { _raw: e.data };
    }
    allEvents.push(env);
    const tag = env?.event?.type || "?";
    const fr = env?.event?.finish_reason;
    console.log(`  << [p2 ${tag}${fr ? `/${fr}` : ""}]`, JSON.stringify(env));
    if (tag === "done" || tag === "error") {
      p2done(env);
    }
  };
  ws.addEventListener("message", newHandler);
  ws.send(JSON.stringify({ type: "send", text: TEXT }));

  const result = await Promise.race([
    p2,
    new Promise((r) => setTimeout(() => r({ timeout: true }), 8000)),
  ]);
  ws.removeEventListener("message", newHandler);
  ws.close();

  console.log("\n=== VERDICT ===");
  if (result.timeout) {
    console.log("FAIL — 2nd send timed out (no response in 8s)");
    console.log("Total events received:", allEvents.length);
  } else if (result?.event?.finish_reason === "deduped") {
    console.log("PASS — 2nd send was deduped by the server.");
  } else {
    console.log("FAIL — 2nd send got:", JSON.stringify(result));
  }
  process.exit(result?.event?.finish_reason === "deduped" ? 0 : 1);
})().catch((err) => {
  console.error("TEST CRASHED:", err);
  process.exit(2);
});
