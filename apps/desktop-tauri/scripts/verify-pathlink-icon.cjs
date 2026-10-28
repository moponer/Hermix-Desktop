/**
 * 行内路径链接（.md-path-link）图标垂直对齐实测。
 *
 * 存在的理由：图标"被挤到上面一层"是肉眼判断，tsc / vitest 全绿说明不了
 * 任何事。这里绕过整个 App，把 MarkdownBody 单独挂进真实 dev server 的
 * 样式环境里渲染，量出 link / icon / 文字基线的真实像素关系，并留截图。
 *
 * 跑法：cd apps/desktop-tauri && node scripts/verify-pathlink-icon.cjs
 * 前置：dev server 已在 127.0.0.1:1420 运行（npm run dev）。
 */

const fs = require("fs");
const path = require("path");
const { chromium } = require("playwright");

const DEV_URL = process.env.PV_DEV_URL || "http://127.0.0.1:1420";
const OUT_DIR = path.resolve(__dirname, "..", "..", "..", "outputs", "pathlink-icon");

// 复刻主人截图里那段 LLM 输出：行内 code 的 IP/端口 + 正文里的裸文件名。
const MD = [
  "列格式和我猜的不一样，实际是 `length N: src.ip.port > dst.ip.port: PROTO, length N`。重写解析器，并直接做端口→进程映射。",
  "",
  "铁证。99.9% 的流量走一个 UDP 流：`101.85.130.67:62849` → 本地 `54408`。而 54408 正是 GameViewer 的 UDP 端口。",
  "",
  "## 结论",
  "",
  "那 240 KB/s 是网易 UU 远程（GameViewer）的屏幕串流。",
  "",
  "抓包 20 秒，42716 个包，入站 36.33 MB，其中 `37.16 MB（99.9%）` 挤在一条流上：",
  "",
  "`101.85.130.67:62849` → `192.168.1.4:54408` UDP",
  "",
  "本地端口 54408 归属 `GameViewer.exe` PID 5456。`101.85.130.67` 是电信 IP（tracert 第 2 跳 `100.80.64.1` 是运营商 CGN 网关）。",
].join("\n");

(async () => {
  fs.mkdirSync(OUT_DIR, { recursive: true });

  const browser = await chromium
    .launch({ channel: "chrome", args: ["--no-proxy-server"] })
    .catch(() => chromium.launch({ channel: "msedge", args: ["--no-proxy-server"] }));
  const page = await browser.newPage({ viewport: { width: 1200, height: 900 } });

  await page.addInitScript(() => {
    window.__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: "main" },
        currentWebview: { label: "main", windowLabel: "main" },
      },
      transformCallback: () => 1,
      invoke: () => Promise.resolve("{}"),
      plugins: {},
      convertFileSrc: (p) => String(p),
    };
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  });

  // 用 domcontentloaded 而非 load：dev server 冷启动/首次依赖预构建时，
  // 个别资源（字体、动态 import 链）可能长时间不触发 load 事件，导致
  // goto 30s 超时、守卫假红。本探针只依赖 DOM 就绪 + 后续动态 import。
  await page.goto(`${DEV_URL}/`, { waitUntil: "domcontentloaded", timeout: 60000 });

  // 必须等 CSS 真正注入再测量。dev server 冷启动/首次编译时，globals.css 的
  // 11 个 @import + Tailwind 需要数秒才注入完；若提前测量，`.md-path-link`
  // 会退回 button 的 UA 值 `inline-block`、图标因 preflight 变块级 ——
  // 表现与真实 bug 完全一致，守卫会**假红**。这里等"含有 .md-path-link
  // 规则的样式表已出现"（之后测的仍是层叠结果，不是加载本身）。
  await page.waitForFunction(
    () => {
      for (const s of Array.from(document.styleSheets)) {
        let rules;
        try {
          rules = s.cssRules;
        } catch {
          continue;
        }
        for (const r of Array.from(rules)) {
          if (r.selectorText && r.selectorText.includes(".md-path-link")) return true;
        }
      }
      return false;
    },
    null,
    { timeout: 60000 },
  );
  await page.waitForTimeout(500);

  const probe = await page.evaluate(async (md) => {
    const reactMod = await import("/node_modules/.vite/deps/react.js");
    const React = reactMod.default ?? reactMod;
    const domMod = await import("/node_modules/.vite/deps/react-dom_client.js");
    const ReactDOM = domMod.default ?? domMod;
    const body = await import("/src/components/MarkdownBody.tsx");

    const host = document.createElement("div");
    host.id = "pathlink-probe";
    // 贴近真实气泡：正文 14px、容器宽约 820px。
    host.style.cssText = "width:820px;margin:40px auto;padding:0 24px;";
    document.body.appendChild(host);
    const root = ReactDOM.createRoot(host);
    root.render(React.createElement(body.MarkdownBody, { content: md }));
    await new Promise((r) => setTimeout(r, 900));

    const links = Array.from(host.querySelectorAll(".md-path-link"));
    const rows = links.map((el) => {
      const cs = getComputedStyle(el);
      const svg = el.querySelector("svg");
      const elr = el.getBoundingClientRect();
      const iconR = svg ? svg.getBoundingClientRect() : null;
      const nameSpan = el.querySelector("span");
      const nameR = nameSpan ? nameSpan.getBoundingClientRect() : null;
      // 跨行判据：inline 元素若跨行，getClientRects 会返回多段
      const rects = Array.from(el.getClientRects()).map((r) => `${r.width.toFixed(1)}x${r.height.toFixed(1)}@${r.top.toFixed(1)}`);
      // 这一行（父元素）里的文字基线：插零高 inline-block 探针
      const holder = el.parentElement;
      const probeSpan = document.createElement("span");
      probeSpan.style.cssText = "display:inline-block;width:0;height:0;";
      // 必须插在 link 之前：若追加到段落末尾（appendChild），多行段落里探针会落到
      // 段落最后一行，量到的 baseline 不是 link 所在行 —— 会凭空多出一个行高的负偏移。
      holder.insertBefore(probeSpan, el);
      const baselineY = probeSpan.getBoundingClientRect().bottom;
      probeSpan.remove();
      // 同一行里 link 之外的纯文本行盒
      return {
        text: el.textContent,
        inInlineCode: !!el.closest("code"),
        display: cs.display,
        verticalAlign: cs.verticalAlign,
        alignItems: cs.alignItems,
        gap: cs.gap,
        whiteSpace: cs.whiteSpace,
        fontFamily: cs.fontFamily.split(",")[0],
        fontSize: cs.fontSize,
        color: cs.color,
        linkTop: +elr.top.toFixed(2),
        linkBottom: +elr.bottom.toFixed(2),
        linkHeight: +elr.height.toFixed(2),
        iconTopRelBaseline: iconR ? +(iconR.top - baselineY).toFixed(2) : null,
        iconBottomRelBaseline: iconR ? +(iconR.bottom - baselineY).toFixed(2) : null,
        iconSize: iconR ? `${iconR.width.toFixed(1)}x${iconR.height.toFixed(1)}` : null,
        iconTop: iconR ? +iconR.top.toFixed(2) : null,
        nameSpanTop: nameR ? +nameR.top.toFixed(2) : null,
        nameSpanHeight: nameR ? +nameR.height.toFixed(2) : null,
        // 与基线无关的同行判据：图标顶 vs 文件名 span 顶
        sameRow: iconR && nameR ? Math.abs(iconR.top - nameR.top) < 6 : null,
        iconVsNameTop: iconR && nameR ? +(iconR.top - nameR.top).toFixed(2) : null,
        clientRects: rects,
        baselineY: +baselineY.toFixed(2),
      };
    });

    const inlineCodes = Array.from(host.querySelectorAll(".md-inline-code")).map((c) => ({
      text: c.textContent,
      display: getComputedStyle(c).display,
      hasLinkInside: !!c.querySelector(".md-path-link"),
      html: c.innerHTML.slice(0, 200),
    }));

    // 谁在改写 .md-path-link：穷举所有样式表里命中该类的规则（含来源）
    const matchedRules = [];    const walk = (list, href) => {
      for (const r of Array.from(list)) {
        // 注意：Chrome 支持 CSS Nesting 后 CSSStyleRule 也带 cssRules（空表），
        // 用它当"是不是容器"的判据会恒真 → 普通规则全被跳过、扫出空数组。
        if (r.selectorText) {
          if (r.selectorText.includes("md-path-link")) {
            matchedRules.push({
              source: href ? href.split("/").slice(-2).join("/") : "<inline>",
              selector: r.selectorText.replace(/\s+/g, " ").slice(0, 120),
              display: r.style.display || null,
              verticalAlign: r.style.verticalAlign || null,
              declCount: r.style.length,
            });
          }
        }
        if (r.cssRules && r.cssRules.length) walk(r.cssRules, href);
      }
    };
    for (const sheet of Array.from(document.styleSheets)) {
      let rules;
      try {
        rules = sheet.cssRules;
      } catch {
        continue;
      }
      walk(rules, sheet.href);
    }

    // 目标元素是否带行内 style
    const first = links[0];
    const inlineStyleAttr = first ? first.getAttribute("style") : null;

    // ── 规则来源与层叠归属（vite dev 用 <style data-vite-dev-id> 注入）──
    const styleTags = Array.from(document.querySelectorAll("style")).map((s, i) => ({
      i,
      file: s.getAttribute("data-vite-dev-id") || (s.id ? `#${s.id}` : "<no-id>"),
      hasPathLink: s.textContent.includes("md-path-link"),
      len: s.textContent.length,
    }));
    const pathLinkTags = styleTags.filter((t) => t.hasPathLink);

    // 对每个 style 标签内的 md-path-link 规则，看它是否被 @layer/@media 包着
    const ruleContext = [];
    for (const sheet of Array.from(document.styleSheets)) {
      let rules;
      try {
        rules = sheet.cssRules;
      } catch {
        continue;
      }
      for (const r of Array.from(rules)) {
        if (!r.selectorText || !r.selectorText.includes("md-path-link")) continue;
        ruleContext.push({
          selector: r.selectorText.replace(/\s+/g, " ").slice(0, 60),
          display: r.style.display || null,
          parentRule: r.parentRule ? r.parentRule.constructor.name : null,
          parentCssText: r.parentRule ? String(r.parentRule.cssText).slice(0, 60) : null,
          cssTextSample: r.cssText.slice(0, 120),
        });
      }
    }

    // div 探针：同样的类名放在非表单元素上
    const divProbe = document.createElement("div");
    divProbe.className = "md-path-link";
    divProbe.style.cssText = "position:absolute;visibility:hidden;";
    document.body.appendChild(divProbe);
    const divProbeDisplay = getComputedStyle(divProbe).display;
    const divProbeVA = getComputedStyle(divProbe).verticalAlign;
    const divProbeGap = getComputedStyle(divProbe).gap;
    const divProbeFF = getComputedStyle(divProbe).fontFamily.slice(0, 40);
    divProbe.remove();

    // 实验：同一类名 + 新注入的未分层规则 / 内联 style，看 display 能不能被改写
    const probeStyle = document.createElement("style");
    probeStyle.textContent = ".md-path-link-zz { display: inline-flex; }";
    document.head.appendChild(probeStyle);
    const d2 = document.createElement("div");
    d2.className = "md-path-link md-path-link-zz";
    document.body.appendChild(d2);
    const forcedByNewRule = getComputedStyle(d2).display;
    d2.remove();
    probeStyle.remove();

    const d3 = document.createElement("div");
    d3.style.display = "inline-flex";
    document.body.appendChild(d3);
    const forcedByInline = getComputedStyle(d3).display;
    d3.remove();

    const d4 = document.createElement("button");
    d4.style.display = "inline-flex";
    document.body.appendChild(d4);
    const btnForcedByInline = getComputedStyle(d4).display;
    d4.remove();

    const firstIdentity = first
      ? {
          tag: first.tagName,
          className: first.className,
          matchesClass: first.matches(".md-path-link"),
          displayByRules: first.matches(".md-path-link--bare") ? "bare" : "plain",
          // 同元素上 inline-block 是否来自某条作者规则
          uaProbe: (() => {
            const t = document.createElement("button");
            t.className = "md-path-link";
            t.style.position = "absolute";
            t.style.visibility = "hidden";
            document.body.appendChild(t);
            const d = getComputedStyle(t).display;
            t.remove();
            return d;
          })(),
        }
      : null;

    return {
      matchedRules,
      inlineStyleAttr,
      pathLinkTags,
      ruleContext,
      divProbeDisplay,
      divProbeVA,
      divProbeGap,
      divProbeFF,
      forcedByNewRule,
      forcedByInline,
      btnForcedByInline,
      firstIdentity,
      linkCount: links.length,
      iconCount: host.querySelectorAll(".md-path-link__icon").length,
      rows,
      inlineCodes,
      bodyFontSize: getComputedStyle(host.querySelector(".prose")).fontSize,
      bodyLineHeight: getComputedStyle(host.querySelector(".prose")).lineHeight,
    };
  }, MD);

  console.log("### PROBE");
  console.log(JSON.stringify(probe, null, 2));

  // ── 权威层叠：让浏览器自己说哪条规则赢了 display ──────────────────
  const client = await page.context().newCDPSession(page);
  await client.send("DOM.enable");
  await client.send("CSS.enable");
  const doc = await client.send("DOM.getDocument", { depth: -1 });
  const { nodeId } = await client.send("DOM.querySelector", {
    nodeId: doc.root.nodeId,
    selector: "#pathlink-probe .md-path-link",
  });
  const matched = await client.send("CSS.getMatchedStylesForNode", { nodeId });

  const displayChain = [];
  for (const m of matched.matchedCSSRules || []) {
    const props = (m.rule.style?.cssProperties || []).filter((p) => p.name === "display" || p.name === "vertical-align");
    if (!props.length) continue;
    displayChain.push({
      origin: m.rule.origin,
      selector: m.rule.selectorList.text.replace(/\s+/g, " ").slice(0, 90),
      matchingSelectors: m.matchingSelectors,
      inSource: m.rule.style?.range ? "author" : "?",
      decls: props.map((p) => `${p.name}: ${p.value}${p.implicit ? " (implicit)" : ""}${p.disabled ? " (disabled)" : ""}`),
    });
  }
  console.log("### DISPLAY_CHAIN");
  console.log(JSON.stringify(displayChain, null, 2));
  const computed = await client.send("CSS.getComputedStyleForNode", { nodeId });
  console.log("### COMPUTED_DISPLAY");
  console.log(
    JSON.stringify(
      computed.computedStyle.filter((p) => ["display", "vertical-align", "gap", "align-items", "white-space", "font-size"].includes(p.name)),
      null,
      2,
    ),
  );

  // 规则原始文本与每声明的 parsedOk —— 判定 display 是否"存在但无效"
  const raw = await client.send("CSS.getMatchedStylesForNode", { nodeId });
  const declDetail = (raw.matchedCSSRules || []).map((m) => ({
    selector: m.rule.selectorList.text.replace(/\s+/g, " ").slice(0, 70),
    origin: m.rule.origin,
    layer: m.rule.layerName ?? null,
    media: (m.rule.media || []).map((x) => x.text),
    allProps: (m.rule.style?.cssProperties || [])
      .map((p) => `${p.name}:${p.value}${p.important ? "!important" : ""}${p.disabled ? "(disabled)" : ""}`)
      .join(" | ")
      .slice(0, 500),
  }));
  console.log("### DECL_PARSED_OK");
  console.log(JSON.stringify(declDetail, null, 2));

  // 强制 !important 探针：若能让它变 inline-flex，说明是被更高优先级"非 important"压制
  const forceImported = await page.evaluate(() => {
    const st = document.createElement("style");
    st.textContent = ".md-path-link { display: inline-flex !important; }";
    document.head.appendChild(st);
    const d = document.createElement("div");
    d.className = "md-path-link";
    d.style.cssText = "position:absolute;visibility:hidden;";
    document.body.appendChild(d);
    const out = getComputedStyle(d).display;
    d.remove();
    st.remove();
    return out;
  });
  console.log("### FORCE_IMPORTANT_DISPLAY");
  console.log(forceImported);

  // ── 对照实验：临时把寄生规则里的 .md-path-link 摘掉（只改内存，不动磁盘）──
  const abTest = await page.evaluate(() => {
    const read = () => {
      const el = document.querySelector("#pathlink-probe .md-path-link");
      if (!el) return null;
      const svg = el.querySelector("svg");
      const span = el.querySelector("span");
      const holder = el.parentElement;
      const p = document.createElement("span");
      p.style.cssText = "display:inline-block;width:0;height:0;";
      holder.appendChild(p);
      const baseline = p.getBoundingClientRect().bottom;
      p.remove();
      const ir = svg.getBoundingClientRect();
      const sr = span.getBoundingClientRect();
      const er = el.getBoundingClientRect();
      return {
        linkDisplay: getComputedStyle(el).display,
        svgDisplay: getComputedStyle(svg).display,
        spanDisplay: getComputedStyle(span).display,
        linkHeight: +er.height.toFixed(2),
        iconBottomRelBaseline: +(ir.bottom - baseline).toFixed(2),
        iconTopVsSpanTop: +(ir.top - sr.top).toFixed(2),
        sameRow: Math.abs(ir.top - sr.top) < 4,
      };
    };
    const before = read();
    let patchedSelector = null;
    for (const sheet of Array.from(document.styleSheets)) {
      let rules;
      try {
        rules = sheet.cssRules;
      } catch {
        continue;
      }
      for (const r of Array.from(rules)) {
        if (r.selectorText && r.selectorText.includes("file-pill.is-created") && r.selectorText.includes("md-path-link")) {
          r.selectorText = r.selectorText
            .split(",")
            .filter((s) => !s.includes("md-path-link"))
            .join(",");
          patchedSelector = r.selectorText.replace(/\s+/g, " ").trim();
        }
      }
    }
    const after = read();
    return { before, after, patchedSelector };
  });
  console.log("### AB_TEST");
  console.log(JSON.stringify(abTest, null, 2));

  // ── 决定性实验：类规则的 display 声明到底有没有被解析/应用 ──────────
  const decisive = await page.evaluate(() => {
    const out = {};

    // A. 普通流（非 absolute）div + 同一个类
    const d = document.createElement("div");
    d.className = "md-path-link";
    d.textContent = "x";
    document.body.appendChild(d);
    out.flowDivDisplay = getComputedStyle(d).display;
    d.remove();

    // B. CSSOM 里那条规则的属性列表与 display 取值
    const found = [];
    for (const sheet of Array.from(document.styleSheets)) {
      let rules;
      try {
        rules = sheet.cssRules;
      } catch {
        continue;
      }
      for (const r of Array.from(rules)) {
        if (!r.selectorText) continue;
        if (r.selectorText === ".md-path-link") {
          found.push({
            displayValue: r.style.getPropertyValue("display"),
            displayPriority: r.style.getPropertyPriority("display"),
            propCount: r.style.length,
            hasDisplayInList: Array.from({ length: r.style.length }, (_, i) => r.style.item(i)).includes("display"),
            firstThree: Array.from({ length: Math.min(3, r.style.length) }, (_, i) => r.style.item(i)),
            parentType: r.parentRule ? r.parentRule.constructor.name : null,
            href: sheet.href,
          });
        }
        if (r.cssRules && r.cssRules.length) {
          for (const inner of Array.from(r.cssRules)) {
            if (inner.selectorText === ".md-path-link") {
              found.push({
                nested: true,
                parentType: inner.parentRule?.constructor.name,
                parentText: String(inner.parentRule?.cssText || "").slice(0, 80),
                displayValue: inner.style.getPropertyValue("display"),
              });
            }
          }
        }
      }
    }
    out.cssomRules = found;

    // C. 显式改写那条规则的 display，看 computed 变不变
    const real = document.querySelector("#pathlink-probe .md-path-link");
    out.realBefore = getComputedStyle(real).display;
    for (const sheet of Array.from(document.styleSheets)) {
      let rules;
      try {
        rules = sheet.cssRules;
      } catch {
        continue;
      }
      for (const r of Array.from(rules)) {
        if (r.selectorText === ".md-path-link") r.style.setProperty("display", "inline-flex");
      }
    }
    out.realAfterExplicitSet = getComputedStyle(real).display;

    // D. 有没有别的东西在设 display：逐个看匹配规则里 display 的值
    out.mediaQueries = Array.from(document.styleSheets)
      .flatMap((s) => {
        try {
          return Array.from(s.cssRules);
        } catch {
          return [];
        }
      })
      .filter((r) => r.constructor.name.includes("Media"))
      .map((r) => String(r.conditionText || r.media?.mediaText || "").slice(0, 60))
      .slice(0, 10);

    return out;
  });
  console.log("### DECISIVE");
  console.log(JSON.stringify(decisive, null, 2));

  // ── 直接从元素出发：它到底匹配了哪些规则 ──────────────────────────
  const ownerScan = await page.evaluate(() => {
    const el = document.querySelector("#pathlink-probe .md-path-link");
    const info = {
      found: !!el,
      className: el ? el.className : null,
      outerHTML: el ? el.outerHTML.slice(0, 180) : null,
      styleTagCount: document.querySelectorAll("style").length,
      styleTagsWithRule: [],
      matched: [],
    };
    if (!el) return info;

    for (const s of Array.from(document.querySelectorAll("style"))) {
      const n = (s.textContent.match(/md-path-link/g) || []).length;
      if (n)
        info.styleTagsWithRule.push({
          id: s.getAttribute("data-vite-dev-id") || s.id || "<no-id>",
          occurrences: n,
          len: s.textContent.length,
        });
    }

    // 深度不限地枚举："哪些规则能让 el.matches 为真"
    const visit = (list, path) => {
      for (const r of list) {
        if (r.selectorText) {
          let m = false;
          try {
            m = el.matches(r.selectorText);
          } catch {
            m = "selector-unparsable";
          }
          if (m === true) {
            info.matched.push({
              path,
              sel: r.selectorText.replace(/\s+/g, " ").slice(0, 70),
              display: r.style.display || null,
            });
          }
        }
        if (r.cssRules && r.cssRules.length) {
          const cond = r.conditionText || (r.media && r.media.mediaText) || r.name || "";
          visit(Array.from(r.cssRules), `${path}/${r.constructor.name}${cond ? `(${cond})` : ""}`);
        }
      }
    };
    for (const s of Array.from(document.styleSheets)) {
      let rules;
      try {
        rules = s.cssRules;
      } catch {
        continue;
      }
      visit(Array.from(rules), s.href ? s.href.split("/").pop() : "<inline>");
    }
    return info;
  });
  console.log("### OWNER_SCAN");
  console.log(JSON.stringify(ownerScan, null, 2));

  // ── button vs div：类规则 / 内联 在同一属性上的四种组合 ──────────────
  const btnVsDiv = await page.evaluate(() => {
    const st = document.createElement("style");
    st.textContent = ".zz-probe { display: inline-flex; }";
    document.head.appendChild(st);
    const mk = (tag, useInline) => {
      const el = document.createElement(tag);
      el.className = "zz-probe";
      if (useInline) el.style.display = "inline-flex";
      document.body.appendChild(el);
      const cs = getComputedStyle(el);
      const out = { display: cs.display, appearance: cs.appearance, position: cs.position };
      el.remove();
      return out;
    };
    const divClass = mk("div", false);
    const btnClass = mk("button", false);
    const btnInline = mk("button", true);
    const real = document.querySelector("#pathlink-probe .md-path-link");
    const realCS = getComputedStyle(real);
    st.remove();
    return { divClass, btnClass, btnInline, real: { display: realCS.display, appearance: realCS.appearance } };
  });
  console.log("### BTN_VS_DIV");
  console.log(JSON.stringify(btnVsDiv, null, 2));

  const shotPath = path.join(OUT_DIR, "probe.png");
  try {
    await page.locator("#pathlink-probe").screenshot({ path: shotPath });
  } catch (e) {
    await page.screenshot({ path: shotPath, fullPage: true });
  }
  console.log("screenshot:", shotPath);

  // ── 契约断言：任一条不满足即非零退出（回归守卫）────────────────────
  // 这类 bug（CSS 层叠被后加载文件压制）在 jsdom/node 单测里抓不到 ——
  // MarkdownBody.pathlink.test.tsx 只覆盖组件逻辑，不加载 CSS。本断言是
  // 唯一能挡住复发的防线，故必须"失败即红"，不能只打印。
  const violations = [];
  const rows = probe.rows || [];
  if (rows.length === 0)
    violations.push("没有任何 .md-path-link 被渲染（选择器或组件回归？）");
  for (const r of rows) {
    if (r.display !== "inline-flex")
      violations.push(`[${r.text}] display=${r.display}，期望 inline-flex`);
    if (r.sameRow !== true)
      violations.push(`[${r.text}] 图标与文件名不同行（iconVsNameTop=${r.iconVsNameTop}）`);
  }
  if (probe.iconCount !== probe.linkCount)
    violations.push(`图标数 ${probe.iconCount} != 链接数 ${probe.linkCount}`);

  console.log("\n### CONTRACT");
  if (violations.length) {
    console.error("FAIL 行内路径链接契约被破坏：");
    for (const v of violations) console.error("  - " + v);
    await browser.close();
    process.exit(1);
  }
  console.log(
    `PASS ${rows.length}/${rows.length} 链接均为 inline-flex 且图标与文件名同行（iconCount=${probe.iconCount}）`,
  );

  await browser.close();
})();
