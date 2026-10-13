/**
 * 文件预览引擎实机验证（纯浏览器 + 伪 Tauri internals）。
 *
 * 存在的理由：预览重构连续两轮被判「没任何变化」——tsc / vitest / vite build
 * 全绿不代表引擎真的把字节画出来了。这个脚本用真实样本（docx / pdf / pptx /
 * 图片）走完整链路 resolveKind → loaders → viewer，断言 DOM 里出现引擎产物
 * （canvas、docx 渲染出的文本），并截图留证。
 *
 * 跑法：cd apps/desktop-tauri && node scripts/verify-preview-engines.cjs
 * 前置：dev server 已在 1420 端口运行（npm run dev）。
 */

const fs = require("fs");
const path = require("path");
const { chromium } = require("playwright");

const OUT_DIR = path.resolve(__dirname, "..", "..", "..", "outputs");
const DEV_URL = process.env.PV_DEV_URL || "http://127.0.0.1:1420";

/** 无依赖生成的最小样本：docx 是 zip+xml，pdf 手写对象表，pptx 同理。 */
function buildSamples() {
  const samples = {};

  // ── docx ────────────────────────────────────────────────────────
  {
    const files = {
      "[Content_Types].xml": `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>`,
      "_rels/.rels": `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>`,
      "word/document.xml": `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:rPr><w:b/><w:sz w:val="48"/></w:rPr><w:t>PREVIEW ENGINE PROOF</w:t></w:r></w:p>
<w:p><w:r><w:t>docx-preview 已接管渲染（纯前端引擎）</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>单元格 A</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>单元格 B</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
</w:body></w:document>`,
    };
    samples.docx = zip(files);
  }

  // ── pptx ────────────────────────────────────────────────────────
  {
    const slideRels = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>`;
    const files = {
      "[Content_Types].xml": `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/></Types>`,
      "_rels/.rels": `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>`,
      "ppt/presentation.xml": `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId id="256" r:id="rId2"/></p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>`,
      "ppt/_rels/presentation.xml.rels": `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>`,
      "ppt/slides/slide1.xml": `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr><p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="838200" y="1825625"/><a:ext cx="10515600" cy="1325563"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr lang="zh-CN" sz="4000" b="1"/><a:t>PPTX ENGINE PROOF</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>`,
      "ppt/slides/_rels/slide1.xml.rels": slideRels,
      "ppt/slideLayouts/slideLayout1.xml": `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sldLayout xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" type="blank" preserve="1"><p:cSld name="Blank"><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>`,
    };
    samples.pptx = zip(files);
  }

  // ── 图片：2x2 红色 PNG（手写最小 PNG） ─────────────────────────
  samples.image = Buffer.from(
    "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAFUlEQVR42mP8z8DAwMDAxAADjAwMDAAtAAO/1hQ0AAAAAElFTkSuQmCC",
    "base64",
  );

  // ── pdf：手写单页对象表 ────────────────────────────────────────
  {
    const objs = [
      Buffer.from("<< /Type /Catalog /Pages 2 0 R >>"),
      Buffer.from("<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
      Buffer.from(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
      ),
      null,
      Buffer.from("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
    ];
    const stream = Buffer.from(
      "BT /F1 30 Tf 60 720 Td (PDF ENGINE PROOF) Tj ET\nBT /F1 14 Tf 60 660 Td (rendered by pdfjs canvas) Tj ET",
    );
    objs[3] = Buffer.concat([
      Buffer.from(`<< /Length ${stream.length} >>\nstream\n`),
      stream,
      Buffer.from("\nendstream"),
    ]);
    const out = [Buffer.from("%PDF-1.4\n")];
    const offsets = [];
    let len = out[0].length;
    objs.forEach((o, i) => {
      offsets.push(len);
      const chunk = Buffer.concat([
        Buffer.from(`${i + 1} 0 obj\n`),
        o,
        Buffer.from("\nendobj\n"),
      ]);
      out.push(chunk);
      len += chunk.length;
    });
    const xref = len;
    let xrefBody = `xref\n0 ${objs.length + 1}\n0000000000 65535 f \n`;
    for (const off of offsets) xrefBody += `${String(off).padStart(10, "0")} 00000 n \n`;
    out.push(
      Buffer.from(
        xrefBody +
          `trailer\n<< /Size ${objs.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`,
      ),
    );
    samples.pdf = Buffer.concat(out);
  }

  return samples;
}

/** 极简 zip 打包（store，无压缩）。够 OOXML 解析器读。 */
function zip(files) {
  const entries = [];
  const central = [];
  let offset = 0;
  for (const [name, content] of Object.entries(files)) {
    const nameBuf = Buffer.from(name, "utf8");
    const dataBuf = Buffer.from(content, "utf8");
    const crc = crc32(dataBuf);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(0, 6);
    local.writeUInt16LE(0, 8);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(dataBuf.length, 18);
    local.writeUInt32LE(dataBuf.length, 22);
    local.writeUInt16LE(nameBuf.length, 26);
    entries.push(local, nameBuf, dataBuf);

    const cd = Buffer.alloc(46);
    cd.writeUInt32LE(0x02014b50, 0);
    cd.writeUInt16LE(20, 4);
    cd.writeUInt16LE(20, 6);
    cd.writeUInt32LE(crc, 16);
    cd.writeUInt32LE(dataBuf.length, 20);
    cd.writeUInt32LE(dataBuf.length, 24);
    cd.writeUInt16LE(nameBuf.length, 28);
    cd.writeUInt32LE(offset, 42);
    central.push(cd, nameBuf);

    offset += 30 + nameBuf.length + dataBuf.length;
  }
  const localPart = Buffer.concat(entries);
  const centralPart = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(Object.keys(files).length, 8);
  end.writeUInt16LE(Object.keys(files).length, 10);
  end.writeUInt32LE(centralPart.length, 12);
  end.writeUInt32LE(localPart.length, 16);
  return Buffer.concat([localPart, centralPart, end]);
}

function crc32(buf) {
  let c;
  const table = [];
  for (let n = 0; n < 256; n++) {
    c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  let crc = 0xffffffff;
  for (const b of buf) crc = table[(crc ^ b) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

/** 每种文件的判定断言：命中即证明对应引擎真的渲染了。 */
const CASES = [
  {
    key: "docx",
    file: "sample.docx",
    kind: "office",
    expect: async (page) => ({
      engineHost: await page.locator(".preview-docx-body").count(),
      text: (await page.locator(".preview-docx-body").first().innerText().catch(() => "")).slice(0, 80),
      hasTable: await page.locator(".preview-docx-body table").count(),
    }),
    pass: (r) => r.engineHost > 0 && /PREVIEW ENGINE PROOF/.test(r.text),
  },
  {
    key: "pptx",
    file: "sample.pptx",
    kind: "office",
    expect: async (page) => ({
      canvas: await page.locator(".preview-pptx-stage canvas").count(),
      width: await page
        .locator(".preview-pptx-stage canvas")
        .first()
        .evaluate((el) => el.width)
        .catch(() => 0),
      bar: await page.locator(".preview-pdf-page").count(),
    }),
    pass: (r) => r.canvas > 0 && r.width > 0,
  },
  {
    key: "pdf",
    file: "sample.pdf",
    kind: "office",
    expect: async (page) => ({
      canvas: await page.locator(".preview-pdf-stage canvas").count(),
      width: await page
        .locator(".preview-pdf-stage canvas")
        .first()
        .evaluate((el) => el.width)
        .catch(() => 0),
      pageLabel: await page.locator(".preview-pdf-page").first().innerText().catch(() => ""),
    }),
    pass: (r) => r.canvas > 0 && r.width > 0,
  },
  {
    key: "image",
    file: "sample.png",
    kind: "image",
    expect: async (page) => ({
      img: await page.locator(".preview-image-img").count(),
      bar: await page.locator(".preview-pdf-bar").count(),
    }),
    pass: (r) => r.img > 0 && r.bar > 0,
  },
];

async function main() {
  fs.mkdirSync(OUT_DIR, { recursive: true });
  const samples = buildSamples();
  const payload = {};
  for (const [k, v] of Object.entries(samples)) payload[k] = v.toString("base64");

  // `--no-proxy-server` 不能省：环境里 HTTP_PROXY 指向 127.0.0.1:52644，
  // Chromium 会连 localhost:1420 也走代理，表现为 goto 60s 超时（跟
  // curl 拿 502 是同一个根因，看起来完全像 dev server 没起来）。
  const launchOpts = { channel: "chrome", args: ["--no-proxy-server"] };
  const browser = await chromium
    .launch(launchOpts)
    .catch(() => chromium.launch({ ...launchOpts, channel: "msedge" }));
  const results = [];

  try {
    for (const c of CASES) {
      const page = await browser.newPage();
      await page.setViewportSize({ width: 1280, height: 860 });

      await page.addInitScript((data) => {
        window.__PV_SAMPLES__ = data.bytes;
        window.__PV_FILE__ = data.file;
        window.__PV_KIND__ = data.kind;
        window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
        window.__TAURI_INTERNALS__ = {
          metadata: {
            currentWindow: { label: "main" },
            currentWebview: { label: "main", windowLabel: "main" },
          },
          transformCallback: () => 1,
          convertFileSrc: (p) => String(p),
          invoke: (cmd, args) => {
            if (cmd === "read_artifact_file_bytes") {
              const b64 = window.__PV_SAMPLES__;
              const bin = atob(b64);
              return Promise.resolve({
                kind: "bytes",
                contentBase64: b64,
                size: bin.length,
                resolvedPath: (args && args.path) || "",
              });
            }
            return Promise.resolve({});
          },
          plugins: {},
        };
      }, { bytes: payload[c.key], file: c.file, kind: c.kind });

      await page.goto(DEV_URL, { waitUntil: "domcontentloaded", timeout: 60000 });
      await page.waitForTimeout(1500);

      // 把 PreviewPanel 单独挂到一个浮层上：绕开 App 自身的会话/导航状态，
      // 只验「文件字节 → 引擎渲染」这一段。
      await page.evaluate(async (meta) => {
        const host = document.createElement("div");
        host.id = "pv-probe";
        host.style.cssText =
          "position:fixed;inset:0;z-index:99999;background:#1c1c20;display:flex;";
        document.body.appendChild(host);
        // react / react-dom 必须用 vite 预构建产物的真实路径：evaluate 里的
        // 动态 import 不经过 vite 的裸模块重写，import("react") 会直接抛
        // "Failed to resolve module specifier"。PreviewPanel 走 /src/ 路径，
        // 它内部的 import 由 vite 转换，且指向同一份预构建产物（同一实例）。
        const [mod0, mod1, mod] = await Promise.all([
          import("/node_modules/.vite/deps/react.js"),
          import("/node_modules/.vite/deps/react-dom_client.js"),
          import("/src/components/PreviewPanel.tsx"),
        ]);
        // 预构建产物是 CJS→ESM 包装，导出可能挂在 default 上。
        const React = mod0.default ?? mod0;
        const ReactDOM = mod1.default ?? mod1;
        const root = ReactDOM.createRoot(host);
        root.render(
          React.createElement(mod.PreviewPanel, {
            artifact: {
              id: "probe",
              name: meta.file,
              path: meta.file,
              kind: meta.kind,
              tool: "probe",
              timestamp: new Date().toISOString(),
              isAbsolute: true,
            },
            workspaceRoot: null,
            onBack: () => {},
          }),
        );
      }, { file: c.file, kind: c.kind });

      await page.waitForTimeout(6000);
      const measured = await c.expect(page);
      const shot = path.join(OUT_DIR, `preview-${c.key}.png`);
      await page.screenshot({ path: shot });
      results.push({
        case: c.key,
        pass: c.pass(measured),
        ...measured,
        error: await page.locator(".preview-state-error-row").first().innerText().catch(() => null),
        shot,
      });
      await page.close();
    }
  } finally {
    await browser.close();
  }

  console.log(JSON.stringify(results, null, 2));
  const failed = results.filter((r) => !r.pass);
  if (failed.length) {
    console.error(`\nFAILED: ${failed.map((f) => f.case).join(", ")}`);
    process.exit(1);
  }
  console.log(`\nALL PASS (${results.length} cases)`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
