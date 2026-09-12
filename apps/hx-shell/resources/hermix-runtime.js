// Hermix 自定义应用壳预置前端能力层 (hermix-runtime.js)
// spec §7 — 壳暴露给 app 的 window.Hermix.* 原语全集。
// 通过 hx-shell 的 with_initialization_script 在每个页面加载前注入，
// 多文件 app 在 app:// 协议下与脚本一同加载。
//
// 设计原则 (六步决策阶梯):
// 1. YAGNI: 仅实现 LLM 高频刚需（store/component/icon/route/storage/sqlite/screenshot/无障碍）
// 2. 标准库优先：fetch / Proxy / CustomElementRegistry / DOM API 都是 Web 标准
// 3. 原生平台优先：组件用原生 CustomElements，状态用原生 Proxy，不引 React
// 4. 现有依赖：Preact 全局 ~10KB 留 Phase 4 扩展；本轮不内嵌
// 5. 单行能完不展开
// 6. 最小可用实现：接口稳定 + 关键路径跑通，性能优化留后
//
// IIFE 包装：(function(Hermix){ ... })(window.Hermix = window.Hermix || {})
// 允许多次注入幂等（pe 同窗口重载 / reload 时不会重复注册）。

(function (Hermix) {
  "use strict";

  // ─────────────────────────────────────────────────────────
  // 0. appId 隔离根 — 来自壳 spawn 时写好的 window.Hermix.__appId
  //    缺省 "anonymous" 用于直接 --html 加载的临时调试页
  // ─────────────────────────────────────────────────────────
  const APP_ID = (typeof Hermix.__appId === "string" && Hermix.__appId) || "anonymous";
  const KEY_PREFIX = "hx:" + APP_ID + ":";          // localStorage key 前缀
  const IDB_PREFIX = "hx-app-" + APP_ID;            // IndexedDB 库名前缀

  // ─────────────────────────────────────────────────────────
  // 1. Hermix.store — Proxy 响应式 state + 订阅即重渲染
  // ─────────────────────────────────────────────────────────
  // 设计：深层 reactive，get 收集依赖 (effect), set 触发依赖。
  // 简化：订阅者 = 整树重渲染 target = document.body 也行；首期用「订阅函数」
  //      模型，组件注册 render 时调 store.subscribe(fn)，fn 内读 store 然后手动 patch DOM。
  //      更激进：自动 inject 到所有 Hermix.defineComponent 组件，但这是大改动 ——
  //      本轮不引入，留作 Phase 4 扩展。
  function reactive(obj, onNotify) {
    const handler = {
      get(target, key, receiver) {
        const v = Reflect.get(target, key, receiver);
        if (v && typeof v === "object" && !v.__reactive__ && !Array.isArray(v)) {
          // 递归包装嵌套对象（一次性，避免循环）
          const wrapped = reactive(v, onNotify);
          Reflect.set(target, key, wrapped, receiver);
          return wrapped;
        }
        return v;
      },
      set(target, key, value, receiver) {
        const old = target[key];
        const ok = Reflect.set(target, key, value, receiver);
        if (ok && old !== value) onNotify(key);
        return ok;
      },
      deleteProperty(target, key) {
        const had = key in target;
        const ok = Reflect.deleteProperty(target, key);
        if (ok && had) onNotify(key);
        return ok;
      },
    };
    obj.__reactive__ = true;
    return new Proxy(obj, handler);
  }

  Hermix.store = function (initial) {
    const subs = new Set();
    const state = reactive(
      typeof initial === "function" ? initial() : (initial || {}),
      (k) => { for (const fn of subs) { try { fn(k, state); } catch (e) { console.error("[Hermix.store] subscriber error:", e); } } }
    );
    return {
      state,
      subscribe(fn) { subs.add(fn); return () => subs.delete(fn); },
      patch(partial) { Object.assign(state, partial); },
    };
  };

  // ─────────────────────────────────────────────────────────
  // 2. Hermix.defineComponent(name, def) — 薄封装原生 Custom Elements
  // ─────────────────────────────────────────────────────────
  // def = { props?, state?, render, onMount?, onUnmount? }
  // 用 Shadow DOM 隔离样式（壳 inject 一段 minimal reset）。
  // state 变更触发 render 自动重渲染（细粒度订阅到实例）。
  Hermix.defineComponent = function (name, def) {
    if (!customElements.get(name)) {
      // 兼容旧浏览器 fallback：直接 return 一个无效果壳，不抛错
      if (typeof customElements === "undefined") return;
      const Cls = class extends HTMLElement {
        constructor() {
          super();
          this._state = null;
          this._subs = null;
          this._render = def.render || (() => "");
          this._props = {};
          const root = this.attachShadow({ mode: "open" });
          root.innerHTML = `
            <style>:host { display: block; font-family: inherit; }
              button { cursor: pointer; }</style>
            <slot id="root"></slot>`;
          this._root = root.querySelector("#root");
        }
        connectedCallback() {
          // props 从 attribute 反射
          if (def.props) {
            for (const p of def.props) {
              if (this.hasAttribute(p)) this._props[p] = this.getAttribute(p);
            }
          }
          // state 初始化
          if (def.state) {
            this._state = Hermix.store(typeof def.state === "function"
              ? () => def.state.call(this)
              : def.state);
            this._subs = this._state.subscribe(() => this._doRender());
          }
          this._doRender();
          if (def.onMount) def.onMount.call(this);
        }
        disconnectedCallback() {
          if (this._subs) this._subs();
          if (def.onUnmount) def.onUnmount.call(this);
        }
        attributeChangedCallback(name, _old, val) {
          this._props[name] = val;
          this._doRender();
        }
        adoptedCallback() {}
        _doRender() {
          try {
            // 把 this._root 当容器，render 接受 props+state 返回 string（HTML 片段）
            const html = this._render.call(this, this._props, this._state ? this._state.state : {});
            this._root.innerHTML = html;
          } catch (e) {
            console.error("[Hermix.defineComponent " + name + "] render error:", e);
          }
        }
      };
      // 静态反射属性列表
      Cls.observedAttributes = (def.props || []).slice();
      customElements.define(name, Cls);
    }
  };

  // ─────────────────────────────────────────────────────────
  // 3. Hermix.icon(name) — Lucide 精选子集（SVG path 字符串）
  // ─────────────────────────────────────────────────────────
  // 全部 icon 24x24 viewBox="0 0 24 24" stroke="currentColor"
  // stroke-width="2" fill="none" stroke-linecap="round" stroke-linejoin="round"。
  // 精选 40+ 枚 — 多用是 vendor 兜底（用 Hermix.icon.register(name, svg)）。
  const ICON_LIB = {
    "settings": '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 0 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 0 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 0 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 0 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>',
    "home": '<path d="m3 9 9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><polyline points="9 22 9 12 15 12 15 22"/>',
    "search": '<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>',
    "plus": '<path d="M5 12h14"/><path d="M12 5v14"/>',
    "x": '<path d="M18 6 6 18"/><path d="m6 6 12 12"/>',
    "chevron-down": '<path d="m6 9 6 6 6-6"/>',
    "chevron-up": '<path d="m18 15-6-6-6 6"/>',
    "chevron-right": '<path d="m9 18 6-6-6-6"/>',
    "chevron-left": '<path d="m15 18-6-6 6-6"/>',
    "arrow-up": '<path d="m5 12 7-7 7 7"/><path d="M12 19V5"/>',
    "arrow-down": '<path d="M12 5v14"/><path d="m19 12-7 7-7-7"/>',
    "arrow-right": '<path d="M5 12h14"/><path d="m12 5 7 7-7 7"/>',
    "arrow-left": '<path d="m12 19-7-7 7-7"/><path d="M19 12H5"/>',
    "check": '<path d="M20 6 9 17l-5-5"/>',
    "alert-triangle": '<path d="m21.7 18-8-14a2 2 0 0 0-3.5 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.7-3"/><path d="M12 9v4"/><path d="M12 17h.01"/>',
    "info": '<circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/>',
    "user": '<path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/>',
    "mail": '<rect width="20" height="16" x="2" y="4" rx="2"/><path d="m22 7-8.97 5.7a1.94 1.94 0 0 1-2.06 0L2 7"/>',
    "calendar": '<rect width="18" height="18" x="3" y="4" rx="2"/><path d="M16 2v4"/><path d="M8 2v4"/><path d="M3 10h18"/>',
    "file": '<path d="M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5z"/><polyline points="14 2 14 8 20 8"/>',
    "folder": '<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.7-.9l-.8-1.2A2 2 0 0 0 7.9 2H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/>',
    "download": '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><path d="M12 15V3"/>',
    "upload": '<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="17 8 12 3 7 8"/><path d="M12 3v12"/>',
    "trash": '<path d="M3 6h18"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>',
    "copy": '<rect width="13" height="13" x="9" y="9" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>',
    "edit": '<path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.5 2.5a2.12 2.12 0 0 1 3 3L12 15l-4 1 1-4Z"/>',
    "save": '<path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2z"/><polyline points="17 21 17 13 7 13 7 21"/><polyline points="7 3 7 8 15 8"/>',
    "play": '<polygon points="6 3 20 12 6 21 6 3"/>',
    "pause": '<rect width="4" height="16" x="6" y="4"/><rect width="4" height="16" x="14" y="4"/>',
    "star": '<polygon points="12 2 15.1 8.3 22 9.3 17 14.1 18.2 21 12 17.8 5.8 21 7 14.1 2 9.3 8.9 8.3 12 2"/>',
    "heart": '<path d="M19 14c1.5-1.5 3-3.2 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.8 0-3 .5-4.5 2-1.5-1.5-2.7-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4 3 5.5l7 7Z"/>',
    "menu": '<line x1="4" x2="20" y1="12" y2="12"/><line x1="4" x2="20" y1="6" y2="6"/><line x1="4" x2="20" y1="18" y2="18"/>',
    "more": '<circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/>',
    "sun": '<circle cx="12" cy="12" r="4"/><path d="M12 2v2"/><path d="M12 20v2"/><path d="m4.9 4.9 1.4 1.4"/><path d="m17.7 17.7 1.4 1.4"/><path d="M2 12h2"/><path d="M20 12h2"/><path d="m6.3 17.7-1.4 1.4"/><path d="m19.1 4.9-1.4 1.4"/>',
    "moon": '<path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8z"/>',
    "bell": '<path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9"/><path d="M10.3 21a1.94 1.94 0 0 0 3.4 0"/>',
    "bookmark": '<path d="m19 21-7-5-7 5V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v16z"/>',
    "tag": '<path d="M12 2H2v10l9.3 9.3a2 2 0 0 0 2.8 0l6.6-6.6a2 2 0 0 0 0-2.8L12 2Z"/><path d="M7 7h.01"/>',
    "image": '<rect width="18" height="18" x="3" y="3" rx="2" ry="2"/><circle cx="9" cy="9" r="2"/><path d="m21 15-3.1-3.1a2 2 0 0 0-2.8 0L6 21"/>',
    "filter": '<polygon points="22 3 2 3 10 12.5 10 19 14 21 14 12.5 22 3"/>',
    "refresh": '<path d="M21 12a9 9 0 0 0-9-9 9.7 9.7 0 0 0-6.7 2.7L3 8"/><path d="M3 3v5h5"/><path d="M3 12a9 9 0 0 0 9 9 9.7 9.7 0 0 0 6.7-2.7L21 16"/><path d="M16 16h5v5"/>',
  };
  Hermix.icon = function (name, opts) {
    const path = ICON_LIB[name];
    if (!path) {
      // fall back to empty 24x24 box (visual placeholder)
      return '<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg" width="' +
        ((opts && opts.size) || 24) +
        '" height="' + ((opts && opts.size) || 24) + '"></svg>';
    }
    const size = (opts && opts.size) || 24;
    const cls = (opts && opts.class) || "";
    return '<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg" ' +
      'width="' + size + '" height="' + size + '" stroke="currentColor" ' +
      'stroke-width="2" fill="none" stroke-linecap="round" stroke-linejoin="round" ' +
      'class="' + cls + '">' + path + '</svg>';
  };
  Hermix.icon.register = function (name, svgInner) { ICON_LIB[name] = svgInner; };
  Hermix.icon.list = function () { return Object.keys(ICON_LIB); };

  // ─────────────────────────────────────────────────────────
  // 4. Hermix.route — hash 路由 helper
  // ─────────────────────────────────────────────────────────
  // 单页多视图免手写 hash 监听：
  //   Hermix.route('/list', () => renderList(), { default: true })
  //   Hermix.route('/detail/:id', (m) => renderDetail(m.id))
  //   Hermix.go('/detail/42')
  Hermix.route = function (pattern, handler, opts) {
    const reg = routeCompile(pattern);
    routes.push({ reg, handler, opts: opts || {} });
    if ((opts || {}).default) currentRoute = pattern;
  };
  Hermix.go = function (path) {
    if (location.hash !== "#" + path) location.hash = "#" + path;
    else dispatch();
  };
  function routeCompile(pattern) {
    // 仅支持 :name 参数；其他当字面量
    const segs = pattern.split("/").filter(Boolean);
    const parts = segs.map((s) => s.startsWith(":") ? { name: s.slice(1), literal: null } : { literal: s, name: null });
    return {
      pattern,
      match(path) {
        if (!path.startsWith("/")) return null;
        const ps = path.split("/").filter(Boolean);
        if (ps.length !== parts.length) return null;
        const out = {};
        for (let i = 0; i < parts.length; i++) {
          const p = parts[i];
          if (p.literal !== null) { if (ps[i] !== p.literal) return null; }
          else { out[p.name] = decodeURIComponent(ps[i]); }
        }
        return out;
      },
    };
  }
  const routes = [];
  let currentRoute = null;
  function dispatch() {
    const path = (location.hash || "#/").slice(1);
    for (const r of routes) {
      const m = r.reg.match(path);
      if (m) { try { r.handler(m); return; } catch (e) { console.error("[Hermix.route]", e); return; } }
    }
    // 没匹配 — 用默认
    const dflt = routes.find((r) => r.opts.default);
    if (dflt) dflt.handler({});
  }
  window.addEventListener("hashchange", dispatch);
  // 启动时不自动 dispatch — 让 app 显式 Hermix.route(...) 注册后再 dispatch
  Hermix.route.start = function () { dispatch(); };

  // ─────────────────────────────────────────────────────────
  // 5. Hermix.storage — localStorage 封装 + appId 命名空间隔离
  //    API: get/set/remove/clear/list
  // ─────────────────────────────────────────────────────────
  const storage = {
    get(key, fallback) {
      const v = localStorage.getItem(KEY_PREFIX + key);
      if (v === null) return fallback;
      try { return JSON.parse(v); } catch { return v; }
    },
    set(key, val) {
      try { localStorage.setItem(KEY_PREFIX + key, JSON.stringify(val)); }
      catch (e) { console.warn("[Hermix.storage] set failed:", e); }
    },
    remove(key) { localStorage.removeItem(KEY_PREFIX + key); },
    clear() {
      // 只清当前 app 的 key，不动宿主
      const prefix = KEY_PREFIX;
      const toDel = [];
      for (let i = 0; i < localStorage.length; i++) {
        const k = localStorage.key(i);
        if (k && k.startsWith(prefix)) toDel.push(k);
      }
      for (const k of toDel) localStorage.removeItem(k);
    },
    list() {
      const out = [];
      const prefix = KEY_PREFIX;
      for (let i = 0; i < localStorage.length; i++) {
        const k = localStorage.key(i);
        if (k && k.startsWith(prefix)) out.push(k.slice(prefix.length));
      }
      return out;
    },
  };
  // IndexedDB 入口（按 appId 命名空间）
  storage.idb = function (name) {
    return new Promise((resolve, reject) => {
      if (typeof indexedDB === "undefined") return reject(new Error("IDB unavailable"));
      const req = indexedDB.open(IDB_PREFIX + "/" + name);
      req.onerror = () => reject(req.error);
      req.onsuccess = () => resolve(req.result);
      req.onupgradeneeded = () => {
        // app 自己处理 onupgradeneeded — 重新打开前可能需要 schema
        const db = req.result;
        if (db.objectStoreNames.length === 0) db.createObjectStore("kv");
      };
    });
  };
  Hermix.storage = storage;

  // ─────────────────────────────────────────────────────────
  // 6. Hermix.sqlite — sql.js WASM 接入 (Phase 4 vendor 完整，Phase 3 先稳定 API)
  // ─────────────────────────────────────────────────────────
  // 设计：app_dir/vendor/sql-wasm.wasm 必须存在（虚拟协议 app://vendor/sql-wasm.wasm）。
  // 缺则 init() reject with 明确错误，app 引导用户重 vendor。
  //
  // dev 模式：当 hx-shell 用 --html-url http(s):// 加载时，页面 origin 在远程域。
  //   此时直接 `fetch("app://...")` 被 WebView2 同源策略拦。
  //   解决：检测 location.protocol，dev 用 wry 重写后的形式 `http(s)://app.localhost/...`，
  //   生产仍走 `app://localhost/...`。
  function __hermixAppUrl(relpath) {
    const proto = (typeof location !== "undefined" && location.protocol) || "";
    if (proto === "http:" || proto === "https:") {
      return proto + "//app.localhost/" + relpath.replace(/^\/+/, "");
    }
    return "app://localhost/" + relpath.replace(/^\/+/, "");
  }
  Hermix.__appUrl = __hermixAppUrl;

  let _sqlReady = null;
  Hermix.sqlite = {
    init() {
      if (_sqlReady) return _sqlReady;
      _sqlReady = new Promise((resolve, reject) => {
        if (!window.fetch) return reject(new Error("fetch unavailable"));
        const jsUrl = __hermixAppUrl("vendor/sql-wasm.js");
        // 动态加载 sql-wasm.js (UMD) — dev 用 fetch+eval，生产直接 <script src>
        const proto = (typeof location !== "undefined" && location.protocol) || "";
        if (proto === "http:" || proto === "https:") {
          fetch(jsUrl).then((r) => {
            if (!r.ok) throw new Error("HTTP " + r.status);
            return r.text();
          }).then((code) => {
            // eval 后 initSqlJs 应暴露于 window
            const s = document.createElement("script");
            s.textContent = code;
            document.head.appendChild(s);
            if (typeof initSqlJs !== "function") {
              return reject(new Error("initSqlJs 未暴露于 window (dev 模式下 sql-wasm.js 内容异常?)"));
            }
            return initSqlJs({ locateFile: (f) => __hermixAppUrl("vendor/" + f) });
          }).then((SQL) => { _sqlLib = SQL; resolve(SQL); }).catch(reject);
          return;
        }
        // 生产：app:// 协议，由壳 app_protocol_handler 解包喂 WebView
        const script = document.createElement("script");
        script.src = "app://vendor/sql-wasm.js";
        script.onerror = () => reject(new Error(
          "app://vendor/sql-wasm.js 加载失败 — 请在 app_dir/vendor/ 内 sql-wasm.js + sql-wasm.wasm (Phase 4 vendor)"
        ));
        script.onload = () => {
          if (typeof initSqlJs !== "function") return reject(new Error("initSqlJs 未暴露于 window"));
          initSqlJs({
            locateFile: (f) => __hermixAppUrl("vendor/" + f),
          }).then((SQL) => {
            _sqlLib = SQL;
            resolve(SQL);
          }).catch(reject);
        };
        document.head.appendChild(script);
      });
      return _sqlReady;
    },
    async open(name) {
      const SQL = await Hermix.sqlite.init();
      // 持久化路径: ~/.hermix/custom-apps-data/<appId>/<name>.sqlite
      // 但 WebView 没直接 FS — 走 window.hx.readFileBytes / writeFileBytes
      const path = "__sqlite__/" + name + ".db";
      let bytes;
      if (window.hx && window.hx.readFileBytes) {
        try { bytes = await window.hx.readFileBytes(path); } catch (_) { bytes = null; }
      }
      const db = bytes ? new SQL.Database(bytes) : new SQL.Database();
      // 自动持久化钩子：每次 exec 后整库导出（wasm 通病，见 spec §7.4）
      let dirty = false;
      const persistHook = () => { dirty = true; };
      return {
        exec(sql, params) {
          const r = db.exec(sql, params || []);
          persistHook();
          return r;
        },
        run(sql, params) { db.run(sql, params || []); persistHook(); },
        async save() {
          if (window.hx && window.hx.writeFileBytes) {
            const buf = db.export();
            await window.hx.writeFileBytes(path, buf);
            dirty = false;
          }
        },
        get dirty() { return dirty; },
      };
    },
  };
  let _sqlLib = null;

  // ─────────────────────────────────────────────────────────
  // 7. Hermix.screenshot() — 离屏 PNG 导出 (简化版)
  // ─────────────────────────────────────────────────────────
  // 实现路线：
  //   a) target 元素 (默认 document.body) 边界矩形
  //   b) 用 foreignObject SVG + 当前 document clone → Image + canvas
  //   c) canvas.toDataURL("image/png") → 经 window.hx.saveDialog 弹出保存框落盘
  // 局限：无法截到 <canvas> / <video>（外部纹理）— 这是 foreignObject 的已知限制。
  //      复杂场景留 Phase 4 + WebView2 WebView.CapturePreview 桥接。
  Hermix.screenshot = async function (opts) {
    const target = (opts && opts.target) || document.body;
    const w = target.scrollWidth, h = target.scrollHeight;
    const html = target.outerHTML.replace(/"/g, "&quot;").replace(/#/g, "%23");
    // 拼接<style>抓取当前生效样式
    let styles = "";
    for (const sheet of document.styleSheets) {
      try { styles += Array.from(sheet.cssRules).map((r) => r.cssText).join("\n"); }
      catch (_) { /* CORS 跨域 stylesheet 跳过 */ }
    }
    const svg =
      '<svg xmlns="http://www.w3.org/2000/svg" width="' + w + '" height="' + h + '">' +
      '<foreignObject width="100%" height="100%">' +
      '<div xmlns="http://www.w3.org/1999/xhtml">' +
      '<style>' + styles + '</style>' + html +
      '</div></foreignObject></svg>';
    const blob = new Blob([svg], { type: "image/svg+xml;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const img = new Image();
    img.crossOrigin = "anonymous";
    await new Promise((res, rej) => {
      img.onload = res;
      img.onerror = (e) => rej(new Error("SVG → Image 失败"));
      img.src = url;
    });
    const canvas = document.createElement("canvas");
    canvas.width = w; canvas.height = h;
    canvas.getContext("2d").drawImage(img, 0, 0, w, h);
    URL.revokeObjectURL(url);
    const pngBlob = await new Promise((res) => canvas.toBlob(res, "image/png"));
    if (!pngBlob) throw new Error("canvas.toBlob 失败");
    if (window.hx && window.hx.saveFile) {
      const buf = new Uint8Array(await pngBlob.arrayBuffer());
      return await window.hx.saveFile("__screenshot__.png", buf, "image/png");
    }
    // 兜底：返回 dataURL（app 自己处理保存）
    return canvas.toDataURL("image/png");
  };

  // ─────────────────────────────────────────────────────────
  // 8. 无障碍基线 — focus 管理 + 语义提示
  // ─────────────────────────────────────────────────────────
  // 极简实现：注入键盘 focus-visible CSS + 监听 Tab 顺序，给调试入口。
  (function a11yBootstrap() {
    if (document.getElementById("__hx_a11y__")) return;
    const css = document.createElement("style");
    css.id = "__hx_a11y__";
    css.textContent =
      "[tabindex]:focus, button:focus, [role=button]:focus, input:focus, select:focus, textarea:focus, a:focus {" +
      "outline: 2px solid currentColor !important; outline-offset: 2px !important; }" +
      ":focus:not(:focus-visible) { outline: none !important; }";
    document.head ? document.head.appendChild(css) :
      document.addEventListener("DOMContentLoaded", () => document.head.appendChild(css));
  })();

  // ─────────────────────────────────────────────────────────
  // 9. 图片加载兜底 — 失败 / 加载中动态占位 (shimmer + 图标)
  //     全局捕获 <img> error（capture），在失败 img 外包裹一层，
  //     渲染 shimmer 动画 + 居中图标，避免破图 / 空白。
  //     不影响成功加载的图片；纯样式兜底，不替换业务 DOM 结构。
  // ─────────────────────────────────────────────────────────
  (function imageFallback() {
    const CSS_ID = "__hx_img_fallback__";
    if (document.getElementById(CSS_ID)) return;
    const style = document.createElement("style");
    style.id = CSS_ID;
    style.textContent = [
      "@keyframes hx-shimmer { 0% { background-position: 200% 0; } 100% { background-position: -200% 0; } }",
      ".hx-img-fallback { position: relative; display: inline-block; min-width: 48px; min-height: 48px;",
      "  background: linear-gradient(90deg,#eceef1 25%,#f6f7f9 37%,#eceef1 63%);",
      "  background-size: 400% 100%; animation: hx-shimmer 1.4s ease infinite;",
      "  border-radius: 8px; overflow: hidden; vertical-align: middle; }",
      ".hx-img-fallback > img { display: none; }",
      ".hx-img-fallback.is-loaded { background: none; animation: none; }",
      ".hx-img-fallback.is-loaded > img { display: inline-block; }",
      ".hx-img-fallback.is-loaded::after { content: none; }",
      ".hx-img-fallback::after { content: ''; position: absolute; inset: 0;",
      "  background: url('data:image/svg+xml;utf8,%3Csvg xmlns=%22http://www.w3.org/2000/svg%22 width=%2224%22 height=%2224%22 viewBox=%220 0 24 24%22 fill=%22none%22 stroke=%22%23999%22 stroke-width=%222%22 stroke-linecap=%22round%22 stroke-linejoin=%22round%22%3E%3Crect width=%2218%22 height=%2218%22 x=%223%22 y=%223%22 rx=%222%22/%3E%3Ccircle cx=%229%22 cy=%229%22 r=%222%22/%3E%3Cpath d=%22m21 15-3.1-3.1a2 2 0 0 0-2.8 0L6 21%22/%3E%3C/svg%3E') center / 32px no-repeat; }",
    ].join("");
    const mount = () => { if (document.head) document.head.appendChild(style); };
    if (document.head) mount();
    else document.addEventListener("DOMContentLoaded", mount);

    document.addEventListener("error", function (e) {
      const el = e.target;
      if (!el || el.tagName !== "IMG" || el.dataset.hxFallback) return;
      if (el.complete && el.naturalWidth > 0) return; // 已成功加载，忽略
      el.dataset.hxFallback = "1";
      const wrap = document.createElement("span");
      wrap.className = "hx-img-fallback";
      const w = el.getAttribute("width") || el.style.width;
      const h = el.getAttribute("height") || el.style.height;
      if (w) wrap.style.width = String(w);
      if (h) wrap.style.height = String(h);
      if (el.parentNode) el.parentNode.replaceChild(wrap, el);
      wrap.appendChild(el);
      // 暴露重置入口：图片后续加载成功时移除兜底
      el.addEventListener("load", function () {
        if (el.naturalWidth > 0) { wrap.classList.add("is-loaded"); }
      });
    }, true);
  })();

  // ─────────────────────────────────────────────────────────
  // 10. 媒体源兜底 — hxfile:// 经 blob 桥接喂 <audio>/<video>
  //     根因: 自定义 scheme (hxfile://) 不在 Chromium 媒体源白名单,
  //           直接 <audio src="hxfile://..."> 会被网络层拒绝 → code=4 Format error。
  //           壳统一在 runtime 注入层拦截 play(),把 hxfile:// src 转成
  //           blob: URL 再播放 (blob: 属媒体白名单)。
  //     路径: 优先 fetch(hxfile://) 字节直转 blob (最快,不占 JS 堆字符串);
  //           fetch 因 scheme 被网络层拦截或非 2xx → fallback window.hx.readFileBytes (IPC)。
  //     覆盖不改 app 代码(设 src 后立即 play / autoplay)的场景。
  // ─────────────────────────────────────────────────────────
  (function mediaSourceFallback() {
    const HXFILE = /^hxfile:\/\//i;
    const cache = new Map();

    function hxfileToLocalPath(url) {
      let p = url.replace(/^hxfile:\/\/localhost\//i, "").replace(/^hxfile:\/\/\//i, "");
      try { p = decodeURIComponent(p); } catch (_) {}
      return p;
    }
    function mimeFromPath(p) {
      const m = /\.([a-z0-9]+)$/i.exec(p || "");
      const ext = (m && m[1] || "").toLowerCase();
      const MAP = {
        mp3: "audio/mpeg", flac: "audio/flac", wav: "audio/wav", ogg: "audio/ogg",
        m4a: "audio/mp4", aac: "audio/aac", wma: "audio/x-ms-wma",
        mp4: "video/mp4", webm: "video/webm", mov: "video/quicktime", mkv: "video/x-matroska",
      };
      return MAP[ext] || "";
    }
    function base64ToUint8(base64) {
      const bin = atob(base64);
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      return bytes;
    }

    // url → (blob: URL)。非 hxfile 原样返回。结果缓存避免重复 fetch。
    async function mediaUrl(url) {
      if (!HXFILE.test(url)) return url;
      if (cache.has(url)) return cache.get(url);
      const p = (async () => {
        try {
          const r = await fetch(url, { cache: "no-store" });
          if (!r.ok) throw new Error("HTTP " + r.status);
          const blob = await r.blob();
          return URL.createObjectURL(blob);
        } catch (e) {
          // fetch 失败(scheme 被网络层拦截 / 非 2xx) → fallback IPC 读字节
          try {
            const localPath = hxfileToLocalPath(url);
            const res = await window.hx.readFileBytes(localPath);
            if (!res || !res.ok) throw new Error("readFileBytes: " + (res && res.err));
            const blob = new Blob([base64ToUint8(res.data)], {
              type: mimeFromPath(localPath) || "application/octet-stream",
            });
            return URL.createObjectURL(blob);
          } catch (e2) {
            console.error("[Hermix.media] 加载失败:", url, e2);
            throw e2;
          }
        }
      })();
      cache.set(url, p);
      p.catch(() => cache.delete(url)); // 失败不永久阻塞同 url
      return p;
    }
    Hermix.mediaUrl = mediaUrl;

    // 拦截 play(): src 是 hxfile:// 时,await blob 就绪再真正 play。
    const origPlay = HTMLMediaElement.prototype.play;
    HTMLMediaElement.prototype.play = function () {
      const src = this.src || this.currentSrc || "";
      if (HXFILE.test(src)) {
        return mediaUrl(src).then((blobUrl) => {
          this.src = blobUrl; // 设 blob: 后 currentSrc 变更,后续 play 不拦截
          return origPlay.call(this);
        }).catch((e) => {
          console.error("[Hermix.media] play fallback failed:", e);
          return Promise.reject(e);
        });
      }
      return origPlay.call(this);
    };

    // 显式预加载 helper: app 也可 Hermix.loadMedia(el, hxfileUrl) 主动触发
    Hermix.loadMedia = function (el, url) {
      return mediaUrl(url).then((blobUrl) => { el.src = blobUrl; return blobUrl; });
    };
  })();

  // ─────────────────────────────────────────────────────────
  // 11. 版本号 (spec §6.1 / §8.4 验收用)
  // ─────────────────────────────────────────────────────────
  Hermix.runtime = { version: "1.0.0", capabilities: [
    "store", "defineComponent", "icon", "route", "storage", "sqlite", "screenshot",
    "imgFallback", "mediaBlob",
  ] };

})(window.Hermix = window.Hermix || {});
